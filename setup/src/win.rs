//! Win32 half of the installer. The step list comes from [`crate::plan`].
//!
//! Service configuration goes through `CreateServiceW` / `ChangeServiceConfigW`.
//! `sc.exe` is not argv-based: a `binPath` with spaces is split before the
//! service manager sees it. The planned path is already quoted for CreateProcess.

use std::ffi::OsStr;
use std::fs;
use std::io::ErrorKind;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_ACCESS_DENIED, ERROR_SERVICE_ALREADY_RUNNING,
    ERROR_SERVICE_CANNOT_ACCEPT_CTRL, ERROR_SERVICE_DOES_NOT_EXIST,
    ERROR_SERVICE_MARKED_FOR_DELETE, ERROR_SERVICE_NOT_ACTIVE, TRUE,
};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_64KEY,
};
use windows_sys::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatus, StartServiceW,
    SC_ACTION, SC_ACTION_RESTART, SC_HANDLE, SC_MANAGER_ALL_ACCESS, SERVICE_ALL_ACCESS,
    SERVICE_AUTO_START, SERVICE_CONFIG_DELAYED_AUTO_START_INFO, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONFIG_FAILURE_ACTIONS_FLAG, SERVICE_CONTROL_STOP,
    SERVICE_DELAYED_AUTO_START_INFO, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL,
    SERVICE_FAILURE_ACTIONSW, SERVICE_FAILURE_ACTIONS_FLAG, SERVICE_QUERY_STATUS, SERVICE_STATUS,
    SERVICE_STOPPED, SERVICE_STOP_PENDING, SERVICE_WIN32_OWN_PROCESS,
};

use crate::plan::{
    self, Cli, NpcapExit, Paths, Step, CONFIG_FILE, FAILURE_RESET_SEC, FAILURE_RESTARTS,
    FAILURE_RESTART_MS, NPCAP_SERVICE, PREVIOUS_SERVICE_NAME, SERVICE_DESCRIPTION, SERVICE_DISPLAY,
    SERVICE_NAME,
};

pub fn execute(cli: &Cli) -> Result<i32, String> {
    if cli.unattended != Some(true) {
        if cli.uninstall {
            println!("Removing the etherquic service.");
        } else {
            println!("Installing the etherquic service.");
        }
    }
    let paths = paths_for(cli);
    let steps = if cli.uninstall {
        plan::uninstall_plan(&paths, cli.purge)?
    } else {
        let config_path = paths.program_data.join(CONFIG_FILE);
        let existing = fs::read_to_string(&config_path).ok();
        plan::install_plan(&paths, cli, existing.as_deref(), &machine_id())?
    };
    let mut session = Session::default();
    for step in &steps {
        apply(step, &mut session)?;
    }
    if session.reboot {
        println!(
            "Npcap requested a reboot (exit 3010). etherquic-setup did not reboot and did not start the service."
        );
        return Ok(3010);
    }
    if !cli.uninstall && !steps.iter().any(|step| matches!(step, Step::StartService)) {
        println!("etherquic service installed and not started.");
    }
    if cli.uninstall {
        println!("etherquic service removed.");
        if cli.purge {
            println!("etherquic configuration removed.");
        }
    }
    Ok(0)
}

fn paths_for(cli: &Cli) -> Paths {
    let install_dir = cli.install_dir.clone().unwrap_or_else(default_install_dir);
    let payload_dir = cli.payload_dir.clone().unwrap_or_else(default_payload_dir);
    let program_data = default_program_data();
    Paths {
        install_dir,
        payload_dir,
        program_data,
    }
}

fn default_install_dir() -> PathBuf {
    env_dir("ProgramFiles", r"C:\Program Files").join("etherquic")
}

fn default_program_data() -> PathBuf {
    env_dir("ProgramData", r"C:\ProgramData").join("etherquic")
}

fn default_payload_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn env_dir(key: &str, fallback: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

#[derive(Default)]
struct Session {
    scm: Option<Handle>,
    reboot: bool,
}

fn apply(step: &Step, session: &mut Session) -> Result<(), String> {
    match step {
        Step::RequireAdmin => {
            session.scm = Some(open_scm()?);
            Ok(())
        }
        Step::StopService => {
            let scm = scm(session)?;
            stop_named(scm, SERVICE_NAME)
        }
        Step::RetirePreviousService => {
            let scm = scm(session)?;
            retire_previous(scm)
        }
        Step::CopyFile { from, to } => copy_file(from, to),
        Step::WriteConfig { path, body } => write_atomic(path, body),
        Step::RunNpcap { path } => {
            let code = run_npcap(path)?;
            if plan::interpret_npcap_exit(code)? == NpcapExit::RebootRequired {
                session.reboot = true;
            }
            Ok(())
        }
        Step::EnsureService { bin_path } => {
            let scm = scm(session)?;
            ensure_service(scm, bin_path)
        }
        Step::SetDescription => {
            let scm = scm(session)?;
            set_description(scm)
        }
        Step::SetFailureActions => {
            let scm = scm(session)?;
            set_failure_actions(scm)
        }
        Step::SetDelayedAutoStart => {
            let scm = scm(session)?;
            set_delayed_auto(scm)
        }
        Step::GrantConfigAccess { path } => grant_config_access(path),
        Step::WriteAddress {
            path,
            mac,
            service,
            unattended,
        } => {
            write_atomic(path, &format!("{mac}\n"))?;
            println!("etherquic address {mac} service {service}");
            if *unattended {
                println!("unattended: a player connects with no confirmation");
            } else {
                println!("confirmation required: etherquic-host --approve");
            }
            Ok(())
        }
        Step::StartService => {
            if session.reboot {
                return Ok(());
            }
            let scm = scm(session)?;
            start_service(scm)
        }
        Step::DeleteService => {
            let scm = scm(session)?;
            delete_named(scm, SERVICE_NAME)
        }
        Step::RemoveDir { path } => remove_dir(path),
    }
}

fn scm(session: &Session) -> Result<SC_HANDLE, String> {
    session
        .scm
        .as_ref()
        .map(Handle::as_raw)
        .ok_or_else(|| "service manager is not open".into())
}

struct Handle(SC_HANDLE);

impl Handle {
    fn as_raw(&self) -> SC_HANDLE {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseServiceHandle(self.0);
            }
        }
    }
}

fn open_scm() -> Result<Handle, String> {
    let handle = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_ALL_ACCESS) };
    if handle.is_null() {
        let err = unsafe { GetLastError() };
        if err == ERROR_ACCESS_DENIED {
            return Err(
                "needs an elevated administrator (OpenSCManagerW returned access denied)".into(),
            );
        }
        return Err(format!(
            "needs an elevated administrator (OpenSCManagerW failed with {err})"
        ));
    }
    Ok(Handle(handle))
}

fn open_service(scm: SC_HANDLE, name: &str, access: u32) -> Result<Option<Handle>, String> {
    let wide_name = wide(name);
    let handle = unsafe { OpenServiceW(scm, wide_name.as_ptr(), access) };
    if !handle.is_null() {
        return Ok(Some(Handle(handle)));
    }
    let err = unsafe { GetLastError() };
    if err == ERROR_SERVICE_DOES_NOT_EXIST {
        return Ok(None);
    }
    Err(format!("OpenServiceW {name} failed ({err})"))
}

fn retire_previous(scm: SC_HANDLE) -> Result<(), String> {
    if PREVIOUS_SERVICE_NAME == SERVICE_NAME {
        return Ok(());
    }
    stop_named(scm, PREVIOUS_SERVICE_NAME)?;
    delete_named(scm, PREVIOUS_SERVICE_NAME)
}

fn stop_named(scm: SC_HANDLE, name: &str) -> Result<(), String> {
    let Some(service) = open_service(scm, name, SERVICE_ALL_ACCESS)? else {
        return Ok(());
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut status = zero_status();
        if unsafe { QueryServiceStatus(service.as_raw(), &mut status) } == 0 {
            return Err(format!("QueryServiceStatus failed ({})", unsafe {
                GetLastError()
            }));
        }
        if status.dwCurrentState == SERVICE_STOPPED {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("{name} did not stop within 20 seconds"));
        }
        if status.dwCurrentState != SERVICE_STOP_PENDING {
            let sent =
                unsafe { ControlService(service.as_raw(), SERVICE_CONTROL_STOP, &mut status) };
            if sent == 0 {
                let err = unsafe { GetLastError() };
                if err == ERROR_SERVICE_NOT_ACTIVE {
                    return Ok(());
                }
                if err != ERROR_SERVICE_CANNOT_ACCEPT_CTRL {
                    return Err(format!("stop {name} failed ({err})"));
                }
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
}

fn ensure_service(scm: SC_HANDLE, bin_path: &str) -> Result<(), String> {
    let name = wide(SERVICE_NAME);
    let display = wide(SERVICE_DISPLAY);
    let binary = wide(bin_path);
    let dependencies = if service_present(scm, NPCAP_SERVICE)? {
        multi_sz(&[NPCAP_SERVICE])
    } else {
        // An empty list clears dependencies. NULL on ChangeServiceConfigW would
        // leave a stale npcap dependency on an NDIS-only machine.
        multi_sz(&[])
    };
    let existing = open_service(scm, SERVICE_NAME, SERVICE_ALL_ACCESS)?;
    if let Some(service) = existing {
        let ok = unsafe {
            ChangeServiceConfigW(
                service.as_raw(),
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                binary.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                dependencies.as_ptr(),
                ptr::null(),
                ptr::null(),
                display.as_ptr(),
            )
        };
        if ok == 0 {
            return Err(format!("ChangeServiceConfigW failed ({})", unsafe {
                GetLastError()
            }));
        }
        return Ok(());
    }
    let created = unsafe {
        CreateServiceW(
            scm,
            name.as_ptr(),
            display.as_ptr(),
            SERVICE_ALL_ACCESS,
            SERVICE_WIN32_OWN_PROCESS,
            SERVICE_AUTO_START,
            SERVICE_ERROR_NORMAL,
            binary.as_ptr(),
            ptr::null(),
            ptr::null_mut(),
            dependencies.as_ptr(),
            ptr::null(),
            ptr::null(),
        )
    };
    if created.is_null() {
        return Err(format!("CreateServiceW failed ({})", unsafe {
            GetLastError()
        }));
    }
    unsafe {
        CloseServiceHandle(created);
    }
    Ok(())
}

fn service_present(scm: SC_HANDLE, name: &str) -> Result<bool, String> {
    Ok(open_service(scm, name, SERVICE_QUERY_STATUS)?.is_some())
}

fn set_description(scm: SC_HANDLE) -> Result<(), String> {
    let service = require_service(scm)?;
    let mut text = wide(SERVICE_DESCRIPTION);
    let info = SERVICE_DESCRIPTIONW {
        lpDescription: text.as_mut_ptr(),
    };
    config2(service.as_raw(), SERVICE_CONFIG_DESCRIPTION, &info)
}

fn set_failure_actions(scm: SC_HANDLE) -> Result<(), String> {
    let service = require_service(scm)?;
    let mut actions = [SC_ACTION {
        Type: SC_ACTION_RESTART,
        Delay: FAILURE_RESTART_MS,
    }; FAILURE_RESTARTS];
    let info = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: FAILURE_RESET_SEC,
        lpRebootMsg: ptr::null_mut(),
        lpCommand: ptr::null_mut(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    config2(service.as_raw(), SERVICE_CONFIG_FAILURE_ACTIONS, &info)?;
    // A clean SERVICE_STOPPED with a non-zero exit code is a failure only when
    // this flag is set. A requested stop reports exit code 0, so it does not restart.
    let flag = SERVICE_FAILURE_ACTIONS_FLAG {
        fFailureActionsOnNonCrashFailures: TRUE,
    };
    config2(service.as_raw(), SERVICE_CONFIG_FAILURE_ACTIONS_FLAG, &flag)
}

fn set_delayed_auto(scm: SC_HANDLE) -> Result<(), String> {
    let service = require_service(scm)?;
    let info = SERVICE_DELAYED_AUTO_START_INFO {
        fDelayedAutostart: TRUE,
    };
    config2(
        service.as_raw(),
        SERVICE_CONFIG_DELAYED_AUTO_START_INFO,
        &info,
    )
}

fn config2<T>(service: SC_HANDLE, level: u32, info: &T) -> Result<(), String> {
    let ok = unsafe { ChangeServiceConfig2W(service, level, info as *const T as *const _) };
    if ok == 0 {
        return Err(format!(
            "ChangeServiceConfig2W level {level} failed ({})",
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

fn require_service(scm: SC_HANDLE) -> Result<Handle, String> {
    open_service(scm, SERVICE_NAME, SERVICE_ALL_ACCESS)?
        .ok_or_else(|| format!("service {SERVICE_NAME} is not installed"))
}

fn start_service(scm: SC_HANDLE) -> Result<(), String> {
    let service = require_service(scm)?;
    let ok = unsafe { StartServiceW(service.as_raw(), 0, ptr::null()) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_SERVICE_ALREADY_RUNNING {
            return Ok(());
        }
        return Err(format!("StartServiceW failed ({err})"));
    }
    Ok(())
}

fn delete_named(scm: SC_HANDLE, name: &str) -> Result<(), String> {
    let Some(service) = open_service(scm, name, SERVICE_ALL_ACCESS)? else {
        return Ok(());
    };
    let ok = unsafe { DeleteService(service.as_raw()) };
    if ok == 0 {
        let err = unsafe { GetLastError() };
        if err == ERROR_SERVICE_MARKED_FOR_DELETE || err == ERROR_SERVICE_DOES_NOT_EXIST {
            return Ok(());
        }
        return Err(format!("DeleteService failed ({err})"));
    }
    Ok(())
}

fn run_npcap(path: &Path) -> Result<i32, String> {
    if !path.is_file() {
        return Err(format!("Npcap installer not found: {}", path.display()));
    }
    let status = Command::new(path)
        .arg("/S")
        .status()
        .map_err(|err| format!("failed to run Npcap installer {}: {err}", path.display()))?;
    status
        .code()
        .ok_or_else(|| "Npcap installer did not exit".into())
}

fn grant_config_access(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|err| format!("create {}: {err}", path.display()))?;
    // BUILTIN\Users (S-1-5-32-545) can create approve/deny. The SID avoids a
    // localized group name. host.conf is in the parent directory and is not granted.
    let status = Command::new("icacls")
        .arg(path)
        .arg("/grant")
        .arg("*S-1-5-32-545:(OI)(CI)M")
        .arg("/C")
        .arg("/Q")
        .status()
        .map_err(|err| format!("icacls {}: {err}", path.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "could not let users approve connections (icacls exited {})",
            status.code().unwrap_or(-1)
        ))
    }
}

fn copy_file(from: &Path, to: &Path) -> Result<(), String> {
    if !from.is_file() {
        return Err(format!(
            "missing payload file {} (pass --payload or place it next to etherquic-setup.exe)",
            from.display()
        ));
    }
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let mut last = None;
    for _ in 0..20 {
        match fs::copy(from, to) {
            Ok(_) => return Ok(()),
            Err(err) => {
                last = Some(err);
                thread::sleep(Duration::from_millis(250));
            }
        }
    }
    Err(format!(
        "copy {} to {}: {}",
        from.display(),
        to.display(),
        last.expect("copy failed")
    ))
}

fn write_atomic(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| format!("create {}: {err}", parent.display()))?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, body).map_err(|err| format!("write {}: {err}", tmp.display()))?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) => {
            // Windows refuses to rename over a file that already exists.
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(err) if err.kind() == ErrorKind::NotFound => {}
                Err(err) => {
                    let _ = fs::remove_file(&tmp);
                    return Err(format!("replace {}: {err}", path.display()));
                }
            }
            fs::rename(&tmp, path).map_err(|err| format!("replace {}: {err}", path.display()))
        }
    }
}

fn remove_dir(path: &Path) -> Result<(), String> {
    if !plan::dir_is_removable(path) {
        return Err(format!("refusing to remove {}", path.display()));
    }
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(format!("remove {}: {err}", path.display())),
    }
}

fn zero_status() -> SERVICE_STATUS {
    SERVICE_STATUS {
        dwServiceType: 0,
        dwCurrentState: 0,
        dwControlsAccepted: 0,
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    }
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn multi_sz(names: &[&str]) -> Vec<u16> {
    let mut encoded = Vec::new();
    for name in names {
        encoded.extend(OsStr::new(name).encode_wide());
        encoded.push(0);
    }
    encoded.push(0);
    if names.is_empty() {
        encoded.push(0);
    }
    encoded
}

fn machine_id() -> String {
    machine_guid()
        .unwrap_or_else(|| std::env::var("COMPUTERNAME").unwrap_or_else(|_| "etherquic".into()))
}

fn machine_guid() -> Option<String> {
    let subkey = wide(r"SOFTWARE\Microsoft\Cryptography");
    let mut key = ptr::null_mut();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            subkey.as_ptr(),
            0,
            KEY_READ | KEY_WOW64_64KEY,
            &mut key,
        )
    };
    if opened != 0 || key.is_null() {
        return None;
    }
    let _guard = HandleKey(key);
    let name = wide("MachineGuid");
    let mut buf = [0u16; 80];
    let mut size = (buf.len() * 2) as u32;
    let mut kind = 0u32;
    let read = unsafe {
        RegQueryValueExW(
            key,
            name.as_ptr(),
            ptr::null(),
            &mut kind,
            buf.as_mut_ptr() as *mut u8,
            &mut size,
        )
    };
    if read != 0 || kind != 1 {
        return None;
    }
    let chars = (size as usize / 2).min(buf.len());
    let end = buf[..chars]
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(chars);
    let text = String::from_utf16_lossy(&buf[..end]);
    let text = text.trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

struct HandleKey(windows_sys::Win32::System::Registry::HKEY);

impl Drop for HandleKey {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }
}

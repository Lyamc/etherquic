//! Named pipe between the LocalSystem service and the console-session helper.
//!
//! The service creates the pipe and launches this exe with `--desk` on
//! `winsta0\default`. The helper is started from the console session's
//! `winlogon.exe` token, which is SYSTEM on that desktop, so `SendInput`
//! reaches high-integrity windows such as Task Manager. A null DACL lets that
//! process connect. Npcap stays in the service; this module does not open an
//! adapter.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, GetLastError, DUPLICATE_SAME_ACCESS, FALSE, HANDLE,
    INVALID_HANDLE_VALUE, LUID, TRUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, DuplicateTokenEx, GetTokenInformation, InitializeSecurityDescriptor,
    IsWellKnownSid, LookupPrivilegeValueW, SecurityImpersonation, SetSecurityDescriptorDacl,
    SetTokenInformation, TokenPrimary, TokenSessionId, TokenUser, WinLocalSystemSid,
    LUID_AND_ATTRIBUTES, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_ALL_ACCESS, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE,
    TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSGetActiveConsoleSessionId, WTSQueryUserToken,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateProcessAsUserW, GetCurrentProcess, OpenProcess, OpenProcessToken,
    TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW, PROCESS_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, STARTF_USESHOWWINDOW, STARTUPINFOW,
};
use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_sys::Win32::UI::WindowsAndMessaging::{SetProcessDPIAware, SW_HIDE};

use crate::proto::{frame, Message, MAX_PAYLOAD};

const PIPE_NAME: &str = r"\\.\pipe\etherquic-desk";

const ERROR_BROKEN_PIPE: u32 = 109;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_IO_PENDING: u32 = 997;
const ERROR_NO_DATA: u32 = 232;
const ERROR_OPERATION_ABORTED: u32 = 995;
const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;
const ERROR_PIPE_CONNECTED: u32 = 535;
const ERROR_PIPE_NOT_CONNECTED: u32 = 233;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

/// Byte pipe. Handles are duplicated so the reader and the writer each own one.
pub struct Pipe {
    handle: isize,
}

impl Pipe {
    pub fn listen() -> Result<Self, String> {
        let mut sd: SECURITY_DESCRIPTOR = unsafe { std::mem::zeroed() };
        // NULL DACL: the console user may connect to the LocalSystem pipe.
        // An empty DACL would deny every caller, including the helper.
        let present = unsafe {
            InitializeSecurityDescriptor((&mut sd as *mut SECURITY_DESCRIPTOR).cast(), 1) != 0
                && SetSecurityDescriptorDacl(
                    (&mut sd as *mut SECURITY_DESCRIPTOR).cast(),
                    TRUE,
                    std::ptr::null_mut(),
                    FALSE,
                ) != 0
        };
        if !present {
            return Err(win_err("desk pipe security"));
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: &mut sd as *mut _ as *mut _,
            bInheritHandle: FALSE,
        };
        let name = wide(PIPE_NAME);
        let handle = unsafe {
            CreateNamedPipeW(
                name.as_ptr(),
                PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                1024 * 1024,
                1024 * 1024,
                0,
                &sa,
            )
        };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return Err(win_err("create desk pipe"));
        }
        Ok(Self {
            handle: handle as isize,
        })
    }

    pub fn connect_client(stop: &AtomicBool) -> Result<Self, String> {
        let name = wide(PIPE_NAME);
        let start = Instant::now();
        loop {
            if stop.load(Ordering::SeqCst) {
                return Err("stopped".into());
            }
            let handle = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
                return Ok(Self {
                    handle: handle as isize,
                });
            }
            if start.elapsed() > Duration::from_secs(30) {
                return Err(win_err("connect desk pipe"));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    pub fn accept(&self, stop: &AtomicBool) -> Result<(), String> {
        let event = create_event()?;
        let mut overlapped = blank_overlapped(event);
        let ok = unsafe { ConnectNamedPipe(self.raw(), &mut overlapped) };
        if ok != 0 {
            close(event);
            return Ok(());
        }
        let err = last_error();
        if err == ERROR_PIPE_CONNECTED {
            close(event);
            return Ok(());
        }
        if err != ERROR_IO_PENDING {
            close(event);
            return Err(win_err("connect desk pipe"));
        }
        let start = Instant::now();
        loop {
            if stop.load(Ordering::SeqCst) {
                let _ = unsafe { CancelIoEx(self.raw(), &overlapped) };
                reap(self.raw(), &overlapped, event);
                return Err("stopped".into());
            }
            if start.elapsed() > Duration::from_secs(20) {
                let _ = unsafe { CancelIoEx(self.raw(), &overlapped) };
                reap(self.raw(), &overlapped, event);
                return Err("desk helper did not connect".into());
            }
            let wait = unsafe { WaitForSingleObject(event, 200) };
            if wait == WAIT_OBJECT_0 {
                let mut transferred = 0u32;
                let got = unsafe {
                    GetOverlappedResult(self.raw(), &overlapped, &mut transferred, FALSE)
                };
                close(event);
                if got == 0 {
                    return Err(win_err("desk helper connect"));
                }
                return Ok(());
            }
            if wait != WAIT_TIMEOUT {
                close(event);
                return Err(win_err("wait for desk helper"));
            }
        }
    }

    pub fn duplicate(&self) -> Result<Self, String> {
        let mut out: HANDLE = std::ptr::null_mut();
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                self.raw(),
                GetCurrentProcess(),
                &mut out,
                0,
                FALSE,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 || out.is_null() {
            return Err(win_err("duplicate desk pipe"));
        }
        Ok(Self {
            handle: out as isize,
        })
    }

    /// `Ok(None)` is a clean end (the other side closed, or `stop` was set).
    pub fn read_message(&self, stop: &AtomicBool) -> Result<Option<Message>, String> {
        let mut header = [0u8; 5];
        if !self.read_exact(&mut header, stop)? {
            return Ok(None);
        }
        let kind = header[0];
        let len = u32::from_le_bytes([header[1], header[2], header[3], header[4]]) as usize;
        if len > MAX_PAYLOAD {
            return Err(format!("desk message is {len} bytes"));
        }
        let mut payload = vec![0u8; len];
        if !self.read_exact(&mut payload, stop)? {
            return Err("desk message truncated".into());
        }
        Message::decode(kind, &payload).map(Some)
    }

    pub fn write_message(&self, message: &Message, stop: &AtomicBool) -> Result<(), String> {
        let (kind, payload) = message.parts();
        let bytes = frame(kind, payload)?;
        self.write_all(&bytes, stop)
    }

    fn read_exact(&self, buf: &mut [u8], stop: &AtomicBool) -> Result<bool, String> {
        let mut filled = 0;
        while filled < buf.len() {
            match self.read_some(&mut buf[filled..], stop)? {
                0 if filled == 0 => return Ok(false),
                0 => return Err("desk message truncated".into()),
                n => filled += n,
            }
        }
        Ok(true)
    }

    fn raw(&self) -> HANDLE {
        self.handle as HANDLE
    }

    fn read_some(&self, buf: &mut [u8], stop: &AtomicBool) -> Result<usize, String> {
        read_some(self.raw(), buf, stop)
    }

    fn write_all(&self, bytes: &[u8], stop: &AtomicBool) -> Result<(), String> {
        let mut offset = 0;
        while offset < bytes.len() {
            let n = write_some(self.raw(), &bytes[offset..], stop)?;
            if n == 0 {
                return Err("desk pipe closed".into());
            }
            offset += n;
        }
        Ok(())
    }
}

impl Drop for Pipe {
    fn drop(&mut self) {
        close(self.handle as HANDLE);
        self.handle = 0;
    }
}

/// Helper process on the interactive desktop.
pub struct Child {
    process: isize,
    thread: isize,
    pub session: u32,
    /// Launch path that produced the running helper: `winlogon`, `system`, or `user`.
    pub via: &'static str,
    /// Earlier launch attempts that did not start, empty when the first path worked.
    pub note: String,
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.process != 0 {
            unsafe { TerminateProcess(self.process as HANDLE, 1) };
            close(self.process as HANDLE);
            close(self.thread as HANDLE);
            self.process = 0;
        }
    }
}

pub fn launch() -> Result<Child, String> {
    let _ = enable_privileges();
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == 0 || session == u32::MAX {
        return Err(format!("no interactive console (session {session})"));
    }
    // The logged-on token from WTSQueryUserToken is medium integrity under UAC.
    // SendInput from that process is dropped while Task Manager or another
    // high-integrity window is in front. Prefer a SYSTEM token already in
    // this session, then the service token moved into the session.
    let attempts: [(&str, fn(u32) -> Result<HANDLE, String>); 3] = [
        ("winlogon", winlogon_token),
        ("system", interactive_system_token),
        ("user", user_primary_token),
    ];
    let mut skipped = Vec::new();
    for (via, open) in attempts {
        let primary = match open(session) {
            Ok(token) => token,
            Err(error) => {
                skipped.push(format!("{via}: {error}"));
                continue;
            }
        };
        match start_desk(primary) {
            Ok((process, thread)) => {
                return Ok(Child {
                    process,
                    thread,
                    session,
                    via,
                    note: skipped.join("; "),
                });
            }
            Err(error) => skipped.push(format!("{via}: {error}")),
        }
    }
    Err(skipped.join("; "))
}

/// `system` when this process is LocalSystem, otherwise `user`.
pub fn account_label() -> &'static str {
    if process_is_local_system() {
        "system"
    } else {
        "user"
    }
}

fn winlogon_token(session: u32) -> Result<HANDLE, String> {
    let pid = session_pid(session, "winlogon.exe")?;
    primary_from_process(pid, "winlogon")
}

fn interactive_system_token(session: u32) -> Result<HANDLE, String> {
    enable_privileges()?;
    let mut current: HANDLE = std::ptr::null_mut();
    let opened = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut current,
        )
    };
    if opened == 0 {
        return Err(win_err("open system token"));
    }
    let mut primary: HANDLE = std::ptr::null_mut();
    let duplicated = unsafe {
        DuplicateTokenEx(
            current,
            TOKEN_ALL_ACCESS,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
    };
    close(current);
    if duplicated == 0 {
        return Err(win_err("duplicate system token"));
    }
    let id = session;
    let set = unsafe {
        SetTokenInformation(
            primary,
            TokenSessionId,
            std::ptr::addr_of!(id).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };
    if set == 0 {
        let error = win_err("session id");
        close(primary);
        return Err(error);
    }
    Ok(primary)
}

fn user_primary_token(session: u32) -> Result<HANDLE, String> {
    let mut user: HANDLE = std::ptr::null_mut();
    if unsafe { WTSQueryUserToken(session, &mut user) } == 0 {
        return Err(win_err("user token"));
    }
    let mut primary: HANDLE = std::ptr::null_mut();
    let duplicated = unsafe {
        DuplicateTokenEx(
            user,
            TOKEN_ALL_ACCESS,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
    };
    close(user);
    if duplicated == 0 {
        return Err(win_err("duplicate user token"));
    }
    Ok(primary)
}

fn start_desk(primary: HANDLE) -> Result<(isize, isize), String> {
    let exe = match exe_path() {
        Ok(exe) => exe,
        Err(error) => {
            close(primary);
            return Err(error);
        }
    };
    let mut command = wide(&format!("\"{exe}\" --desk"));
    let app = wide(&exe);
    let mut desktop = wide("winsta0\\default");
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    startup.lpDesktop = desktop.as_mut_ptr();
    startup.dwFlags = STARTF_USESHOWWINDOW;
    startup.wShowWindow = SW_HIDE as u16;
    let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let started = unsafe {
        CreateProcessAsUserW(
            primary,
            app.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            FALSE,
            CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut info,
        )
    };
    close(primary);
    if started == 0 {
        return Err(win_err("start desk helper"));
    }
    Ok((info.hProcess as isize, info.hThread as isize))
}

fn session_pid(session: u32, exe_name: &str) -> Result<u32, String> {
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snap.is_null() || snap == INVALID_HANDLE_VALUE {
        return Err(win_err("process snapshot"));
    }
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
    let mut found = 0u32;
    let mut saw = unsafe { Process32FirstW(snap, &mut entry) } != 0;
    while saw {
        let mut process_session = 0u32;
        if exe_file(&entry).eq_ignore_ascii_case(exe_name)
            && unsafe { ProcessIdToSessionId(entry.th32ProcessID, &mut process_session) } != 0
            && process_session == session
        {
            found = entry.th32ProcessID;
            break;
        }
        saw = unsafe { Process32NextW(snap, &mut entry) } != 0;
    }
    close(snap);
    if found == 0 {
        return Err(format!("no {exe_name}"));
    }
    Ok(found)
}

fn exe_file(entry: &PROCESSENTRY32W) -> String {
    let end = entry
        .szExeFile
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(entry.szExeFile.len());
    String::from_utf16_lossy(&entry.szExeFile[..end])
}

fn primary_from_process(pid: u32, what: &str) -> Result<HANDLE, String> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, pid) };
    if process.is_null() {
        return Err(win_err(&format!("open {what}")));
    }
    let mut token: HANDLE = std::ptr::null_mut();
    let opened = unsafe {
        OpenProcessToken(
            process,
            TOKEN_ASSIGN_PRIMARY | TOKEN_DUPLICATE | TOKEN_QUERY,
            &mut token,
        )
    };
    close(process);
    if opened == 0 {
        return Err(win_err(&format!("{what} token")));
    }
    let mut primary: HANDLE = std::ptr::null_mut();
    let duplicated = unsafe {
        DuplicateTokenEx(
            token,
            TOKEN_ALL_ACCESS,
            std::ptr::null(),
            SecurityImpersonation,
            TokenPrimary,
            &mut primary,
        )
    };
    close(token);
    if duplicated == 0 {
        return Err(win_err(&format!("duplicate {what} token")));
    }
    Ok(primary)
}

fn process_is_local_system() -> bool {
    let mut token: HANDLE = std::ptr::null_mut();
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) };
    if opened == 0 {
        return false;
    }
    let system = token_is_local_system(token);
    close(token);
    system
}

fn token_is_local_system(token: HANDLE) -> bool {
    let mut needed = 0u32;
    unsafe {
        GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
    }
    if needed == 0 {
        return false;
    }
    let mut buf = vec![0u8; needed as usize];
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            needed,
            &mut needed,
        )
    };
    if ok == 0 {
        return false;
    }
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let system = unsafe { IsWellKnownSid(user.User.Sid, WinLocalSystemSid) };
    system != 0
}

pub fn init_com() {
    unsafe {
        CoInitializeEx(std::ptr::null_mut(), COINIT_MULTITHREADED as u32);
    }
}

pub fn dpi_aware() {
    unsafe {
        SetProcessDPIAware();
    }
}

fn enable_privileges() -> Result<(), String> {
    let mut token: HANDLE = std::ptr::null_mut();
    let opened = unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    };
    if opened == 0 {
        return Err(win_err("open process token"));
    }
    let result = (|| {
        for name in [
            "SeAssignPrimaryTokenPrivilege",
            "SeIncreaseQuotaPrivilege",
            "SeTcbPrivilege",
        ] {
            enable_privilege(token, name)?;
        }
        Ok(())
    })();
    close(token);
    result
}

fn enable_privilege(token: HANDLE, name: &str) -> Result<(), String> {
    let mut luid = LUID {
        LowPart: 0,
        HighPart: 0,
    };
    let privilege = wide(name);
    if unsafe { LookupPrivilegeValueW(std::ptr::null(), privilege.as_ptr(), &mut luid) } == 0 {
        return Err(win_err(name));
    }
    let mut tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    let adjusted = unsafe {
        AdjustTokenPrivileges(
            token,
            FALSE,
            &mut tp,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    let code = last_error();
    if adjusted == 0 || code == ERROR_NOT_ALL_ASSIGNED {
        return Err(format!("{name}: win32 {code}"));
    }
    Ok(())
}

fn exe_path() -> Result<String, String> {
    let mut buf = [0u16; 32_768];
    let n = unsafe { GetModuleFileNameW(std::ptr::null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return Err(win_err("exe path"));
    }
    Ok(String::from_utf16_lossy(&buf[..n as usize]))
}

fn read_some(handle: HANDLE, buf: &mut [u8], stop: &AtomicBool) -> Result<usize, String> {
    let event = create_event()?;
    let mut overlapped = blank_overlapped(event);
    let mut transferred = 0u32;
    let ok = unsafe {
        ReadFile(
            handle,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
            &mut transferred,
            &mut overlapped,
        )
    };
    let n = finish_io(handle, ok, transferred, &mut overlapped, event, stop)?;
    Ok(n)
}

fn write_some(handle: HANDLE, buf: &[u8], stop: &AtomicBool) -> Result<usize, String> {
    let event = create_event()?;
    let mut overlapped = blank_overlapped(event);
    let mut transferred = 0u32;
    let ok = unsafe {
        WriteFile(
            handle,
            buf.as_ptr().cast(),
            buf.len() as u32,
            &mut transferred,
            &mut overlapped,
        )
    };
    finish_io(handle, ok, transferred, &mut overlapped, event, stop)
}

fn finish_io(
    handle: HANDLE,
    ok: i32,
    mut transferred: u32,
    overlapped: &mut OVERLAPPED,
    event: HANDLE,
    stop: &AtomicBool,
) -> Result<usize, String> {
    if ok != 0 {
        close(event);
        return Ok(transferred as usize);
    }
    let err = last_error();
    if err != ERROR_IO_PENDING {
        close(event);
        return end_or_err(err);
    }
    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = unsafe { CancelIoEx(handle, overlapped) };
            reap(handle, overlapped, event);
            return Ok(0);
        }
        let wait = unsafe { WaitForSingleObject(event, 200) };
        if wait == WAIT_TIMEOUT {
            continue;
        }
        if wait != WAIT_OBJECT_0 {
            close(event);
            return Err(win_err("desk pipe wait"));
        }
        let got = unsafe { GetOverlappedResult(handle, overlapped, &mut transferred, FALSE) };
        close(event);
        if got == 0 {
            return end_or_err(last_error());
        }
        return Ok(transferred as usize);
    }
}

fn end_or_err(err: u32) -> Result<usize, String> {
    if matches!(
        err,
        ERROR_BROKEN_PIPE
            | ERROR_INVALID_HANDLE
            | ERROR_NO_DATA
            | ERROR_OPERATION_ABORTED
            | ERROR_PIPE_NOT_CONNECTED
    ) {
        Ok(0)
    } else {
        Err(format!("desk pipe: win32 {err}"))
    }
}

fn reap(pipe: HANDLE, overlapped: &OVERLAPPED, event: HANDLE) {
    let mut transferred = 0u32;
    unsafe {
        GetOverlappedResult(pipe, overlapped, &mut transferred, TRUE);
    }
    close(event);
}

fn create_event() -> Result<HANDLE, String> {
    let event = unsafe { CreateEventW(std::ptr::null(), FALSE, FALSE, std::ptr::null()) };
    if event.is_null() {
        return Err(win_err("desk event"));
    }
    Ok(event)
}

fn blank_overlapped(event: HANDLE) -> OVERLAPPED {
    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    overlapped.hEvent = event;
    overlapped
}

fn close(handle: HANDLE) {
    if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(handle) };
    }
}

fn last_error() -> u32 {
    unsafe { GetLastError() }
}

fn win_err(what: &str) -> String {
    format!("{what}: win32 {}", last_error())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

//! What the installer will do, with no Windows calls.
//!
//! The service binary path is stored quoted. `sc.exe` splits on spaces before
//! the service manager ever sees the path; `CreateServiceW` still needs the
//! quotes because the service manager later parses the path as a command line.

#![forbid(unsafe_code)]

use std::path::{Component, Path, PathBuf};

pub const SERVICE_NAME: &str = "etherquic-service";
pub const SERVICE_DISPLAY: &str = "etherquic service";
/// SCM name from before this rename. Install and uninstall remove it so the
/// old service does not keep the adapter.
pub const PREVIOUS_SERVICE_NAME: &str = "etherquic";
pub const SERVICE_DESCRIPTION: &str =
    "Accepts an etherquic player. Unattended mode connects with no confirmation. The address is in ProgramData\\etherquic\\address.txt.";
pub const HOST_EXE: &str = "etherquic-host.exe";
pub const CONFIG_FILE: &str = "host.conf";
pub const ADDRESS_FILE: &str = "address.txt";
pub const DEFAULT_SERVICE: u16 = 4433;
pub const NPCAP_SERVICE: &str = "npcap";
/// Failure count resets after one day.
pub const FAILURE_RESET_SEC: u32 = 86_400;
/// SCM restarts the process. The host itself does not loop on a missing NIC.
pub const FAILURE_RESTART_MS: u32 = 5_000;
pub const FAILURE_RESTARTS: usize = 3;

pub const PAYLOAD_FILES: [&str; 3] = [HOST_EXE, "cert.pem", "key.pem"];
/// Copied only when the payload directory already has the file. PyroWave is MIT,
/// so the shared library and its notice are bundled. Upstream publishes no binary.
pub const PYROWAVE_FILES: [&str; 3] = [
    "libpyrowave-shared.dll",
    "libpyrowave-shared-1.dll",
    "LICENSE-pyrowave.txt",
];
/// Copied only when present. libopus (BSD) gives Opus audio. The MinGW C++
/// runtime (GCC runtime exception) and mcfgthread are what the MinGW-built
/// PyroWave library loads. The host probes each codec and announces only what works.
pub const CODEC_FILES: [&str; 7] = [
    "libopus-0.dll",
    "LICENSE-opus.txt",
    "libgcc_s_seh-1.dll",
    "libstdc++-6.dll",
    "libmcfgthread-2.dll",
    "LICENSE-gcc-runtime.txt",
    "LICENSE-mcfgthread.txt",
];
/// Copied only when present. The host runs cjxl.exe from its own directory to make
/// the JPEG XL thumbnail it announces in discovery. libjxl and its parts are BSD/MIT/zlib.
pub const THUMBNAIL_FILES: [&str; 11] = [
    "cjxl.exe",
    "LICENSE.brotli",
    "LICENSE.giflib",
    "LICENSE.highway",
    "LICENSE.libjpeg-turbo",
    "LICENSE.libjxl",
    "LICENSE.libpng",
    "LICENSE.libwebp",
    "LICENSE.sjpeg",
    "LICENSE.skcms",
    "LICENSE.zlib",
];

pub const USAGE: &str = "\
etherquic-setup [--attended] [--unattended|/S|/quiet] [--nic NAME] [--no-start]
                [--install-dir DIR] [--payload DIR] [--npcap SETUP.exe]
                [--uninstall] [--purge]

Installs the etherquic host service. The installer does not prompt and
does not reboot. With no mode flag, a player waits for confirmation
(etherquic-host --approve). --unattended, /S, and /quiet accept a player
with no confirmation. Run etherquic-host --client --peer MAC to connect
out instead of installing. Npcap is not downloaded or bundled.
";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cli {
    pub uninstall: bool,
    pub purge: bool,
    /// `None` keeps the value in host.conf. Fresh install requires confirmation.
    pub unattended: Option<bool>,
    pub no_start: bool,
    pub install_dir: Option<PathBuf>,
    pub payload_dir: Option<PathBuf>,
    pub npcap: Option<PathBuf>,
    pub nic: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub install_dir: PathBuf,
    pub payload_dir: PathBuf,
    pub program_data: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostConfig {
    pub mac: [u8; 6],
    pub nic: String,
    pub service: u16,
    /// When false, a player waits until `etherquic-host --approve`.
    pub unattended: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    RequireAdmin,
    /// Stop and delete [`PREVIOUS_SERVICE_NAME`] when it is still installed.
    RetirePreviousService,
    StopService,
    CopyFile {
        from: PathBuf,
        to: PathBuf,
    },
    WriteConfig {
        path: PathBuf,
        body: String,
    },
    RunNpcap {
        path: PathBuf,
    },
    EnsureService {
        bin_path: String,
    },
    SetDescription,
    SetFailureActions,
    SetDelayedAutoStart,
    GrantConfigAccess {
        path: PathBuf,
    },
    WriteAddress {
        path: PathBuf,
        mac: String,
        service: u16,
        unattended: bool,
    },
    StartService,
    DeleteService,
    RemoveDir {
        path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NpcapExit {
    Ready,
    RebootRequired,
}

pub fn parse_args<I, S>(args: I) -> Result<Cli, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    let mut cli = Cli {
        uninstall: false,
        purge: false,
        unattended: None,
        no_start: false,
        install_dir: None,
        payload_dir: None,
        npcap: None,
        nic: None,
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if is_unattended(arg) {
            if cli.unattended == Some(false) {
                return Err("--unattended cannot be combined with --attended".into());
            }
            cli.unattended = Some(true);
        } else if arg == "--attended" {
            if cli.unattended == Some(true) {
                return Err("--attended cannot be combined with --unattended".into());
            }
            cli.unattended = Some(false);
        } else if arg == "--uninstall" {
            cli.uninstall = true;
        } else if arg == "--purge" {
            cli.purge = true;
        } else if arg == "--no-start" {
            cli.no_start = true;
        } else if let Some(value) = takes_value(arg, &args, &mut i, "--install-dir")? {
            cli.install_dir = Some(PathBuf::from(value));
        } else if let Some(value) = takes_value(arg, &args, &mut i, "--payload")? {
            cli.payload_dir = Some(PathBuf::from(value));
        } else if let Some(value) = takes_value(arg, &args, &mut i, "--npcap")? {
            cli.npcap = Some(PathBuf::from(value));
        } else if let Some(value) = takes_value(arg, &args, &mut i, "--nic")? {
            if value.is_empty() {
                return Err("--nic needs an adapter name".into());
            }
            cli.nic = Some(value);
        } else {
            return Err(format!("unknown argument {arg}"));
        }
        i += 1;
    }
    if cli.purge && !cli.uninstall {
        return Err("--purge is only valid with --uninstall".into());
    }
    if cli.uninstall && cli.npcap.is_some() {
        return Err("--npcap cannot be combined with --uninstall".into());
    }
    if cli.uninstall && cli.nic.is_some() {
        return Err("--nic cannot be combined with --uninstall".into());
    }
    Ok(cli)
}

fn is_unattended(arg: &str) -> bool {
    matches!(
        arg.to_ascii_lowercase().as_str(),
        "--unattended" | "/s" | "/quiet" | "--quiet"
    )
}

/// `Some` when `arg` is this flag. `None` when it is a different flag.
/// The index is left on the flag; the caller advances past it.
fn takes_value(
    arg: &str,
    args: &[String],
    index: &mut usize,
    flag: &str,
) -> Result<Option<String>, String> {
    let equals = format!("{flag}=");
    if let Some(value) = arg.strip_prefix(&equals) {
        if value.is_empty() {
            return Err(format!("{flag} needs a value"));
        }
        return Ok(Some(value.to_string()));
    }
    if arg != flag {
        return Ok(None);
    }
    let next = args
        .get(*index + 1)
        .ok_or_else(|| format!("{flag} needs a value"))?;
    if next.starts_with('-') || next.starts_with('/') {
        return Err(format!("{flag} needs a value"));
    }
    *index += 1;
    Ok(Some(next.clone()))
}

pub fn install_plan(
    paths: &Paths,
    cli: &Cli,
    existing: Option<&str>,
    machine_id: &str,
) -> Result<Vec<Step>, String> {
    check_dir(&paths.install_dir, "install directory")?;
    check_dir(&paths.program_data, "configuration directory")?;
    let cfg = merge_config(existing, cli.nic.as_deref(), cli.unattended, machine_id)?;
    let bin = quote_service_bin(&paths.install_dir.join(HOST_EXE))?;
    let mut steps = vec![
        Step::RequireAdmin,
        Step::RetirePreviousService,
        Step::StopService,
    ];
    for name in PAYLOAD_FILES {
        steps.push(Step::CopyFile {
            from: paths.payload_dir.join(name),
            to: paths.install_dir.join(name),
        });
    }
    for name in PYROWAVE_FILES.iter().chain(CODEC_FILES.iter()).chain(THUMBNAIL_FILES.iter()) {
        let from = paths.payload_dir.join(name);
        if from.is_file() {
            steps.push(Step::CopyFile {
                from,
                to: paths.install_dir.join(name),
            });
        }
    }
    steps.push(Step::WriteConfig {
        path: paths.program_data.join(CONFIG_FILE),
        body: render_config(&cfg),
    });
    // Users can drop an approve file here. host.conf stays in the parent directory.
    steps.push(Step::GrantConfigAccess {
        path: paths.program_data.join("control"),
    });
    if let Some(path) = &cli.npcap {
        steps.push(Step::RunNpcap { path: path.clone() });
    }
    steps.push(Step::EnsureService { bin_path: bin });
    steps.push(Step::SetDescription);
    steps.push(Step::SetFailureActions);
    steps.push(Step::SetDelayedAutoStart);
    steps.push(Step::WriteAddress {
        path: paths.program_data.join(ADDRESS_FILE),
        mac: format_mac(cfg.mac),
        service: cfg.service,
        unattended: cfg.unattended,
    });
    if !cli.no_start {
        steps.push(Step::StartService);
    }
    Ok(steps)
}

pub fn uninstall_plan(paths: &Paths, purge: bool) -> Result<Vec<Step>, String> {
    check_dir(&paths.install_dir, "install directory")?;
    if purge {
        check_dir(&paths.program_data, "configuration directory")?;
    }
    let mut steps = vec![
        Step::RequireAdmin,
        Step::RetirePreviousService,
        Step::StopService,
        Step::DeleteService,
        Step::RemoveDir {
            path: paths.install_dir.clone(),
        },
    ];
    if purge {
        steps.push(Step::RemoveDir {
            path: paths.program_data.clone(),
        });
    }
    Ok(steps)
}

fn check_dir(path: &Path, label: &str) -> Result<(), String> {
    if dir_is_removable(path) {
        Ok(())
    } else {
        Err(format!(
            "{label} {} is too broad to install into or remove",
            path.display()
        ))
    }
}

/// Refuse drive roots and well-known Windows directories. A child such as
/// `Program Files\etherquic` is allowed.
pub fn dir_is_removable(path: &Path) -> bool {
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => names.push(name.to_string_lossy().to_ascii_lowercase()),
            Component::ParentDir => return false,
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
        }
    }
    let Some(last) = names.last() else {
        return false;
    };
    !matches!(
        last.as_str(),
        "windows"
            | "system32"
            | "syswow64"
            | "program files"
            | "program files (x86)"
            | "programdata"
            | "users"
    )
}

/// Quote a path for the service manager's command-line parser.
pub fn quote_service_bin(path: &Path) -> Result<String, String> {
    let shown = path.display().to_string();
    if shown.contains('"') {
        return Err(format!("service path contains a quote: {shown}"));
    }
    Ok(format!("\"{shown}\""))
}

pub fn interpret_npcap_exit(code: i32) -> Result<NpcapExit, String> {
    match code {
        0 => Ok(NpcapExit::Ready),
        3010 => Ok(NpcapExit::RebootRequired),
        other => Err(format!("Npcap installer exited {other}")),
    }
}

/// Keep a valid MAC across reinstalls. `--nic` replaces only the adapter name.
pub fn merge_config(
    existing: Option<&str>,
    nic_override: Option<&str>,
    unattended: Option<bool>,
    machine_id: &str,
) -> Result<HostConfig, String> {
    let partial = match existing {
        Some(text) if !text.trim().is_empty() => parse_partial(text)?,
        _ => Partial::default(),
    };
    if let Some(nic) = nic_override {
        if nic.contains(['\n', '\r']) {
            return Err("--nic contains a newline".into());
        }
    }
    let mac = match partial.mac {
        Some(mac) => mac,
        None => mac_from_machine_id(machine_id),
    };
    let nic = match nic_override {
        Some(nic) => nic.to_string(),
        None => partial.nic.unwrap_or_default(),
    };
    let unattended = match unattended {
        Some(value) => value,
        None => partial.unattended.unwrap_or(false),
    };
    Ok(HostConfig {
        mac,
        nic,
        service: partial.service.unwrap_or(DEFAULT_SERVICE),
        unattended,
    })
}

pub fn render_config(cfg: &HostConfig) -> String {
    format!(
        "mac={}\nnic={}\nservice={}\nunattended={}\n",
        format_mac(cfg.mac),
        cfg.nic,
        cfg.service,
        if cfg.unattended { "yes" } else { "no" }
    )
}

pub fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// Locally administered unicast MAC. Stable for one machine id.
/// The same function lives in the host crate; the test vector is shared.
pub fn mac_from_machine_id(id: &str) -> [u8; 6] {
    let id = normalize_machine_id(id);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let bytes = hash.to_le_bytes();
    let mut mac = [0u8; 6];
    mac.copy_from_slice(&bytes[..6]);
    mac[0] = (mac[0] & 0xfc) | 0x02;
    mac
}

pub fn normalize_machine_id(id: &str) -> String {
    id.trim()
        .trim_matches(['{', '}'])
        .trim()
        .to_ascii_lowercase()
}

#[derive(Default)]
struct Partial {
    /// `None` means generate one. A parsed MAC is kept, including a hand-set one.
    mac: Option<[u8; 6]>,
    nic: Option<String>,
    service: Option<u16>,
    unattended: Option<bool>,
}

fn parse_partial(text: &str) -> Result<Partial, String> {
    let mut partial = Partial::default();
    for (line_no, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "host.conf line {}: expected key=value",
                line_no + 1
            ));
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        match key.as_str() {
            "mac" => {
                partial.mac =
                    if value.is_empty() || value.eq_ignore_ascii_case("auto") {
                        None
                    } else {
                        Some(parse_mac(value).ok_or_else(|| {
                            format!("host.conf line {}: invalid mac", line_no + 1)
                        })?)
                    };
            }
            "nic" => {
                if value.contains(['\n', '\r']) {
                    return Err(format!(
                        "host.conf line {}: nic contains a newline",
                        line_no + 1
                    ));
                }
                partial.nic = Some(value.to_string());
            }
            "service" => {
                partial.service = Some(value.parse::<u16>().map_err(|_| {
                    format!("host.conf line {}: service is not a number", line_no + 1)
                })?);
            }
            "unattended" => {
                partial.unattended = Some(parse_yes_no(value).ok_or_else(|| {
                    format!(
                        "host.conf line {}: unattended is not yes or no",
                        line_no + 1
                    )
                })?);
            }
            _ => {}
        }
    }
    Ok(partial)
}

fn parse_yes_no(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" | "on" => Some(true),
        "no" | "false" | "0" | "off" => Some(false),
        _ => None,
    }
}

fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = text.split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (index, part) in parts.iter().enumerate() {
        if part.len() != 2 {
            return None;
        }
        mac[index] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MACHINE: &str = "00112233-4455-6677-8899-aabbccddeeff";
    const GENERATED: &str = "be:4e:c5:33:fb:a0";

    fn paths() -> Paths {
        Paths {
            install_dir: PathBuf::from(r"C:\Program Files\etherquic"),
            payload_dir: PathBuf::from(r"D:\payload"),
            program_data: PathBuf::from(r"C:\ProgramData\etherquic"),
        }
    }

    fn cli() -> Cli {
        Cli {
            uninstall: false,
            purge: false,
            unattended: None,
            no_start: false,
            install_dir: None,
            payload_dir: None,
            npcap: None,
            nic: None,
        }
    }

    fn tags(steps: &[Step]) -> Vec<&'static str> {
        steps
            .iter()
            .map(|step| match step {
                Step::RequireAdmin => "admin",
                Step::RetirePreviousService => "retire",
                Step::StopService => "stop",
                Step::CopyFile { .. } => "copy",
                Step::WriteConfig { .. } => "config",
                Step::GrantConfigAccess { .. } => "grant",
                Step::RunNpcap { .. } => "npcap",
                Step::EnsureService { .. } => "service",
                Step::SetDescription => "description",
                Step::SetFailureActions => "failure",
                Step::SetDelayedAutoStart => "delayed",
                Step::WriteAddress { .. } => "address",
                Step::StartService => "start",
                Step::DeleteService => "delete",
                Step::RemoveDir { .. } => "remove",
            })
            .collect()
    }

    #[test]
    fn unattended_flags_parse() {
        for flag in ["--unattended", "/S", "/s", "/quiet", "--quiet"] {
            let cli = parse_args([flag]).unwrap();
            assert_eq!(cli.unattended, Some(true), "{flag}");
        }
    }

    #[test]
    fn valued_flags_parse() {
        let cli = parse_args([
            "--uninstall",
            "--purge",
            "--no-start",
            "--install-dir",
            r"D:\etherquic",
            r"--payload=E:\drop",
            "--nic",
            "Killer",
        ])
        .unwrap_err();
        assert!(cli.contains("--nic"), "{cli}");

        let cli = parse_args([
            "--install-dir",
            r"D:\etherquic",
            r"--payload=E:\drop",
            "--npcap",
            r"E:\npcap-setup.exe",
            "--nic",
            "Killer",
            "--no-start",
            "/quiet",
        ])
        .unwrap();
        assert_eq!(cli.install_dir.unwrap(), PathBuf::from(r"D:\etherquic"));
        assert_eq!(cli.payload_dir.unwrap(), PathBuf::from(r"E:\drop"));
        assert_eq!(cli.npcap.unwrap(), PathBuf::from(r"E:\npcap-setup.exe"));
        assert_eq!(cli.nic.as_deref(), Some("Killer"));
        assert!(cli.no_start);
        assert_eq!(cli.unattended, Some(true));
        assert!(!cli.uninstall);
    }

    #[test]
    fn rejects_unknown_and_misplaced_flags() {
        assert!(parse_args(["--reboot"]).is_err());
        assert!(parse_args(["--purge"]).unwrap_err().contains("--purge"));
        assert!(parse_args(["--uninstall", "--npcap", "a.exe"])
            .unwrap_err()
            .contains("--npcap"));
        assert!(parse_args(["--attended", "--unattended"]).is_err());
        assert!(parse_args(["--nic"]).is_err());
        assert!(parse_args(["--nic", ""]).is_err());
    }

    #[test]
    fn mac_is_stable_locally_administered_unicast() {
        let mac = mac_from_machine_id(MACHINE);
        assert_eq!(format_mac(mac), GENERATED);
        assert_eq!(mac_from_machine_id(&format!("{{{MACHINE}}}")), mac);
        assert_eq!(mac_from_machine_id(&MACHINE.to_ascii_uppercase()), mac);
        assert_eq!(format_mac(mac_from_machine_id("")), "26:23:22:84:e4:9c");
        assert_eq!(mac[0] & 0x01, 0, "unicast");
        assert_eq!(mac[0] & 0x02, 0x02, "locally administered");
        assert_ne!(mac_from_machine_id("a"), mac_from_machine_id("b"));
    }

    #[test]
    fn merge_keeps_existing_mac_and_replaces_nic_only_when_asked() {
        let existing = "\
# kept across reinstall
mac=02:11:22:33:44:55
nic=old
service=5000
";
        let kept = merge_config(Some(existing), None, None, MACHINE).unwrap();
        assert_eq!(format_mac(kept.mac), "02:11:22:33:44:55");
        assert_eq!(kept.nic, "old");
        assert_eq!(kept.service, 5000);
        assert_ne!(format_mac(kept.mac), GENERATED);

        let replaced = merge_config(Some(existing), Some("Killer"), None, MACHINE).unwrap();
        assert_eq!(format_mac(replaced.mac), "02:11:22:33:44:55");
        assert_eq!(replaced.nic, "Killer");
        assert_eq!(replaced.service, 5000);
    }

    #[test]
    fn missing_or_auto_mac_is_generated() {
        for existing in [None, Some("mac=auto\n"), Some("mac=\n"), Some("")] {
            let cfg = merge_config(existing, None, None, MACHINE).unwrap();
            assert_eq!(format_mac(cfg.mac), GENERATED);
            assert_eq!(cfg.nic, "");
            assert_eq!(cfg.service, DEFAULT_SERVICE);
        }
        let body =
            render_config(&merge_config(Some("mac=auto\nnic=old\n"), None, None, MACHINE).unwrap());
        assert_eq!(
            body,
            format!("mac={GENERATED}\nnic=old\nservice=4433\nunattended=no\n")
        );
    }

    #[test]
    fn invalid_mac_or_service_is_an_error() {
        assert!(merge_config(Some("mac=nope\n"), None, None, MACHINE).is_err());
        assert!(merge_config(Some("service=nope\n"), None, None, MACHINE).is_err());
        assert!(merge_config(Some("unattended=maybe\n"), None, None, MACHINE).is_err());
    }

    #[test]
    fn unattended_flag_accepts_without_confirmation_and_keeps_the_mac() {
        let existing = "mac=02:11:22:33:44:55\nnic=old\nservice=4433\nunattended=no\n";
        let kept = merge_config(Some(existing), None, None, MACHINE).unwrap();
        assert!(!kept.unattended);
        assert_eq!(format_mac(kept.mac), "02:11:22:33:44:55");

        let opened = merge_config(Some(existing), None, Some(true), MACHINE).unwrap();
        assert!(opened.unattended);
        assert_eq!(format_mac(opened.mac), "02:11:22:33:44:55");

        let mut cli = cli();
        cli.unattended = Some(true);
        let steps = install_plan(&paths(), &cli, Some(existing), MACHINE).unwrap();
        match steps
            .iter()
            .find(|step| matches!(step, Step::WriteConfig { .. }))
        {
            Some(Step::WriteConfig { body, .. }) => {
                assert!(body.contains("mac=02:11:22:33:44:55"), "{body}");
                assert!(body.contains("unattended=yes"), "{body}");
            }
            other => panic!("expected config, got {other:?}"),
        }
        assert!(steps.iter().any(|step| matches!(
            step,
            Step::WriteAddress {
                unattended: true,
                ..
            }
        )));
        assert!(steps.iter().any(|step| matches!(
            step,
            Step::GrantConfigAccess { path } if path.ends_with("control")
        )));
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "Windows path semantics")]
    fn install_steps_copy_configure_and_start() {
        let mut cli = cli();
        cli.npcap = Some(PathBuf::from(r"E:\npcap-setup.exe"));
        let steps = install_plan(&paths(), &cli, None, MACHINE).unwrap();
        assert_eq!(
            tags(&steps),
            [
                "admin",
                "retire",
                "stop",
                "copy",
                "copy",
                "copy",
                "config",
                "grant",
                "npcap",
                "service",
                "description",
                "failure",
                "delayed",
                "address",
                "start",
            ]
        );
        match &steps[6] {
            Step::WriteConfig { body, .. } => {
                assert!(body.contains(GENERATED), "{body}");
                assert!(body.contains("unattended=no"), "{body}");
            }
            other => panic!("expected config, got {other:?}"),
        }
        match &steps[9] {
            Step::EnsureService { bin_path } => {
                assert_eq!(
                    bin_path,
                    r#""C:\Program Files\etherquic\etherquic-host.exe""#
                );
            }
            other => panic!("expected service, got {other:?}"),
        }
        match steps.last().unwrap() {
            Step::StartService => {}
            other => panic!("expected start, got {other:?}"),
        }
        assert!(!steps
            .iter()
            .any(|step| matches!(step, Step::DeleteService | Step::RemoveDir { .. })));
    }

    #[test]
    fn no_start_omits_start_and_still_publishes_the_address() {
        let mut cli = cli();
        cli.no_start = true;
        let steps = install_plan(&paths(), &cli, None, MACHINE).unwrap();
        assert!(!steps.iter().any(|step| matches!(step, Step::StartService)));
        assert!(steps.iter().any(|step| matches!(
            step,
            Step::WriteAddress {
                mac,
                service,
                unattended: false,
                ..
            } if mac == GENERATED && *service == 4433
        )));
    }

    #[test]
    fn uninstall_stops_and_deletes_without_starting() {
        let steps = uninstall_plan(&paths(), false).unwrap();
        assert_eq!(
            tags(&steps),
            ["admin", "retire", "stop", "delete", "remove"]
        );
        assert!(!steps.iter().any(|step| {
            matches!(
                step,
                Step::StartService
                    | Step::CopyFile { .. }
                    | Step::WriteConfig { .. }
                    | Step::RunNpcap { .. }
                    | Step::EnsureService { .. }
            )
        }));
        let purged = uninstall_plan(&paths(), true).unwrap();
        assert_eq!(
            tags(&purged),
            ["admin", "retire", "stop", "delete", "remove", "remove"]
        );
        match purged.last().unwrap() {
            Step::RemoveDir { path } => {
                assert_eq!(path, &PathBuf::from(r"C:\ProgramData\etherquic"));
            }
            other => panic!("expected program data removal, got {other:?}"),
        }
    }

    #[test]
    #[cfg_attr(not(windows), ignore = "Windows path semantics")]
    fn refuses_to_remove_broad_directories() {
        assert!(!dir_is_removable(Path::new(r"C:\")));
        assert!(!dir_is_removable(Path::new(r"C:\Windows")));
        assert!(!dir_is_removable(Path::new(r"C:\Program Files")));
        assert!(!dir_is_removable(Path::new(r"C:\ProgramData")));
        assert!(!dir_is_removable(Path::new(r"C:\etherquic\..")));
        assert!(dir_is_removable(Path::new(r"C:\Program Files\etherquic")));
        assert!(dir_is_removable(Path::new(r"C:\ProgramData\etherquic")));
        let broad = Paths {
            install_dir: PathBuf::from(r"C:\Windows"),
            payload_dir: PathBuf::from(r"D:\payload"),
            program_data: PathBuf::from(r"C:\ProgramData\etherquic"),
        };
        assert!(install_plan(&broad, &cli(), None, MACHINE).is_err());
        assert!(uninstall_plan(&broad, false).is_err());
    }

    #[test]
    fn npcap_exit_3010_means_reboot_without_rebooting_here() {
        assert_eq!(interpret_npcap_exit(0).unwrap(), NpcapExit::Ready);
        assert_eq!(
            interpret_npcap_exit(3010).unwrap(),
            NpcapExit::RebootRequired
        );
        assert!(interpret_npcap_exit(1).is_err());
        assert_eq!(FAILURE_RESET_SEC, 86_400);
        assert_eq!(FAILURE_RESTART_MS, 5_000);
        assert_eq!(FAILURE_RESTARTS, 3);
    }

    #[test]
    fn pyrowave_files_are_copied_only_when_the_payload_has_them() {
        let root = std::env::temp_dir().join(format!("etherquic-pyrowave-{}", std::process::id()));
        let payload = root.join("payload");
        let install = root.join("install");
        let data = root.join("data");
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::create_dir_all(&install).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(payload.join("libpyrowave-shared.dll"), b"dll").unwrap();
        std::fs::write(payload.join("LICENSE-pyrowave.txt"), b"mit").unwrap();
        std::fs::write(payload.join("libopus-0.dll"), b"dll").unwrap();
        let staged = Paths {
            install_dir: install,
            payload_dir: payload,
            program_data: data,
        };
        let steps = install_plan(&staged, &cli(), None, MACHINE).unwrap();
        let names: Vec<String> = steps
            .iter()
            .filter_map(|step| match step {
                Step::CopyFile { to, .. } => {
                    to.file_name().map(|name| name.to_string_lossy().into_owned())
                }
                _ => None,
            })
            .collect();
        assert!(names.iter().any(|name| name == "libpyrowave-shared.dll"));
        assert!(names.iter().any(|name| name == "LICENSE-pyrowave.txt"));
        assert!(!names.iter().any(|name| name == "libpyrowave-shared-1.dll"));
        assert!(names.iter().any(|name| name == "libopus-0.dll"));
        assert!(!names.iter().any(|name| name == "libstdc++-6.dll"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn service_bin_path_is_quoted() {
        let quoted =
            quote_service_bin(Path::new(r"C:\Program Files\etherquic\etherquic-host.exe")).unwrap();
        assert_eq!(quoted, r#""C:\Program Files\etherquic\etherquic-host.exe""#);
        assert!(quote_service_bin(Path::new(r#"C:\a"b"#)).is_err());
    }

    #[test]
    fn cjxl_is_copied_only_when_the_payload_has_it() {
        let payload = std::env::temp_dir().join(format!("eq-setup-cjxl-{}", std::process::id()));
        std::fs::create_dir_all(&payload).unwrap();
        let mut p = paths();
        p.payload_dir = payload.clone();
        let copies = |steps: &[Step]| {
            steps
                .iter()
                .filter_map(|s| match s {
                    Step::CopyFile { to, .. } => to.file_name().map(|n| n.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        let without = install_plan(&p, &cli(), None, MACHINE).unwrap();
        assert!(!copies(&without).contains(&"cjxl.exe".to_string()));
        std::fs::write(payload.join("cjxl.exe"), b"x").unwrap();
        std::fs::write(payload.join("LICENSE.libjxl"), b"x").unwrap();
        let with = install_plan(&p, &cli(), None, MACHINE).unwrap();
        let names = copies(&with);
        assert!(names.contains(&"cjxl.exe".to_string()));
        assert!(names.contains(&"LICENSE.libjxl".to_string()));
        assert!(!names.contains(&"LICENSE.zlib".to_string()));
        std::fs::remove_dir_all(&payload).unwrap();
    }
}
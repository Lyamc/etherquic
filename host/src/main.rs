//! Headless etherquic host, or a client that connects to one.
//!
//! The service control manager starts this exe with no arguments. `--client`
//! connects out. `--console` runs the host loop in the foreground.

mod cli;
mod config;
mod confirm;
mod extras;
mod identity;
mod media;
mod proto;
mod run;

#[cfg(windows)]
mod bridge;
#[cfg(windows)]
mod desk;
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod link;

#[cfg(windows)]
mod service;

mod macos_service;

#[cfg(windows)]
mod console_ctrl {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows_sys::Win32::Foundation::{BOOL, TRUE};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    static STOP: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn on_ctrl(_ctrl: u32) -> BOOL {
        STOP.store(true, Ordering::SeqCst);
        TRUE
    }

    pub fn install() -> &'static AtomicBool {
        unsafe {
            SetConsoleCtrlHandler(Some(on_ctrl), TRUE);
        }
        &STOP
    }
}

fn main() {
    let mode = match cli::parse(std::env::args().skip(1)) {
        Ok(mode) => mode,
        Err(err) => {
            eprintln!("etherquic-host: {err}");
            eprintln!("{}", cli::USAGE);
            std::process::exit(2);
        }
    };
    match mode {
        cli::Mode::Help => {
            println!("{}", cli::USAGE);
        }
        cli::Mode::Approve => exit_result(run::signal(true)),
        cli::Mode::Deny => exit_result(run::signal(false)),
        cli::Mode::Client { peer, nic, service } => {
            let stop = foreground_stop();
            let paths = config::HostPaths::from_env();
            let log = run::Log::open(&paths.log);
            let result = run::connect(stop, &log, peer, nic.as_deref(), service);
            if let Err(err) = &result {
                log.line(&format!("client failed: {err}"));
            }
            exit_result(result);
        }
        cli::Mode::Console => {
            let stop = foreground_stop();
            let paths = config::HostPaths::from_env();
            let log = run::Log::open(&paths.log);
            let result = run::serve(stop, &log, |_| {});
            if let Err(err) = &result {
                log.line(&format!("host failed: {err}"));
            }
            exit_result(result);
        }
        cli::Mode::Service => dispatch_service(),
        cli::Mode::Desk => dispatch_desk(),
        cli::Mode::InstallService => dispatch_install(),
        cli::Mode::UninstallService { purge } => dispatch_uninstall(purge),
    }
}

fn dispatch_desk() {
    #[cfg(windows)]
    {
        let stop = foreground_stop();
        exit_result(desk::run(stop));
    }
    #[cfg(not(windows))]
    {
        eprintln!("etherquic-host: --desk runs on Windows");
        std::process::exit(1);
    }
}

fn exit_result(result: Result<(), String>) {
    match result {
        Ok(()) => {}
        Err(err) => {
            eprintln!("etherquic-host: {err}");
            std::process::exit(1);
        }
    }
}

fn foreground_stop() -> &'static std::sync::atomic::AtomicBool {
    #[cfg(windows)]
    {
        console_ctrl::install()
    }
    #[cfg(unix)]
    {
        unix_stop::install()
    }
    #[cfg(not(any(windows, unix)))]
    {
        static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        &STOP
    }
}

#[cfg(unix)]
mod unix_stop {
    use std::sync::atomic::{AtomicBool, Ordering};

    static STOP: AtomicBool = AtomicBool::new(false);

    extern "C" fn on_signal(_sig: libc::c_int) {
        STOP.store(true, Ordering::SeqCst);
    }

    pub fn install() -> &'static AtomicBool {
        unsafe {
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        }
        &STOP
    }
}

fn dispatch_service() {
    #[cfg(windows)]
    {
        if let Err(err) = service::dispatch() {
            eprintln!("etherquic-host: {err}");
            eprintln!("{}", cli::USAGE);
            std::process::exit(1);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let stop = foreground_stop();
        let paths = config::HostPaths::from_env();
        let log = run::Log::open(&paths.log);
        let result = run::serve(stop, &log, |_| {});
        if let Err(err) = &result {
            log.line(&format!("host failed: {err}"));
        }
        exit_result(result);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        eprintln!("etherquic-host runs as a Windows service or a macOS LaunchDaemon, or use --client / --console.");
        std::process::exit(1);
    }
}

fn dispatch_install() {
    #[cfg(target_os = "macos")]
    {
        exit_result(macos_service::install());
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("etherquic-host --install-service installs the macOS LaunchDaemon.");
        std::process::exit(1);
    }
}

fn dispatch_uninstall(purge: bool) {
    #[cfg(target_os = "macos")]
    {
        exit_result(macos_service::uninstall(purge));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = purge;
        eprintln!("etherquic-host --uninstall-service removes the macOS LaunchDaemon.");
        std::process::exit(1);
    }
}

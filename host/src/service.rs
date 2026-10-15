//! SCM entry point. The service name must match the installer (`etherquic-service`).

use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_dispatcher;

use crate::config::HostPaths;
use crate::run::{self, Log, Note};

pub const SERVICE_NAME: &str = "etherquic-service";

windows_service::define_windows_service!(ffi_service_main, service_main);

pub fn dispatch() -> Result<(), String> {
    service_dispatcher::start(SERVICE_NAME, ffi_service_main).map_err(|err| err.to_string())
}

fn service_main(_arguments: Vec<OsString>) {
    let paths = HostPaths::from_env();
    let log = Log::open(&paths.log);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_handler = Arc::clone(&stop);
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            stop_for_handler.store(true, Ordering::SeqCst);
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = match service_control_handler::register(SERVICE_NAME, handler) {
        Ok(status) => status,
        Err(err) => {
            log.line(&format!("service handler failed: {err}"));
            return;
        }
    };
    let _ = status.set_service_status(report(ServiceState::StartPending, false, 0));
    let result = run::serve(&stop, &log, |note| {
        let state = match note {
            Note::Ready => ServiceState::Running,
            Note::Stopping => ServiceState::StopPending,
        };
        let _ = status.set_service_status(report(state, true, 0));
    });
    let code = match result {
        Ok(()) => 0,
        Err(_) if stop.load(Ordering::SeqCst) => 0,
        Err(err) => {
            log.line(&format!("host failed: {err}"));
            1
        }
    };
    let _ = status.set_service_status(report(ServiceState::Stopped, false, code));
}

fn report(state: ServiceState, accept_stop: bool, exit_code: u32) -> ServiceStatus {
    let pending = matches!(
        state,
        ServiceState::StartPending | ServiceState::StopPending
    );
    ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: state,
        controls_accepted: if accept_stop {
            ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
        } else {
            ServiceControlAccept::empty()
        },
        exit_code: ServiceExitCode::Win32(exit_code),
        checkpoint: 0,
        wait_hint: if pending {
            Duration::from_secs(15)
        } else {
            Duration::default()
        },
        process_id: None,
    }
}

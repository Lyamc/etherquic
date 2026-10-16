//! The window binary is not a console program, so a Start Menu launch has no
//! console. A parent terminal still receives stdout. `--console` allocates a
//! console when nothing is attached.

#[cfg(windows)]
pub fn attach() {
    let flag = std::env::args().skip(1).any(|arg| arg == "--console");
    attach_win(flag);
}

#[cfg(not(windows))]
pub fn attach() {}

#[cfg(windows)]
fn attach_win(flag: bool) {
    use std::os::windows::io::IntoRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Console::{
        AllocConsole, AttachConsole, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE,
        STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };

    let parent = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } != 0;
    if !parent && flag {
        if unsafe { AllocConsole() } == 0 {
            return;
        }
    } else if !parent {
        return;
    }
    // Rust reads the standard handles on first use. These stay open for the process.
    unsafe {
        if let Ok(input) = std::fs::OpenOptions::new().read(true).open("CONIN$") {
            SetStdHandle(STD_INPUT_HANDLE, input.into_raw_handle() as HANDLE);
        }
        if let Ok(output) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            let handle = output.into_raw_handle() as HANDLE;
            SetStdHandle(STD_OUTPUT_HANDLE, handle);
            SetStdHandle(STD_ERROR_HANDLE, handle);
        }
    }
}

//! Desktop capture and injection for the console-session helper.
//!
//! GDI `BitBlt` is the picture. A desktop-duplication session is held only so
//! DWM keeps composing; those frames are released and not encoded. WASAPI
//! loopback and `SendInput` stay in this file too. The `windows` crate's GNU
//! import libraries do not link, so this uses `winapi` against the system DLLs.

use std::cell::Cell;
use std::mem;
use std::ptr;
use std::time::{Duration, Instant};

use winapi::ctypes::c_void;
use winapi::shared::dxgi::{
    CreateDXGIFactory1, IDXGIAdapter, IDXGIDevice, IDXGIFactory1, IDXGIOutput, IDXGIResource,
    DXGI_OUTPUT_DESC,
};
use winapi::shared::dxgi1_2::{IDXGIOutput1, IDXGIOutputDuplication, DXGI_OUTDUPL_FRAME_INFO};
use winapi::shared::minwindef::{DWORD, LPVOID};
use winapi::shared::mmreg::{
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_EXTENSIBLE, WAVE_FORMAT_IEEE_FLOAT,
    WAVE_FORMAT_PCM,
};
use winapi::shared::windef::{HDESK, HGDIOBJ, POINT};
use winapi::um::d3d11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use winapi::um::d3dcommon::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN};
use winapi::um::audioclient::{
    IAudioCaptureClient, IAudioClient, AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED,
};
use winapi::um::audiosessiontypes::{AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK};
use winapi::um::combaseapi::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL};
use winapi::um::errhandlingapi::GetLastError;
use winapi::um::mmdeviceapi::{
    eConsole, eRender, CLSID_MMDeviceEnumerator, IMMDevice, IMMDeviceEnumerator,
};
use winapi::um::unknwnbase::IUnknown;
use winapi::um::wingdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
use winapi::um::processthreadsapi::GetCurrentThreadId;
use winapi::um::winbase::SetThreadExecutionState;
use winapi::um::winnt::{ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, HANDLE};
use winapi::um::winuser::{
    CloseDesktop, GetCursorPos, GetDC, GetSystemMetrics, GetThreadDesktop,
    GetUserObjectInformationW, MapVirtualKeyW, OpenInputDesktop, ReleaseDC, SendInput,
    SetCursorPos, SetThreadDesktop, INPUT, INPUT_KEYBOARD, UOI_NAME,
    INPUT_MOUSE, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
    MAPVK_VK_TO_VSC_EX, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, SM_CXSCREEN, SM_CYSCREEN,
    VK_BACK, VK_CONTROL, VK_DELETE, VK_DIVIDE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_HOME,
    VK_INSERT, VK_LEFT, VK_LWIN, VK_MENU, VK_NEXT, VK_NUMLOCK, VK_OEM_1, VK_OEM_2, VK_OEM_3,
    VK_OEM_4, VK_OEM_5, VK_OEM_6, VK_OEM_7, VK_OEM_COMMA, VK_OEM_MINUS, VK_OEM_PERIOD,
    VK_OEM_PLUS, VK_PRIOR, VK_RCONTROL, VK_RETURN, VK_RIGHT, VK_RMENU, VK_RWIN, VK_SHIFT,
    VK_SNAPSHOT, VK_SPACE, VK_TAB, VK_UP,
};
use winapi::Interface;

use crate::proto::AudioChunk;

const COINIT_MULTITHREADED: u32 = 0;
const DXGI_ERROR_NOT_FOUND: i32 = 0x887A0002u32 as i32;
const DXGI_ERROR_ACCESS_LOST: i32 = 0x887A0026u32 as i32;
const DXGI_ERROR_WAIT_TIMEOUT: i32 = 0x887A0027u32 as i32;

/// Primary-monitor pixels, top-down RGBA.
pub fn capture_rgba() -> Result<(Vec<u8>, u32, u32), String> {
    unsafe { capture_rgba_gdi() }
}

/// GDI capture plus an optional duplication subscription.
///
/// The subscription is what makes DWM compose. An acquired frame is copied from
/// its texture. A timeout falls back to `BitBlt`. `desk.conf` `duplication=no`
/// skips DXGI entirely.
pub struct ScreenCapture {
    duplication: Option<Duplication>,
    mode: String,
    primed: bool,
    held: Option<(Vec<u8>, u32, u32)>,
    /// Input desktop this thread captures: `Default`, `Winlogon`, `Screen-saver`.
    desktop: String,
    /// Frames in a row that were only the held picture.
    stale: u32,
    /// Last attempt to get duplication back after it fell to GDI.
    retried: Instant,
}

/// Held-only frames before duplication is torn down and enumerated again.
/// At 10 fps this is about three seconds.
pub const STALE_LIMIT: u32 = 30;
/// How often GDI mode asks DXGI for an output again. A headless adapter's
/// output can be missing at start and appear later.
const RETRY_DUPLICATION: Duration = Duration::from_secs(5);

/// What the capture loop should do before taking the next frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Recover {
    Nothing,
    /// The input desktop changed. The held picture is from the old one.
    NewDesktop,
    /// Only the held picture for `STALE_LIMIT` frames: enumerate again.
    Stale,
    /// GDI mode, and it is time to ask for duplication again.
    RetryDuplication,
}

/// Pure decision so the policy is testable without a desktop.
pub fn recover_step(moved: bool, stale: u32, gdi_only: bool, since_retry: Duration, wants_dup: bool) -> Recover {
    if moved {
        Recover::NewDesktop
    } else if stale >= STALE_LIMIT {
        Recover::Stale
    } else if gdi_only && wants_dup && since_retry >= RETRY_DUPLICATION {
        Recover::RetryDuplication
    } else {
        Recover::Nothing
    }
}

struct Duplication {
    device: *mut ID3D11Device,
    context: *mut ID3D11DeviceContext,
    output: *mut IDXGIOutputDuplication,
}

enum PumpError {
    Lost,
    Failed(String),
}

impl ScreenCapture {
    pub fn open() -> Self {
        if !crate::config::desk_duplication() {
            return Self::gdi("gdi");
        }
        match unsafe { open_duplication() } {
            Ok(duplication) => Self {
                duplication: Some(duplication),
                mode: "duplication".to_string(),
                primed: false,
                held: None,
                desktop: thread_desktop_name(),
                stale: 0,
                retried: Instant::now(),
            },
            Err(error) => Self::gdi(&format!("gdi ({})", clip(error))),
        }
    }

    pub fn mode(&self) -> &str {
        &self.mode
    }

    pub fn desktop(&self) -> &str {
        &self.desktop
    }

    /// Follow the input desktop, and rebuild capture when it moved, when only
    /// the held picture has been available for a while, or when GDI mode is due
    /// to try duplication again. Then take one frame.
    pub fn frame(&mut self) -> Result<(Vec<u8>, u32, u32), String> {
        let moved = match follow_input_desktop() {
            Ok(Some(name)) => {
                self.desktop = name;
                true
            }
            Ok(None) => false,
            Err(_) => false,
        };
        let step = recover_step(
            moved,
            self.stale,
            self.duplication.is_none(),
            self.retried.elapsed(),
            crate::config::desk_duplication(),
        );
        match step {
            Recover::Nothing => {}
            Recover::NewDesktop | Recover::Stale => {
                self.held = None;
                self.stale = 0;
                if crate::config::desk_duplication() {
                    self.retried = Instant::now();
                    self.reopen_after_loss();
                }
            }
            Recover::RetryDuplication => {
                self.retried = Instant::now();
                self.reopen_after_loss();
            }
        }
        self.frame_once()
    }

    fn frame_once(&mut self) -> Result<(Vec<u8>, u32, u32), String> {
        if self.duplication.is_none() {
            return self.bitblt_or_held();
        }
        let timeout = if self.primed { 0 } else { 50 };
        let acquired = unsafe { acquire(self.duplication.as_ref().unwrap(), timeout) };
        match acquired {
            Ok(Some(picture)) => {
                self.primed = true;
                self.stale = 0;
                self.held = Some(picture.clone());
                if self.mode != "duplication" {
                    self.mode = "duplication".to_string();
                }
                Ok(picture)
            }
            Ok(None) => self.bitblt_or_held(),
            Err(PumpError::Lost) => {
                self.reopen_after_loss();
                if let Some(picture) = self.held.clone() {
                    return Ok(picture);
                }
                self.bitblt_or_held()
            }
            Err(PumpError::Failed(error)) => {
                let broken = error.starts_with("AcquireNextFrame") || error.starts_with("ReleaseFrame");
                if broken {
                    self.drop_to_gdi(error);
                } else {
                    let next = format!("duplication ({})", clip(error));
                    if self.mode != next {
                        self.mode = next;
                    }
                }
                self.bitblt_or_held()
            }
        }
    }

    fn gdi(mode: &str) -> Self {
        Self {
            duplication: None,
            mode: mode.to_string(),
            primed: true,
            held: None,
            desktop: thread_desktop_name(),
            stale: 0,
            retried: Instant::now(),
        }
    }

    fn bitblt_or_held(&mut self) -> Result<(Vec<u8>, u32, u32), String> {
        let mut result = unsafe { capture_rgba_gdi() };
        if result.is_err() {
            // BitBlt fails (win32 6) from a desktop that is not the input desktop.
            if let Ok(Some(name)) = follow_input_desktop() {
                self.desktop = name;
                self.held = None;
                result = unsafe { capture_rgba_gdi() };
            }
        }
        match result {
            Ok(picture) => {
                self.stale = 0;
                Ok(picture)
            }
            Err(error) => {
                self.stale = self.stale.saturating_add(1);
                self.held.clone().ok_or(error)
            }
        }
    }

    fn drop_to_gdi(&mut self, error: String) {
        self.duplication = None;
        self.primed = true;
        self.mode = format!("gdi ({})", clip(error));
    }

    fn reopen_after_loss(&mut self) {
        self.duplication = None;
        self.primed = false;
        match unsafe { open_duplication() } {
            Ok(dup) => match unsafe { acquire(&dup, 50) } {
                Ok(Some(picture)) => {
                    self.primed = true;
                    self.held = Some(picture);
                    self.duplication = Some(dup);
                    self.mode = "duplication".to_string();
                }
                Ok(None) => {
                    self.duplication = Some(dup);
                    self.mode = "duplication".to_string();
                }
                Err(PumpError::Lost) => {
                    drop(dup);
                    self.mode = "gdi (desktop duplication access lost)".to_string();
                }
                Err(PumpError::Failed(error)) => {
                    drop(dup);
                    self.mode = format!("gdi ({})", clip(error));
                }
            },
            Err(error) => self.mode = format!("gdi ({})", clip(error)),
        }
    }
}

impl Drop for Duplication {
    fn drop(&mut self) {
        unsafe {
            if !self.output.is_null() {
                (*self.output).Release();
                self.output = ptr::null_mut();
            }
            if !self.context.is_null() {
                (*self.context).Release();
                self.context = ptr::null_mut();
            }
            if !self.device.is_null() {
                (*self.device).Release();
                self.device = ptr::null_mut();
            }
        }
    }
}

fn hresult(code: i32) -> String {
    format!("{:#x}", code as u32)
}

fn clip(error: String) -> String {
    if error.len() > 300 {
        format!("{}...", &error[..300])
    } else {
        error
    }
}

fn release_device(device: *mut ID3D11Device, context: *mut ID3D11DeviceContext) {
    unsafe {
        if !context.is_null() {
            (*context).Release();
        }
        if !device.is_null() {
            (*device).Release();
        }
    }
}

/// Every desktop right except `DELETE`, `WRITE_DAC`, and `WRITE_OWNER`:
/// read objects for GDI, journal playback for `SendInput`.
const DESKTOP_ACCESS: u32 = 0x01ff;

thread_local! {
    /// Desktop handle this thread opened and now sits on. Closed on the next move.
    static THREAD_DESK: Cell<HDESK> = const { Cell::new(ptr::null_mut()) };
}

unsafe fn desktop_name(desk: HDESK) -> Option<String> {
    if desk.is_null() {
        return None;
    }
    let mut buf = [0u16; 256];
    let mut needed: DWORD = 0;
    let ok = GetUserObjectInformationW(
        desk as HANDLE,
        UOI_NAME as i32,
        buf.as_mut_ptr() as *mut c_void,
        (buf.len() * 2) as DWORD,
        &mut needed,
    );
    if ok == 0 {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Desktop the calling thread is on, for logging.
pub fn thread_desktop_name() -> String {
    unsafe { desktop_name(GetThreadDesktop(GetCurrentThreadId())) }.unwrap_or_else(|| "unknown".into())
}

/// Move the calling thread onto the desktop that receives input.
///
/// A screen saver runs on `Screen-saver`, and the lock screen, UAC, and
/// Ctrl+Alt+Del run on `Winlogon`. A thread left on `Default` gets
/// `BitBlt` win32 6, a frozen duplication, and `SendInput` win32 5 until the
/// user comes back. `Ok(Some(name))` means the thread moved, `Ok(None)` that
/// it was already there. The helper is SYSTEM, so it may open `Winlogon`.
pub fn follow_input_desktop() -> Result<Option<String>, String> {
    unsafe {
        let input = OpenInputDesktop(0, 0, DESKTOP_ACCESS);
        if input.is_null() {
            return Err(win32("OpenInputDesktop"));
        }
        let want = desktop_name(input);
        let have = desktop_name(GetThreadDesktop(GetCurrentThreadId()));
        if want.is_some() && want == have {
            CloseDesktop(input);
            return Ok(None);
        }
        if SetThreadDesktop(input) == 0 {
            let error = win32("SetThreadDesktop");
            CloseDesktop(input);
            return Err(error);
        }
        let previous = THREAD_DESK.with(|slot| slot.replace(input));
        if !previous.is_null() {
            CloseDesktop(previous);
        }
        Ok(Some(want.unwrap_or_else(|| "unknown".into())))
    }
}

/// Turn the display on and end a screen saver for a player that just attached.
///
/// The execution state resets the display idle timer, which powers an output
/// back up. A screen saver only ends on input, so the pointer moves a few
/// pixels on the input desktop and is put back.
pub fn wake_display() -> Result<(), String> {
    unsafe {
        SetThreadExecutionState(ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED);
    }
    let _ = follow_input_desktop();
    let mut at = POINT { x: 0, y: 0 };
    let have_point = unsafe { GetCursorPos(&mut at) } != 0;
    send_mouse(16, 16, 0, MOUSEEVENTF_MOVE)?;
    send_mouse(-16, -16, 0, MOUSEEVENTF_MOVE)?;
    if have_point {
        unsafe {
            SetCursorPos(at.x, at.y);
        }
    }
    Ok(())
}

/// Keep the display and the system awake while a player is attached.
///
/// Per thread: call `hold_display(false)` from the thread that held it.
pub fn hold_display(on: bool) {
    let flags = if on {
        ES_CONTINUOUS | ES_DISPLAY_REQUIRED | ES_SYSTEM_REQUIRED
    } else {
        ES_CONTINUOUS
    };
    unsafe {
        SetThreadExecutionState(flags);
    }
}

/// Release every acquired frame. Leaving one held stalls the desktop.
unsafe fn acquire(
    dup: &Duplication,
    timeout_ms: u32,
) -> Result<Option<(Vec<u8>, u32, u32)>, PumpError> {
    let mut info: DXGI_OUTDUPL_FRAME_INFO = mem::zeroed();
    let mut resource: *mut IDXGIResource = ptr::null_mut();
    let hr = (*dup.output).AcquireNextFrame(timeout_ms, &mut info, &mut resource);
    if hr == DXGI_ERROR_WAIT_TIMEOUT {
        return Ok(None);
    }
    if hr == DXGI_ERROR_ACCESS_LOST {
        return Err(PumpError::Lost);
    }
    if hr < 0 {
        return Err(PumpError::Failed(format!(
            "AcquireNextFrame: hresult {}",
            hresult(hr)
        )));
    }
    let copied = if resource.is_null() {
        Err("desktop frame has no texture".to_string())
    } else {
        copy_texture(dup, resource)
    };
    if !resource.is_null() {
        (*resource).Release();
    }
    let released = (*dup.output).ReleaseFrame();
    if released == DXGI_ERROR_ACCESS_LOST {
        return Err(PumpError::Lost);
    }
    if released < 0 {
        return Err(PumpError::Failed(format!(
            "ReleaseFrame: hresult {}",
            hresult(released)
        )));
    }
    match copied {
        Ok(picture) => Ok(Some(picture)),
        Err(error) => Err(PumpError::Failed(error)),
    }
}

unsafe fn copy_texture(
    dup: &Duplication,
    resource: *mut IDXGIResource,
) -> Result<(Vec<u8>, u32, u32), String> {
    let mut texture: *mut ID3D11Texture2D = ptr::null_mut();
    let hr = (*resource).QueryInterface(
        &ID3D11Texture2D::uuidof(),
        &mut texture as *mut *mut ID3D11Texture2D as *mut *mut c_void,
    );
    if hr < 0 || texture.is_null() {
        return Err(format!("desktop texture: hresult {}", hresult(hr)));
    }
    let mut desc: D3D11_TEXTURE2D_DESC = mem::zeroed();
    (*texture).GetDesc(&mut desc);
    if desc.Width == 0 || desc.Height == 0 {
        (*texture).Release();
        return Err("desktop texture is empty".into());
    }
    let mut staging = desc;
    staging.MipLevels = 1;
    staging.ArraySize = 1;
    staging.SampleDesc.Count = 1;
    staging.SampleDesc.Quality = 0;
    staging.Usage = D3D11_USAGE_STAGING;
    staging.BindFlags = 0;
    staging.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    staging.MiscFlags = 0;
    let mut readable: *mut ID3D11Texture2D = ptr::null_mut();
    let hr = (*dup.device).CreateTexture2D(&staging, ptr::null(), &mut readable);
    if hr < 0 || readable.is_null() {
        (*texture).Release();
        return Err(format!("staging texture: hresult {}", hresult(hr)));
    }
    (*dup.context).CopyResource(
        readable as *mut ID3D11Resource,
        texture as *mut ID3D11Resource,
    );
    let mut mapped: D3D11_MAPPED_SUBRESOURCE = mem::zeroed();
    let hr = (*dup.context).Map(
        readable as *mut ID3D11Resource,
        0,
        D3D11_MAP_READ,
        0,
        &mut mapped,
    );
    if hr < 0 || mapped.pData.is_null() {
        (*readable).Release();
        (*texture).Release();
        return Err(format!("Map desktop: hresult {}", hresult(hr)));
    }
    let width = desc.Width as usize;
    let height = desc.Height as usize;
    let pitch = mapped.RowPitch as usize;
    let mut rgba = vec![0u8; width.saturating_mul(height).saturating_mul(4)];
    let short_pitch = pitch < width.saturating_mul(4) || rgba.len() != width * height * 4;
    if !short_pitch {
        let src = mapped.pData as *const u8;
        for y in 0..height {
            let row = src.add(y * pitch);
            for x in 0..width {
                let pixel = row.add(x * 4);
                let at = (y * width + x) * 4;
                rgba[at] = *pixel.add(2);
                rgba[at + 1] = *pixel.add(1);
                rgba[at + 2] = *pixel;
                rgba[at + 3] = 255;
            }
        }
    }
    (*dup.context).Unmap(readable as *mut ID3D11Resource, 0);
    (*readable).Release();
    (*texture).Release();
    if short_pitch {
        return Err("desktop texture pitch is short".into());
    }
    Ok((rgba, desc.Width, desc.Height))
}

unsafe fn open_duplication() -> Result<Duplication, String> {
    let _ = follow_input_desktop();
    match open_from_factory() {
        Ok(dup) => Ok(dup),
        Err(factory_error) => match open_hardware() {
            Ok(dup) => Ok(dup),
            Err(hardware_error) => Err(format!("{factory_error}; hardware: {hardware_error}")),
        },
    }
}

unsafe fn open_from_factory() -> Result<Duplication, String> {
    let mut factory: *mut IDXGIFactory1 = ptr::null_mut();
    let hr = CreateDXGIFactory1(
        &IDXGIFactory1::uuidof(),
        &mut factory as *mut *mut IDXGIFactory1 as *mut *mut c_void,
    );
    if hr < 0 || factory.is_null() {
        return Err(format!("CreateDXGIFactory1: hresult {}", hresult(hr)));
    }
    let mut errors = Vec::new();
    let mut found = None;
    let mut adapter_index = 0u32;
    while found.is_none() && adapter_index < 16 {
        let mut adapter: *mut IDXGIAdapter = ptr::null_mut();
        let hr = (*factory).EnumAdapters(adapter_index, &mut adapter);
        if hr == DXGI_ERROR_NOT_FOUND {
            break;
        }
        if hr < 0 || adapter.is_null() {
            errors.push(format!("adapter {adapter_index}: EnumAdapters {}", hresult(hr)));
            break;
        }
        match duplicate_on_adapter(adapter) {
            Ok(dup) => found = Some(dup),
            Err(error) => errors.push(format!("adapter {adapter_index}: {error}")),
        }
        (*adapter).Release();
        adapter_index += 1;
    }
    (*factory).Release();
    found.ok_or_else(|| {
        if errors.is_empty() {
            "no adapters".to_string()
        } else {
            errors.join("; ")
        }
    })
}

unsafe fn open_hardware() -> Result<Duplication, String> {
    let (device, context) = create_device(ptr::null_mut(), D3D_DRIVER_TYPE_HARDWARE)?;
    let mut dxgi: *mut IDXGIDevice = ptr::null_mut();
    let hr = (*device).QueryInterface(
        &IDXGIDevice::uuidof(),
        &mut dxgi as *mut *mut IDXGIDevice as *mut *mut c_void,
    );
    if hr < 0 || dxgi.is_null() {
        release_device(device, context);
        return Err(format!("IDXGIDevice: hresult {}", hresult(hr)));
    }
    let mut adapter: *mut IDXGIAdapter = ptr::null_mut();
    let hr = (*dxgi).GetAdapter(&mut adapter);
    (*dxgi).Release();
    if hr < 0 || adapter.is_null() {
        release_device(device, context);
        return Err(format!("GetAdapter: hresult {}", hresult(hr)));
    }
    let duplicated = duplicate_with(adapter, device, context);
    (*adapter).Release();
    duplicated
}

unsafe fn duplicate_on_adapter(adapter: *mut IDXGIAdapter) -> Result<Duplication, String> {
    let (device, context) = create_device(adapter, D3D_DRIVER_TYPE_UNKNOWN)?;
    duplicate_with(adapter, device, context)
}

unsafe fn duplicate_with(
    adapter: *mut IDXGIAdapter,
    device: *mut ID3D11Device,
    context: *mut ID3D11DeviceContext,
) -> Result<Duplication, String> {
    let mut failure = "no outputs".to_string();
    for require_origin in [true, false] {
        match first_duplication(adapter, device, require_origin) {
            Ok(Some(output)) => return Ok(Duplication { device, context, output }),
            Ok(None) => {}
            Err(error) => failure = error,
        }
    }
    release_device(device, context);
    Err(failure)
}

unsafe fn create_device(
    adapter: *mut IDXGIAdapter,
    driver: u32,
) -> Result<(*mut ID3D11Device, *mut ID3D11DeviceContext), String> {
    let mut device: *mut ID3D11Device = ptr::null_mut();
    let mut context: *mut ID3D11DeviceContext = ptr::null_mut();
    let hr = D3D11CreateDevice(
        adapter,
        driver,
        ptr::null_mut(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        ptr::null(),
        0,
        D3D11_SDK_VERSION,
        &mut device,
        ptr::null_mut(),
        &mut context,
    );
    if hr < 0 || device.is_null() || context.is_null() {
        release_device(device, context);
        return Err(format!("D3D11CreateDevice: hresult {}", hresult(hr)));
    }
    Ok((device, context))
}

unsafe fn first_duplication(
    adapter: *mut IDXGIAdapter,
    device: *mut ID3D11Device,
    require_origin: bool,
) -> Result<Option<*mut IDXGIOutputDuplication>, String> {
    let mut index = 0u32;
    let mut tried = false;
    let mut failure = String::new();
    while index < 16 {
        let mut output: *mut IDXGIOutput = ptr::null_mut();
        let hr = (*adapter).EnumOutputs(index, &mut output);
        index += 1;
        if hr == DXGI_ERROR_NOT_FOUND {
            break;
        }
        if hr < 0 || output.is_null() {
            return Err(format!("EnumOutputs: hresult {}", hresult(hr)));
        }
        if !output_suits(output, require_origin) {
            (*output).Release();
            continue;
        }
        tried = true;
        match duplicate_output(device, output) {
            Ok(duplication) => {
                (*output).Release();
                return Ok(Some(duplication));
            }
            Err(error) => failure = error,
        }
        (*output).Release();
    }
    if tried {
        Err(failure)
    } else {
        Ok(None)
    }
}

unsafe fn output_suits(output: *mut IDXGIOutput, require_origin: bool) -> bool {
    if !require_origin {
        return true;
    }
    let mut desc: DXGI_OUTPUT_DESC = mem::zeroed();
    if (*output).GetDesc(&mut desc) < 0 {
        return false;
    }
    desc.DesktopCoordinates.left <= 0
        && desc.DesktopCoordinates.top <= 0
        && desc.DesktopCoordinates.right > 0
        && desc.DesktopCoordinates.bottom > 0
}

unsafe fn duplicate_output(
    device: *mut ID3D11Device,
    output: *mut IDXGIOutput,
) -> Result<*mut IDXGIOutputDuplication, String> {
    let mut output1: *mut IDXGIOutput1 = ptr::null_mut();
    let hr = (*output).QueryInterface(
        &IDXGIOutput1::uuidof(),
        &mut output1 as *mut *mut IDXGIOutput1 as *mut *mut c_void,
    );
    if hr < 0 || output1.is_null() {
        return Err(format!("IDXGIOutput1: hresult {}", hresult(hr)));
    }
    let mut duplication: *mut IDXGIOutputDuplication = ptr::null_mut();
    let hr = (*output1).DuplicateOutput(device as *mut IUnknown, &mut duplication);
    (*output1).Release();
    if hr < 0 || duplication.is_null() {
        if !duplication.is_null() {
            (*duplication).Release();
        }
        return Err(format!("DuplicateOutput: hresult {}", hresult(hr)));
    }
    Ok(duplication)
}

/// Default render endpoint, opened in loopback so a silent desktop still yields PCM.
pub struct Loopback {
    client: *mut IAudioClient,
    capture: *mut IAudioCaptureClient,
    rate: u32,
    channels: u16,
    tag: u16,
    bits: u16,
    com: bool,
}

impl Loopback {
    pub fn open() -> Result<Self, String> {
        unsafe { open_loopback() }
    }

    pub fn name(&self) -> &str {
        "default output"
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// One WASAPI packet, or `None` when the capture buffer is empty.
    pub fn poll(&mut self) -> Result<Option<AudioChunk>, String> {
        unsafe {
            let mut data: *mut u8 = ptr::null_mut();
            let mut frames: u32 = 0;
            let mut flags: DWORD = 0;
            let code = (*self.capture).GetBuffer(
                &mut data,
                &mut frames,
                &mut flags,
                ptr::null_mut(),
                ptr::null_mut(),
            );
            if code < 0 {
                return Err(format!("GetBuffer: hresult {code:#x}"));
            }
            if frames == 0 {
                return Ok(None);
            }
            let copied = copy_packet(data, frames as usize, flags, self);
            let released = (*self.capture).ReleaseBuffer(frames);
            if released < 0 {
                return Err(format!("ReleaseBuffer: hresult {released:#x}"));
            }
            copied.map(Some)
        }
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        unsafe {
            if !self.client.is_null() {
                let _ = (*self.client).Stop();
            }
            if !self.capture.is_null() {
                (*self.capture).Release();
                self.capture = ptr::null_mut();
            }
            if !self.client.is_null() {
                (*self.client).Release();
                self.client = ptr::null_mut();
            }
            if self.com {
                CoUninitialize();
                self.com = false;
            }
        }
    }
}

/// EQIN absolute position. `x` and `y` are already `0..=65535` from the top left.
pub fn pointer(x: u16, y: u16) -> Result<(), String> {
    send_mouse(
        i32::from(x),
        i32::from(y),
        0,
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
    )
}

/// `bit` is an EQIN button bit: 1 left, 2 middle, 4 right.
pub fn button(bit: u8, down: bool) -> Result<(), String> {
    let flags = match (bit, down) {
        (1, true) => MOUSEEVENTF_LEFTDOWN,
        (1, false) => MOUSEEVENTF_LEFTUP,
        (2, true) => MOUSEEVENTF_MIDDLEDOWN,
        (2, false) => MOUSEEVENTF_MIDDLEUP,
        (4, true) => MOUSEEVENTF_RIGHTDOWN,
        (4, false) => MOUSEEVENTF_RIGHTUP,
        _ => return Err(format!("unknown button {bit}")),
    };
    send_mouse(0, 0, 0, flags)
}

/// Notch counts. Windows wants `120` units per notch.
pub fn wheel(y: i16, x: i16) -> Result<(), String> {
    if y != 0 {
        let delta = i32::from(y).saturating_mul(120);
        send_mouse(0, 0, delta as u32, MOUSEEVENTF_WHEEL)?;
    }
    if x != 0 {
        let delta = i32::from(x).saturating_mul(120);
        send_mouse(0, 0, delta as u32, MOUSEEVENTF_HWHEEL)?;
    }
    Ok(())
}

/// X11 keysym from the player. `Ok(false)` means the key is ignored.
///
/// Letters and digits used to go out as `KEYEVENTF_UNICODE`. That posts
/// `WM_CHAR` and does not press the key, so a game never sees `1` or `A`.
pub fn key_keysym(code: u32, down: bool) -> Result<bool, String> {
    let mut input: INPUT = unsafe { mem::zeroed() };
    input.type_ = INPUT_KEYBOARD;
    unsafe {
        let key = input.u.ki_mut();
        if let Some((vk, extended)) = mapped_virtual_key(code) {
            fill_virtual_key(key, vk, down, extended);
        } else if code < 0x80 {
            let up = if down { 0 } else { KEYEVENTF_KEYUP };
            key.wScan = code as u16;
            key.dwFlags = KEYEVENTF_UNICODE | up;
        } else {
            return Ok(false);
        }
    }
    send_input(&mut input)?;
    Ok(true)
}

/// US layout. Shifted characters use the same physical key. The player
/// already sends Shift on its own. `winapi` does not export `VK_0` or `VK_A`.
fn ascii_key(code: u32) -> Option<u16> {
    const VK_0: u16 = 0x30;
    const VK_A: u16 = 0x41;
    let ch = char::from_u32(code)?;
    let vk = match ch {
        'a'..='z' => VK_A + (ch as u16 - u16::from(b'a')),
        'A'..='Z' => VK_A + (ch as u16 - u16::from(b'A')),
        '0'..='9' => VK_0 + (ch as u16 - u16::from(b'0')),
        ' ' => VK_SPACE as u16,
        '!' => VK_0 + 1,
        '@' => VK_0 + 2,
        '#' => VK_0 + 3,
        '$' => VK_0 + 4,
        '%' => VK_0 + 5,
        '^' => VK_0 + 6,
        '&' => VK_0 + 7,
        '*' => VK_0 + 8,
        '(' => VK_0 + 9,
        ')' => VK_0,
        '-' | '_' => VK_OEM_MINUS as u16,
        '=' | '+' => VK_OEM_PLUS as u16,
        '[' | '{' => VK_OEM_4 as u16,
        ']' | '}' => VK_OEM_6 as u16,
        '\\' | '|' => VK_OEM_5 as u16,
        ';' | ':' => VK_OEM_1 as u16,
        '\'' | '"' => VK_OEM_7 as u16,
        ',' | '<' => VK_OEM_COMMA as u16,
        '.' | '>' => VK_OEM_PERIOD as u16,
        '/' | '?' => VK_OEM_2 as u16,
        '`' | '~' => VK_OEM_3 as u16,
        _ => return None,
    };
    Some(vk)
}

/// Scan code plus the virtual key. Games read the scan code. `wVk` is still
/// set for callers that ignore the scan-code flag.
///
/// Arrow, Home, End, Page Up/Down, Insert, and Delete share a scan code with
/// the numpad. Without `KEYEVENTF_EXTENDEDKEY`, NumLock on the host types
/// 2/4/6/8 instead of moving. `MapVirtualKeyW` often omits the 0xE0 prefix
/// for those keys, so the flag is taken from [`extended_virtual_key`] as well.
unsafe fn fill_virtual_key(
    key: &mut winapi::um::winuser::KEYBDINPUT,
    vk: u16,
    down: bool,
    extended: bool,
) {
    let up = if down { 0 } else { KEYEVENTF_KEYUP };
    let mapped = MapVirtualKeyW(u32::from(vk), MAPVK_VK_TO_VSC_EX);
    key.wVk = vk;
    if mapped == 0 {
        key.dwFlags = up | if extended { KEYEVENTF_EXTENDEDKEY } else { 0 };
        return;
    }
    let mut flags = KEYEVENTF_SCANCODE | up;
    let mut scan = mapped as u16;
    if extended || mapped & 0xFF00 == 0xE000 || mapped & 0xFF00 == 0xE100 {
        flags |= KEYEVENTF_EXTENDEDKEY;
        scan = (mapped & 0xFF) as u16;
    }
    key.wScan = scan;
    key.dwFlags = flags;
}

fn mapped_virtual_key(code: u32) -> Option<(u16, bool)> {
    if let Some(vk) = virtual_key(code) {
        // Keypad Enter is VK_RETURN with the extended flag. The main Enter
        // key is the same VK without it.
        let extended = extended_virtual_key(vk) || code == 0xff8d;
        return Some((vk, extended));
    }
    Some((ascii_key(code)?, false))
}

/// Navigation keys on the 101-key layout. MSDN "Extended-Key Flag".
fn extended_virtual_key(vk: u16) -> bool {
    vk == VK_LEFT as u16
        || vk == VK_RIGHT as u16
        || vk == VK_UP as u16
        || vk == VK_DOWN as u16
        || vk == VK_HOME as u16
        || vk == VK_END as u16
        || vk == VK_PRIOR as u16
        || vk == VK_NEXT as u16
        || vk == VK_INSERT as u16
        || vk == VK_DELETE as u16
        || vk == VK_DIVIDE as u16
        || vk == VK_NUMLOCK as u16
        || vk == VK_RCONTROL as u16
        || vk == VK_RMENU as u16
        || vk == VK_LWIN as u16
        || vk == VK_RWIN as u16
        || vk == VK_SNAPSHOT as u16
}

fn virtual_key(code: u32) -> Option<u16> {
    let vk = match code {
        0xff1b => VK_ESCAPE,
        0xff0d | 0xff8d => VK_RETURN,
        0xff08 => VK_BACK,
        0xff09 => VK_TAB,
        0xffff | 0xff9f => VK_DELETE,
        0xff63 | 0xff9e => VK_INSERT,
        0xff51 | 0xff96 => VK_LEFT,
        0xff52 | 0xff97 => VK_UP,
        0xff53 | 0xff98 => VK_RIGHT,
        0xff54 | 0xff99 => VK_DOWN,
        0xff50 | 0xff95 => VK_HOME,
        0xff57 | 0xff9c => VK_END,
        0xff55 | 0xff9a => VK_PRIOR,
        0xff56 | 0xff9b => VK_NEXT,
        0xffe1 | 0xffe2 => VK_SHIFT,
        0xffe3 | 0xffe4 => VK_CONTROL,
        0xffe9 | 0xffea => VK_MENU,
        0xffeb | 0xffec => VK_LWIN,
        0xffbe => VK_F1,
        0xffbf => VK_F1 + 1,
        0xffc0 => VK_F1 + 2,
        0xffc1 => VK_F1 + 3,
        0xffc2 => VK_F1 + 4,
        0xffc3 => VK_F1 + 5,
        0xffc4 => VK_F1 + 6,
        0xffc5 => VK_F1 + 7,
        0xffc6 => VK_F1 + 8,
        0xffc7 => VK_F1 + 9,
        0xffc8 => VK_F1 + 10,
        0xffc9 => VK_F1 + 11,
        _ => return None,
    };
    Some(vk as u16)
}

fn send_mouse(dx: i32, dy: i32, data: u32, flags: u32) -> Result<(), String> {
    let mut input: INPUT = unsafe { mem::zeroed() };
    input.type_ = INPUT_MOUSE;
    unsafe {
        let mouse = input.u.mi_mut();
        mouse.dx = dx;
        mouse.dy = dy;
        mouse.mouseData = data;
        mouse.dwFlags = flags;
    }
    send_input(&mut input)
}

fn send_input(input: &mut INPUT) -> Result<(), String> {
    if send_once(input) {
        return Ok(());
    }
    let code = unsafe { GetLastError() };
    // Win32 5: input belongs to another desktop now. Move there and retry once.
    if matches!(follow_input_desktop(), Ok(Some(_))) && send_once(input) {
        return Ok(());
    }
    Err(format!("SendInput: win32 {code}"))
}

fn send_once(input: &mut INPUT) -> bool {
    unsafe { SendInput(1, input, mem::size_of::<INPUT>() as i32) != 0 }
}

unsafe fn capture_rgba_gdi() -> Result<(Vec<u8>, u32, u32), String> {
    let width = GetSystemMetrics(SM_CXSCREEN);
    let height = GetSystemMetrics(SM_CYSCREEN);
    if width <= 0 || height <= 0 {
        return Err("no screen".into());
    }
    let screen = GetDC(ptr::null_mut());
    if screen.is_null() {
        return Err(win32("GetDC"));
    }
    let mem = CreateCompatibleDC(screen);
    if mem.is_null() {
        ReleaseDC(ptr::null_mut(), screen);
        return Err(win32("CreateCompatibleDC"));
    }
    let mut info: BITMAPINFO = mem::zeroed();
    info.bmiHeader.biSize = mem::size_of::<BITMAPINFOHEADER>() as u32;
    info.bmiHeader.biWidth = width;
    info.bmiHeader.biHeight = -height;
    info.bmiHeader.biPlanes = 1;
    info.bmiHeader.biBitCount = 32;
    info.bmiHeader.biCompression = BI_RGB;
    let mut bits: *mut winapi::ctypes::c_void = ptr::null_mut();
    let bitmap = CreateDIBSection(mem, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0);
    if bitmap.is_null() || bits.is_null() {
        DeleteDC(mem);
        ReleaseDC(ptr::null_mut(), screen);
        return Err(win32("CreateDIBSection"));
    }
    let previous = SelectObject(mem, bitmap as HGDIOBJ);
    let copied = BitBlt(mem, 0, 0, width, height, screen, 0, 0, SRCCOPY);
    let bitblt_error = if copied == 0 { GetLastError() } else { 0 };
    let count = (width as usize).saturating_mul(height as usize);
    let mut rgba = vec![0u8; count.saturating_mul(4)];
    if copied != 0 && rgba.len() == count * 4 {
        let src = std::slice::from_raw_parts(bits as *const u8, count * 4);
        for index in 0..count {
            let src_at = index * 4;
            let dst_at = index * 4;
            rgba[dst_at] = src[src_at + 2];
            rgba[dst_at + 1] = src[src_at + 1];
            rgba[dst_at + 2] = src[src_at];
            rgba[dst_at + 3] = 255;
        }
    }
    if !previous.is_null() {
        SelectObject(mem, previous);
    }
    DeleteObject(bitmap as HGDIOBJ);
    DeleteDC(mem);
    ReleaseDC(ptr::null_mut(), screen);
    if copied == 0 {
        return Err(format!("BitBlt: win32 {bitblt_error}"));
    }
    Ok((rgba, width as u32, height as u32))
}

fn win32(what: &str) -> String {
    let code = unsafe { GetLastError() };
    format!("{what}: win32 {code}")
}

unsafe fn open_loopback() -> Result<Loopback, String> {
    let hr = CoInitializeEx(ptr::null_mut(), COINIT_MULTITHREADED);
    if hr < 0 {
        return Err(format!("CoInitializeEx: hresult {hr:#x}"));
    }
    let mut opened = Loopback {
        client: ptr::null_mut(),
        capture: ptr::null_mut(),
        rate: 0,
        channels: 0,
        tag: 0,
        bits: 0,
        com: true,
    };
    let mut enumerator: *mut IMMDeviceEnumerator = ptr::null_mut();
    let hr = CoCreateInstance(
        &CLSID_MMDeviceEnumerator,
        ptr::null_mut(),
        CLSCTX_ALL,
        &IMMDeviceEnumerator::uuidof(),
        &mut enumerator as *mut *mut IMMDeviceEnumerator as *mut LPVOID,
    );
    if hr < 0 || enumerator.is_null() {
        return Err(format!("CoCreateInstance audio: hresult {hr:#x}"));
    }
    let mut device: *mut IMMDevice = ptr::null_mut();
    let hr = (*enumerator).GetDefaultAudioEndpoint(eRender, eConsole, &mut device);
    if hr < 0 || device.is_null() {
        (*enumerator).Release();
        return Err(format!("default output: hresult {hr:#x}"));
    }
    let mut format: *mut WAVEFORMATEX = ptr::null_mut();
    let started = (|| {
        let mut client: *mut IAudioClient = ptr::null_mut();
        let hr = (*device).Activate(
            &IAudioClient::uuidof(),
            CLSCTX_ALL,
            ptr::null_mut(),
            &mut client as *mut *mut IAudioClient as *mut LPVOID,
        );
        if hr < 0 || client.is_null() {
            return Err(format!("Activate audio: hresult {hr:#x}"));
        }
        let hr = (*client).GetMixFormat(&mut format);
        if hr < 0 || format.is_null() {
            (*client).Release();
            return Err(format!("GetMixFormat: hresult {hr:#x}"));
        }
        let mix = match read_mix(format) {
            Ok(mix) => mix,
            Err(error) => {
                (*client).Release();
                return Err(error);
            }
        };
        let client = match init_loopback(device, client, format, mix.rate) {
            Ok(client) => client,
            Err(error) => return Err(error),
        };
        let mut capture: *mut IAudioCaptureClient = ptr::null_mut();
        let hr = (*client).GetService(
            &IAudioCaptureClient::uuidof(),
            &mut capture as *mut *mut IAudioCaptureClient as *mut LPVOID,
        );
        if hr < 0 || capture.is_null() {
            (*client).Release();
            return Err(format!("GetService capture: hresult {hr:#x}"));
        }
        let hr = (*client).Start();
        if hr < 0 {
            (*capture).Release();
            (*client).Release();
            return Err(format!("Start loopback: hresult {hr:#x}"));
        }
        opened.client = client;
        opened.capture = capture;
        opened.rate = mix.rate;
        opened.channels = mix.channels;
        opened.tag = mix.tag;
        opened.bits = mix.bits;
        Ok(())
    })();
    if !format.is_null() {
        CoTaskMemFree(format as LPVOID);
    }
    (*device).Release();
    (*enumerator).Release();
    started?;
    Ok(opened)
}

struct Mix {
    rate: u32,
    channels: u16,
    tag: u16,
    bits: u16,
}

unsafe fn read_mix(format: *const WAVEFORMATEX) -> Result<Mix, String> {
    let tag = ptr::read_unaligned(ptr::addr_of!((*format).wFormatTag));
    let channels = ptr::read_unaligned(ptr::addr_of!((*format).nChannels));
    let rate = ptr::read_unaligned(ptr::addr_of!((*format).nSamplesPerSec));
    let bits = ptr::read_unaligned(ptr::addr_of!((*format).wBitsPerSample));
    let extra = ptr::read_unaligned(ptr::addr_of!((*format).cbSize));
    if channels == 0 || rate == 0 {
        return Err("audio mix format is empty".into());
    }
    let tag = if tag == WAVE_FORMAT_EXTENSIBLE {
        if extra < 22 {
            return Err("extensible audio format is short".into());
        }
        let ext = format as *const WAVEFORMATEXTENSIBLE;
        let sub = ptr::read_unaligned(ptr::addr_of!((*ext).SubFormat));
        sub.Data1 as u16
    } else {
        tag
    };
    if tag != WAVE_FORMAT_PCM && tag != WAVE_FORMAT_IEEE_FLOAT {
        return Err(format!("unsupported audio format tag {tag} bits {bits}"));
    }
    Ok(Mix {
        rate,
        channels,
        tag,
        bits,
    })
}

unsafe fn init_loopback(
    device: *mut IMMDevice,
    mut client: *mut IAudioClient,
    format: *const WAVEFORMATEX,
    rate: u32,
) -> Result<*mut IAudioClient, String> {
    let mut duration: i64 = 1_000_000;
    for attempt in 0..2 {
        let hr = (*client).Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            duration,
            0,
            format,
            ptr::null(),
        );
        if hr == AUDCLNT_E_BUFFER_SIZE_NOT_ALIGNED && attempt == 0 {
            let mut frames = 0u32;
            let size_hr = (*client).GetBufferSize(&mut frames);
            (*client).Release();
            client = ptr::null_mut();
            if size_hr < 0 || frames == 0 || rate == 0 {
                return Err(format!("audio buffer align: hresult {hr:#x}"));
            }
            duration = i64::from(frames) * 10_000_000 / i64::from(rate);
            let hr = (*device).Activate(
                &IAudioClient::uuidof(),
                CLSCTX_ALL,
                ptr::null_mut(),
                &mut client as *mut *mut IAudioClient as *mut LPVOID,
            );
            if hr < 0 || client.is_null() {
                return Err(format!("Activate audio: hresult {hr:#x}"));
            }
            continue;
        }
        if hr < 0 {
            (*client).Release();
            return Err(format!("Initialize loopback: hresult {hr:#x}"));
        }
        return Ok(client);
    }
    if !client.is_null() {
        (*client).Release();
    }
    Err("loopback initialize failed".into())
}

fn copy_packet(data: *mut u8, frames: usize, flags: DWORD, audio: &Loopback) -> Result<AudioChunk, String> {
    let channels = audio.channels as usize;
    let samples = frames.saturating_mul(channels);
    if samples == 0 {
        return Err("audio packet has no samples".into());
    }
    let pcm = if flags & AUDCLNT_BUFFERFLAGS_SILENT != 0 || data.is_null() {
        vec![0i16; samples]
    } else {
        let width = match (audio.tag, audio.bits) {
            (_, 16) => 2,
            (_, 24) => 3,
            (_, 32) => 4,
            _ => return Err(format!("unsupported audio format tag {} bits {}", audio.tag, audio.bits)),
        };
        let bytes = frames.saturating_mul(channels).saturating_mul(width);
        let src = unsafe { std::slice::from_raw_parts(data, bytes) };
        decode_samples(src, samples, audio.tag, audio.bits)?
    };
    Ok(AudioChunk {
        sample_rate: audio.rate,
        channels: audio.channels,
        pcm,
    })
}

fn decode_samples(src: &[u8], samples: usize, tag: u16, bits: u16) -> Result<Vec<i16>, String> {
    let mut pcm = Vec::with_capacity(samples);
    match (tag, bits) {
        (WAVE_FORMAT_PCM, 16) => {
            if src.len() < samples * 2 {
                return Err("pcm16 buffer is short".into());
            }
            for chunk in src.chunks_exact(2).take(samples) {
                pcm.push(i16::from_le_bytes([chunk[0], chunk[1]]));
            }
        }
        (WAVE_FORMAT_PCM, 24) => {
            if src.len() < samples * 3 {
                return Err("pcm24 buffer is short".into());
            }
            for chunk in src.chunks_exact(3).take(samples) {
                let sign = if chunk[2] & 0x80 != 0 { 0xff } else { 0 };
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], sign]);
                pcm.push((value >> 8) as i16);
            }
        }
        (WAVE_FORMAT_PCM, 32) => {
            if src.len() < samples * 4 {
                return Err("pcm32 buffer is short".into());
            }
            for chunk in src.chunks_exact(4).take(samples) {
                let value = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                pcm.push((value >> 16) as i16);
            }
        }
        (WAVE_FORMAT_IEEE_FLOAT, 32) => {
            if src.len() < samples * 4 {
                return Err("float buffer is short".into());
            }
            for chunk in src.chunks_exact(4).take(samples) {
                let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                pcm.push(float_to_i16(sample));
            }
        }
        _ => return Err(format!("unsupported audio format tag {tag} bits {bits}")),
    }
    if pcm.len() != samples {
        return Err("audio conversion was short".into());
    }
    Ok(pcm)
}

fn float_to_i16(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16
}

#[cfg(test)]
mod tests {
    use super::{
        ascii_key, decode_samples, extended_virtual_key, float_to_i16, mapped_virtual_key,
        recover_step, virtual_key, Recover, STALE_LIMIT,
    };
    use winapi::um::winuser::{
        VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN,
        VK_RIGHT, VK_UP,
    };
    use std::time::Duration;

    #[test]
    fn capture_rebuilds_after_a_desktop_switch_or_a_stale_run() {
        let soon = Duration::from_millis(100);
        let late = Duration::from_secs(6);
        assert_eq!(recover_step(true, 0, false, soon, true), Recover::NewDesktop);
        assert_eq!(recover_step(false, STALE_LIMIT, false, soon, true), Recover::Stale);
        assert_eq!(recover_step(false, STALE_LIMIT - 1, false, late, true), Recover::Nothing);
        assert_eq!(recover_step(false, 0, true, late, true), Recover::RetryDuplication);
        assert_eq!(recover_step(false, 0, true, soon, true), Recover::Nothing);
        assert_eq!(recover_step(false, 0, true, late, false), Recover::Nothing);
    }

    use winapi::shared::mmreg::{WAVE_FORMAT_IEEE_FLOAT, WAVE_FORMAT_PCM};

    #[test]
    fn escape_and_function_keys_map_to_virtual_keys() {
        assert_eq!(virtual_key(0xff1b), Some(0x1b));
        assert_eq!(virtual_key(0xffbe), Some(0x70));
        assert_eq!(virtual_key(0xffc9), Some(0x7b));
        assert_eq!(virtual_key(u32::from(b'a')), None);
        assert_eq!(ascii_key(u32::from(b'1')), Some(0x31));
        assert_eq!(ascii_key(u32::from(b'2')), Some(0x32));
        assert_eq!(ascii_key(u32::from(b'0')), Some(0x30));
        assert_eq!(ascii_key(u32::from(b'a')), Some(0x41));
        assert_eq!(ascii_key(u32::from(b'A')), Some(0x41));
        assert_eq!(ascii_key(u32::from(b'!')), Some(0x31));
        assert_eq!(ascii_key(u32::from(b' ')), Some(0x20));
    }

    #[test]
    fn arrows_inject_as_extended_keys_not_numpad_digits() {
        assert_eq!(virtual_key(0xff51), Some(VK_LEFT as u16));
        assert_eq!(virtual_key(0xff52), Some(VK_UP as u16));
        assert_eq!(virtual_key(0xff53), Some(VK_RIGHT as u16));
        assert_eq!(virtual_key(0xff54), Some(VK_DOWN as u16));
        assert_eq!(virtual_key(0xff96), Some(VK_LEFT as u16));
        assert_eq!(mapped_virtual_key(0xff51), Some((VK_LEFT as u16, true)));
        assert_eq!(mapped_virtual_key(0xff54), Some((VK_DOWN as u16, true)));
        assert_eq!(mapped_virtual_key(0xff50), Some((VK_HOME as u16, true)));
        assert_eq!(mapped_virtual_key(0xff57), Some((VK_END as u16, true)));
        assert_eq!(mapped_virtual_key(0xff55), Some((VK_PRIOR as u16, true)));
        assert_eq!(mapped_virtual_key(0xff56), Some((VK_NEXT as u16, true)));
        assert_eq!(mapped_virtual_key(0xff63), Some((VK_INSERT as u16, true)));
        assert_eq!(mapped_virtual_key(0xffff), Some((VK_DELETE as u16, true)));
        assert!(extended_virtual_key(VK_LEFT as u16));
        assert!(extended_virtual_key(VK_UP as u16));
        assert!(extended_virtual_key(VK_RIGHT as u16));
        assert!(extended_virtual_key(VK_DOWN as u16));
        assert!(!extended_virtual_key(VK_RETURN as u16));
        assert!(!extended_virtual_key(0x31));
        // Main Enter is not extended; keypad Enter is.
        assert_eq!(mapped_virtual_key(0xff0d), Some((VK_RETURN as u16, false)));
        assert_eq!(mapped_virtual_key(0xff8d), Some((VK_RETURN as u16, true)));
        // Digit keys stay unextended so they cannot be confused with arrows.
        assert_eq!(mapped_virtual_key(u32::from(b'4')), Some((0x34, false)));
        assert_eq!(mapped_virtual_key(u32::from(b'8')), Some((0x38, false)));
    }

    #[test]
    fn pcm_and_float_become_i16() {
        let pcm = decode_samples(&[0x00, 0x80, 0xff, 0x7f], 2, WAVE_FORMAT_PCM, 16).unwrap();
        assert_eq!(pcm, vec![i16::MIN, i16::MAX]);
        let one = 1.0f32.to_le_bytes();
        let float = decode_samples(&one, 1, WAVE_FORMAT_IEEE_FLOAT, 32).unwrap();
        assert_eq!(float, vec![float_to_i16(1.0)]);
        assert_eq!(float_to_i16(f32::NAN), 0);
    }
}

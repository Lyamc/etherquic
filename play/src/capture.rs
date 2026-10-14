//! Choose which device supplies the host's desktop audio.

/// Where desktop audio comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DesktopAudio {
    /// Default output device, opened as a loopback input. This is the Windows path:
    /// WASAPI sets `AUDCLNT_STREAMFLAGS_LOOPBACK` only when `build_input_stream` is
    /// called on an `eRender` endpoint with that endpoint's output mix format.
    DefaultOutput,
    /// A capture device whose name contains `monitor` or `loopback` (PulseAudio / PipeWire).
    MonitorInput,
    /// First ordinary capture device. Often a microphone.
    CaptureInput,
}

/// Pick the desktop-audio source.
///
/// On Windows the default output wins even when a microphone is also present.
/// Elsewhere a monitor source wins, and a microphone is the last resort.
pub fn desktop_audio(windows: bool, output_available: bool, monitor_input: bool) -> DesktopAudio {
    if windows && output_available {
        DesktopAudio::DefaultOutput
    } else if monitor_input {
        DesktopAudio::MonitorInput
    } else {
        DesktopAudio::CaptureInput
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_loopback_uses_the_default_output_even_when_a_microphone_exists() {
        assert_eq!(desktop_audio(true, true, true), DesktopAudio::DefaultOutput);
        assert_eq!(
            desktop_audio(true, true, false),
            DesktopAudio::DefaultOutput
        );
        assert_eq!(desktop_audio(false, true, true), DesktopAudio::MonitorInput);
        assert_eq!(
            desktop_audio(false, true, false),
            DesktopAudio::CaptureInput
        );
        assert_eq!(desktop_audio(true, false, true), DesktopAudio::MonitorInput);
        assert_eq!(
            desktop_audio(true, false, false),
            DesktopAudio::CaptureInput
        );
    }
}

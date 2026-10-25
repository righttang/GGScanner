mod gesture;
pub mod key_hold;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub use linux::linux_control::LinuxControl as SystemControl;
#[cfg(target_os = "macos")]
pub use macos::macos_control::MacOSControl as SystemControl;
#[cfg(target_os = "windows")]
pub use windows::windows_control::WindowsSystemControl as SystemControl;

/// Initialize the selected native backend and fail before any game input if
/// required Linux input devices are unavailable. Windows needs no preflight.
pub fn prepared_control() -> anyhow::Result<SystemControl> {
    let control = SystemControl::new();
    #[cfg(target_os = "linux")]
    let control = {
        let mut control = control;
        control.prepare()?;
        control
    };
    Ok(control)
}

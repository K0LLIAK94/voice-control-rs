#![windows_subsystem = "windows"]

fn main() {
    if let Err(error) = voice_control::entry() {
        #[cfg(windows)]
        unsafe {
            use windows::{
                Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
                core::HSTRING,
            };
            MessageBoxW(
                None,
                &HSTRING::from(format!("{error:#}")),
                &HSTRING::from("Voice Control"),
                MB_ICONERROR | MB_OK,
            );
        }
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

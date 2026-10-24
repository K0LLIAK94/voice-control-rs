# Validation — 2026-10-24

Verified locally on Windows x64 with Rust 1.93.0 GNU:

- `cargo fmt --check` and strict Clippy: passed.
- Fourteen unit/integration checks: command boundaries/confidence/repeat protection, streaming resampling/anti-aliasing, VAD silence/clicks/plosive/quiet-profile behavior, DPI geometry and actual GDI text bounds. All passed.
- Separately enabled actual Vosk DLL regression: passed. The DLL path is canonicalized to avoid Windows dependency-resolution failure with mixed separators.
- Final release speech pipeline: all ten synthesized Russian PCM fixtures passed, including all four commands and false-positive regressions. See `speech-results.json`; these are synthetic voices, not a live-human acceptance test.
- Actual microphone plus packaged executable: received PCM16 mono 16000 Hz in 640-sample blocks; clean exit.
- Actual WinRT MediaPlayer/GSMTC fixture: Play/Pause and repeated calls preserved the requested state, see `media-session.json`. This did not test the user's specific Chrome video.
- Native GUI: listening/pause/resume, keyboard focus, close-to-tray and second-instance rejection observed. Custom SVG/ICO icons are embedded; window/taskbar/tray select the speech state. Final active-state preview was interrupted before visual confirmation.

The native input function sent Insert down/up successfully. The actual mute-state transition in the user's Discord could not be established from the available UI. Insert is a toggle, and the application explicitly displays only a local estimate. Browser UI testing was stopped when the tool could not establish the URL; a later native preview was stopped with Escape. Those UI checks were not continued by alternate input methods.

The runnable portable app was launched for the user's manual voice/Chrome/Discord acceptance test. On 2026-10-24 the user reported that the application worked and accepted publication. This is user acceptance; the automated checks above retain their stated scope.

Additional 4K UI validation: the real UI was tested in an owned, hidden test window on the actual 3840×2160 monitor with a 3840×2064 work area and 192 DPI (Windows 200%). Per-Monitor V2 was confirmed. At 100/125/75% manual zoom, the window, fonts and child buttons scaled together, remained in the work area, and zoom was persisted in a QA-only config. Reset restored the baseline. See `dpi.json`. No microphone, global input or user application was used by this harness.

No proprietary Laitis code, embedded credentials, original executable or raw extracted assemblies are distributed. The sanitized reverse-engineering report identifies its target SHA256 and exact assembly methods.

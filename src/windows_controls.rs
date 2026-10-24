//! Native controls for the media session shown in Windows Quick Settings and
//! the user's Discord Insert shortcut. No window focus changes are made.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAction {
    Play,
    Pause,
    Toggle,
}

#[cfg(windows)]
mod platform {
    use super::MediaAction;
    use std::{
        mem::size_of,
        thread,
        time::{Duration, Instant},
    };
    use windows::{
        Media::Control::{
            GlobalSystemMediaTransportControlsSession,
            GlobalSystemMediaTransportControlsSessionManager,
            GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
        },
        Win32::{
            Foundation::{CloseHandle, HANDLE},
            System::{
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
                    TH32CS_SNAPPROCESS,
                },
                WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
            },
            UI::Input::KeyboardAndMouse::{
                GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
                KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, SendInput, VK_INSERT,
            },
        },
    };

    // Use the actual operation's type instead of introducing a second version
    // of windows-future. Polling bounds failures without a lingering worker
    // that might issue a media command after the caller has timed out.
    macro_rules! wait_operation {
        ($operation:expr, $context:expr) => {{
            let operation = $operation.map_err(|error| format!("{}: {error}", $context))?;
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let status = operation.Status().map_err(|error| format!("{}: {error}", $context))?;
                // Windows.Foundation.AsyncStatus.Started = 0.
                if status.0 != 0 {
                    break operation.GetResults().map_err(|error| format!("{}: {error}", $context));
                }
                if Instant::now() >= deadline {
                    let _ = operation.Cancel();
                    break Err(format!("{}: Windows не ответила за 5 секунд", $context));
                }
                thread::sleep(Duration::from_millis(10));
            }
        }};
    }

    struct Apartment(bool);

    impl Apartment {
        fn initialize() -> Result<Self, String> {
            // SAFETY: initialize this calling thread and balance successful
            // initialization in Drop. An existing STA is left untouched.
            match unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
                Ok(()) => Ok(Self(true)),
                Err(error) if error.code().0 == 0x80010106_u32 as i32 => Ok(Self(false)),
                Err(error) => Err(format!(
                    "Не удалось инициализировать Windows Runtime: {error}"
                )),
            }
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.0 {
                // SAFETY: balances RoInitialize on the same thread.
                unsafe { RoUninitialize() };
            }
        }
    }

    fn current_session() -> Result<GlobalSystemMediaTransportControlsSession, String> {
        let manager = wait_operation!(
            GlobalSystemMediaTransportControlsSessionManager::RequestAsync(),
            "Не удалось получить медиаплеер Windows"
        )?;
        manager.GetCurrentSession().map_err(|error| {
            let message =
                "Нет активного медиаплеера Windows. Откройте видео или музыку в Chrome/плеере";
            // A null WinRT session can be represented as an error whose HRESULT
            // is S_OK; don't append the misleading "operation successful" text.
            if error.code().0 == 0 {
                format!("{message}.")
            } else {
                format!("{message}: {error}")
            }
        })
    }

    fn source_name(session: &GlobalSystemMediaTransportControlsSession) -> String {
        session
            .SourceAppUserModelId()
            .map(|name| name.to_string())
            .unwrap_or_else(|_| "медиаплеер Windows".into())
    }

    pub fn media(action: MediaAction) -> Result<String, String> {
        let _apartment = Apartment::initialize()?;
        let session = current_session()?;
        let source = source_name(&session);
        let status = session
            .GetPlaybackInfo()
            .and_then(|info| info.PlaybackStatus())
            .map_err(|error| format!("Не удалось прочитать состояние медиаплеера: {error}"))?;
        match (action, status) {
            (MediaAction::Play, PlaybackStatus::Playing) => {
                return Ok(format!("Уже воспроизводится: {source}"));
            }
            (MediaAction::Pause, PlaybackStatus::Paused | PlaybackStatus::Stopped) => {
                return Ok(format!("Уже на паузе: {source}"));
            }
            _ => {}
        }
        let (accepted, command) = match action {
            MediaAction::Play => (
                wait_operation!(session.TryPlayAsync(), "Команда воспроизведения")?,
                "играй",
            ),
            MediaAction::Pause => (
                wait_operation!(session.TryPauseAsync(), "Команда паузы")?,
                "пауза",
            ),
            MediaAction::Toggle => (
                wait_operation!(
                    session.TryTogglePlayPauseAsync(),
                    "Переключение воспроизведения"
                )?,
                "играй / пауза",
            ),
        };
        if accepted {
            Ok(format!("Команда «{command}» принята: {source}"))
        } else {
            Err(format!(
                "{source} не принял команду «{command}». Проверьте открытое видео или музыку."
            ))
        }
    }

    /// Read-only inspection; does not play or pause media.
    pub fn get_media_status() -> Result<String, String> {
        let _apartment = Apartment::initialize()?;
        let session = current_session()?;
        let status = session
            .GetPlaybackInfo()
            .and_then(|info| info.PlaybackStatus())
            .map_err(|error| format!("Не удалось прочитать состояние медиаплеера: {error}"))?;
        let state = match status {
            PlaybackStatus::Closed => "закрыт",
            PlaybackStatus::Opened => "открыт",
            PlaybackStatus::Changing => "переключается",
            PlaybackStatus::Stopped => "остановлен",
            PlaybackStatus::Playing => "воспроизводится",
            PlaybackStatus::Paused => "на паузе",
            _ => "неизвестно",
        };
        Ok(format!("{}: {state}", source_name(&session)))
    }

    struct Snapshot(HANDLE);

    impl Drop for Snapshot {
        fn drop(&mut self) {
            // SAFETY: this handle belongs to our ToolHelp snapshot only.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    fn inspect_discord_processes() -> Result<bool, String> {
        // SAFETY: requests a read-only snapshot of process metadata.
        let snapshot = Snapshot(
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
                .map_err(|error| format!("Не удалось проверить, запущен ли Discord: {error}"))?,
        );
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        // SAFETY: entry has the required size and points to valid storage.
        let first = unsafe { Process32FirstW(snapshot.0, &mut entry) };
        if let Err(error) = first {
            return Err(format!("Не удалось прочитать список процессов: {error}"));
        }
        loop {
            let length = entry
                .szExeFile
                .iter()
                .position(|value| *value == 0)
                .unwrap_or(entry.szExeFile.len());
            let name = String::from_utf16_lossy(&entry.szExeFile[..length]);
            if [
                "Discord.exe",
                "DiscordPTB.exe",
                "DiscordCanary.exe",
                "DiscordDevelopment.exe",
            ]
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
            {
                return Ok(true);
            }
            // SAFETY: snapshot remains open and entry is valid mutable storage.
            match unsafe { Process32NextW(snapshot.0, &mut entry) } {
                Ok(()) => {}
                // ERROR_NO_MORE_FILES (18) is normal iteration completion.
                Err(error) if error.code().0 == 0x80070012_u32 as i32 => return Ok(false),
                Err(error) => return Err(format!("Ошибка чтения списка процессов: {error}")),
            }
        }
    }

    pub fn discord_running() -> bool {
        inspect_discord_processes().unwrap_or(false)
    }

    /// Sends exactly one Insert press. This is a hotkey toggle, not proof of
    /// Discord's mute state: Discord's UI and physical Insert can change it too.
    pub fn discord_toggle() -> Result<String, String> {
        if !inspect_discord_processes()? {
            return Err("Discord не запущен; Insert не отправлен.".into());
        }
        // Don't turn a simultaneous Shift+Insert into paste, or interfere with
        // a physically held hotkey. Check only the current high-order bit.
        for key in [0x10, 0x11, 0x12, 0x5B, 0x5C, i32::from(VK_INSERT.0)] {
            // SAFETY: queries key state only.
            if unsafe { GetAsyncKeyState(key) } < 0 {
                return Err(
                    "Отпустите Shift, Ctrl, Alt, Win и Insert, затем повторите команду.".into(),
                );
            }
        }
        let keyboard = |flags| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VK_INSERT,
                    wScan: 0x52,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        let inputs = [
            keyboard(KEYEVENTF_EXTENDEDKEY),
            keyboard(KEYEVENTF_EXTENDEDKEY | KEYEVENTF_KEYUP),
        ];
        // SAFETY: both input records are initialized keyboard events. SendInput
        // inserts key-down/up consecutively without moving window focus.
        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
        if sent != inputs.len() as u32 {
            if sent == 1 {
                // Release Insert if Windows accepted only its key-down event.
                let _ = unsafe { SendInput(&inputs[1..], size_of::<INPUT>() as i32) };
            }
            return Err(format!(
                "Windows приняла {sent}/2 событий Insert. Проверьте, что Discord и приложение работают с одинаковыми правами."
            ));
        }
        Ok("Insert отправлен. Discord должен обработать настроенную горячую клавишу.".into())
    }
}

#[cfg(windows)]
pub use platform::{discord_running, discord_toggle, get_media_status, media};

#[cfg(not(windows))]
pub fn media(_action: MediaAction) -> Result<String, String> {
    Err("Управление медиаплеером доступно в Windows.".into())
}
#[cfg(not(windows))]
pub fn get_media_status() -> Result<String, String> {
    Err("Управление медиаплеером доступно в Windows.".into())
}
#[cfg(not(windows))]
pub fn discord_running() -> bool {
    false
}
#[cfg(not(windows))]
pub fn discord_toggle() -> Result<String, String> {
    Err("Горячая клавиша Discord доступна в Windows.".into())
}

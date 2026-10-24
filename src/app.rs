use crate::{
    audio::Microphone,
    commands::{Command, Dispatcher, Recognition},
    config::Config,
    recognizer::Recognizer,
    windows_controls::{self, MediaAction},
};
use crossbeam_channel::Receiver;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::Ordering},
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct AppState {
    pub status: String,
    pub microphone: String,
    pub level: u32,
    pub last_phrase: String,
    pub last_action: String,
    pub error: String,
    pub paused: bool,
    pub ready: bool,
    pub speaking: bool,
}
pub enum Control {
    TogglePause,
    Reload,
    ResyncDiscord,
    Quit,
}
pub type Shared = Arc<Mutex<AppState>>;

pub fn change(shared: &Shared, f: impl FnOnce(&mut AppState)) {
    if let Ok(mut state) = shared.lock() {
        f(&mut state);
    }
}

pub fn log(path: &Path, message: &str) {
    use std::io::Write;
    // Deliberately record actions/errors, never arbitrary microphone transcripts or audio.
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() > 1_000_000 {
            let _ = std::fs::rename(path, path.with_extension("previous.log"));
        }
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = writeln!(file, "[{timestamp}] {message}");
    }
}

pub struct Actions {
    pub discord_estimate: Option<bool>,
    dispatcher: Dispatcher,
    dry_run: bool,
}
impl Actions {
    pub fn new(config: &Config, dry_run: bool) -> Self {
        Self {
            discord_estimate: None,
            dispatcher: Dispatcher::new(config.cooldown_ms),
            dry_run,
        }
    }
    pub fn run(&mut self, command: Command) -> Result<String, String> {
        if !self.dispatcher.allow(command, Instant::now()) {
            return Ok("Повтор команды пропущен".into());
        }
        if self.dry_run {
            return Ok(format!("Проверка без действий: {}", command.label()));
        }
        match command {
            Command::Play => windows_controls::media(MediaAction::Play),
            Command::Pause => windows_controls::media(MediaAction::Pause),
            Command::Mute | Command::Unmute => {
                let requested = command == Command::Mute;
                if self.discord_estimate == Some(requested) {
                    return Ok(
                        "Команда уже отправлена. После ручного Insert нажмите «Сбросить Discord»."
                            .into(),
                    );
                }
                windows_controls::discord_toggle()?;
                self.discord_estimate = Some(requested);
                Ok(format!(
                    "Insert отправлен · ожидаемое состояние: {} (оценка)",
                    if requested {
                        "мут"
                    } else {
                        "без мута"
                    }
                ))
            }
        }
    }
}

fn observe(
    recognition: Recognition,
    config: &Config,
    actions: &mut Actions,
    shared: &Shared,
    log_path: &Path,
) {
    if recognition.text.is_empty() {
        return;
    }
    let command = recognition.command(config);
    change(shared, |s| {
        s.last_phrase = if let Some(command) = command {
            command.label().into()
        } else {
            "Речь не совпала с командой".into()
        }
    });
    if let Some(command) = command {
        match actions.run(command) {
            Ok(message) => {
                log(log_path, &message);
                change(shared, |s| {
                    s.last_action = message;
                    s.error.clear();
                });
            }
            Err(error) => {
                log(log_path, &error);
                change(shared, |s| s.error = error);
            }
        }
    }
}

enum SpeechInput {
    Audio(u64, Vec<i16>),
    ResetDiscord,
}
struct SpeechWorker {
    tx: crossbeam_channel::Sender<SpeechInput>,
    epoch: Arc<std::sync::atomic::AtomicU64>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}
impl SpeechWorker {
    fn start(
        config: Config,
        shared: Shared,
        log_path: PathBuf,
        dry_run: bool,
    ) -> anyhow::Result<Self> {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let (ready_tx, ready_rx) = crossbeam_channel::bounded(1);
        let epoch = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let thread_epoch = epoch.clone();
        let thread_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut recognizer = match Recognizer::new(
                &config.library_path,
                &config.model_path,
                &config.grammar(),
            ) {
                Ok(recognizer) => {
                    let _ = ready_tx.send(Ok(()));
                    recognizer
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(format!("{error:#}")));
                    return;
                }
            };
            let mut actions = Actions::new(&config, dry_run);
            while !thread_stop.load(Ordering::Acquire) {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(SpeechInput::ResetDiscord) => {
                        actions.discord_estimate = None;
                        change(&shared, |s| {
                            s.last_action = "Отслеживание Discord сброшено".into()
                        });
                    }
                    Ok(SpeechInput::Audio(utterance_epoch, samples)) => {
                        if utterance_epoch != thread_epoch.load(Ordering::Acquire) {
                            continue;
                        }
                        let result = recognizer.utterance(&samples);
                        if thread_stop.load(Ordering::Acquire)
                            || utterance_epoch != thread_epoch.load(Ordering::Acquire)
                        {
                            continue;
                        }
                        match result {
                            Ok(result) => {
                                observe(result, &config, &mut actions, &shared, &log_path)
                            }
                            Err(error) => {
                                let error = format!("{error:#}");
                                log(&log_path, &error);
                                change(&shared, |s| s.error = error);
                            }
                        }
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                }
            }
        });
        let worker = Self {
            tx,
            epoch,
            stop,
            handle: Some(handle),
        };
        ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("Поток распознавания завершился"))?
            .map_err(anyhow::Error::msg)?;
        Ok(worker)
    }
    fn discard_pending(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
    fn audio(&self, samples: Vec<i16>) -> Result<(), String> {
        self.tx
            .try_send(SpeechInput::Audio(
                self.epoch.load(Ordering::Acquire),
                samples,
            ))
            .map_err(|_| "Очередь распознавания заполнена; фраза пропущена".into())
    }
}
impl Drop for SpeechWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.discard_pending();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub fn worker(
    shared: Shared,
    controls: Receiver<Control>,
    root: PathBuf,
    config_path: PathBuf,
    log_path: PathBuf,
    dry_run: bool,
) {
    let mut paused = false;
    'restart: loop {
        change(&shared, |s| {
            s.ready = false;
            s.speaking = false;
            s.paused = paused;
            s.level = 0;
            s.status = "Загрузка распознавателя…".into();
        });
        let setup = || -> anyhow::Result<_> {
            let mut config = Config::load(&config_path)?;
            config.resolve(&root);
            let speech =
                SpeechWorker::start(config.clone(), shared.clone(), log_path.clone(), dry_run)?;
            Ok((config, speech))
        };
        let (config, speech) = match setup() {
            Ok(ready) => ready,
            Err(error) => {
                let error = format!("{error:#}");
                log(&log_path, &error);
                change(&shared, |s| {
                    s.error = error;
                    s.status = "Не удалось начать прослушивание".into();
                });
                loop {
                    match controls.recv() {
                        Ok(Control::Quit) | Err(_) => return,
                        Ok(Control::Reload) => continue 'restart,
                        Ok(Control::TogglePause) => {
                            paused = !paused;
                            change(&shared, |s| {
                                s.paused = paused;
                                s.status = if paused {
                                    "Прослушивание приостановлено"
                                } else {
                                    "Не удалось начать прослушивание"
                                }
                                .into();
                            });
                            if !paused {
                                continue 'restart;
                            }
                        }
                        _ => {}
                    }
                }
            }
        };
        let mut microphone: Option<Microphone> = None;
        let mut last_audio = Instant::now();
        let mut vad = crate::vad::Vad::default();
        loop {
            while let Ok(control) = controls.try_recv() {
                match control {
                    Control::Quit => return,
                    Control::Reload => continue 'restart,
                    Control::TogglePause => {
                        paused = !paused;
                        speech.discard_pending();
                        if paused {
                            microphone = None;
                        }
                        // Discard unfinished speech when resuming: never join a phrase across pause.
                        if !paused {
                            vad = crate::vad::Vad::default();
                        }
                    }
                    Control::ResyncDiscord => {
                        let _ = speech.tx.try_send(SpeechInput::ResetDiscord);
                    }
                }
            }
            if paused {
                change(&shared, |s| {
                    s.paused = true;
                    s.ready = true;
                    s.level = 0;
                    s.speaking = false;
                    s.status = "Прослушивание приостановлено".into();
                });
                std::thread::sleep(Duration::from_millis(40));
                continue;
            }
            if microphone.is_none() {
                match Microphone::open(&config.microphone) {
                    Ok(mic) => {
                        change(&shared, |s| {
                            s.microphone = mic.description.clone();
                            s.status = "Слушаю команды".into();
                            s.ready = true;
                            s.paused = false;
                            s.error.clear();
                        });
                        log(&log_path, &format!("Микрофон включён: {}", mic.description));
                        microphone = Some(mic);
                        last_audio = Instant::now();
                    }
                    Err(error) => {
                        let error = format!("{error:#}");
                        log(&log_path, &error);
                        change(&shared, |s| {
                            s.error = error;
                            s.ready = false;
                            s.status = "Ожидаю микрофон…".into();
                        });
                        // Keep controls responsive while waiting to reconnect.
                        for _ in 0..50 {
                            if !controls.is_empty() {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(40));
                        }
                        continue;
                    }
                }
            }
            let mic = microphone.as_ref().unwrap();
            change(&shared, |s| s.level = mic.level.load(Ordering::Relaxed));
            let failure = mic.errors.try_recv().ok().or_else(|| {
                if mic.dropped.load(Ordering::Relaxed) > 0 {
                    Some("Обработка звука не успевает: сбрасываю незавершённую фразу".into())
                } else if last_audio.elapsed() > Duration::from_secs(3) {
                    Some("Микрофон перестал передавать звук; переподключение".into())
                } else {
                    None
                }
            });
            if let Some(error) = failure {
                log(&log_path, &error);
                change(&shared, |s| {
                    s.error = error;
                    s.ready = false;
                });
                continue 'restart;
            }
            if let Ok(chunk) = mic.chunks.recv_timeout(Duration::from_millis(40)) {
                last_audio = Instant::now();
                if let Some(utterance) = vad.push(&chunk) {
                    if let Err(error) = speech.audio(utterance) {
                        log(&log_path, &error);
                        change(&shared, |s| s.error = error);
                    }
                }
                change(&shared, |s| s.speaking = vad.speaking());
            }
        }
    }
}

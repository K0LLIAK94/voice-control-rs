pub mod app;
pub mod audio;
pub mod commands;
pub mod config;
mod instance;
pub mod recognizer;
pub mod ui;
pub mod vad;
pub mod windows_controls;
pub mod winmm;

use anyhow::{Context, Result, bail};
use config::Config;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

fn base_path() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let folder = exe.parent().context("Нет папки приложения")?;
    // Cargo build outputs live two directories beneath the repository root.
    if folder
        .file_name()
        .is_some_and(|n| n == "debug" || n == "release")
    {
        if let Some(root) = folder.parent().and_then(Path::parent) {
            if root.join("Cargo.toml").is_file() {
                return Ok(root.into());
            }
        }
    }
    Ok(folder.into())
}

fn recognize_file(path: &Path, config: &Config) -> Result<Vec<serde_json::Value>> {
    let mut reader = hound::WavReader::open(path).context("Не удалось открыть WAV")?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.sample_rate == 0 {
        bail!("Некорректный формат WAV");
    }
    let samples: Vec<f32> = if spec.sample_format == hound::SampleFormat::Float {
        reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()?
    } else {
        let scale = (1_u64 << (spec.bits_per_sample - 1)) as f32;
        reader
            .samples::<i32>()
            .map(|s| s.map(|s| s as f32 / scale))
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut resampler = audio::Resampler::new(spec.sample_rate, spec.channels as usize);
    let mut pcm = Vec::new();
    resampler.push(&samples, |s| {
        pcm.push((s.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16)
    });
    let mut recognizer =
        recognizer::Recognizer::new(&config.library_path, &config.model_path, &config.grammar())?;
    let mut events = vec![];
    let mut record = |r: commands::Recognition| {
        if !r.text.is_empty() {
            events.push(serde_json::json!({"text":r.text,"confidence":r.confidence(),"command":r.command(config)}));
        }
    };
    let mut vad = vad::Vad::default();
    for part in pcm.chunks(audio::CHUNK) {
        if let Some(utterance) = vad.push(part) {
            record(recognizer.utterance(&utterance)?);
        }
    }
    // Synthesis fixtures may omit trailing silence; real capture ends with 520ms silence.
    if let Some(utterance) = vad.finish() {
        record(recognizer.utterance(&utterance)?);
    }
    Ok(events)
}

pub fn entry() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = base_path()?;
    if let Some(i) = args.iter().position(|s| s == "--root") {
        root = PathBuf::from(args.get(i + 1).context("--root требует путь")?);
        args.drain(i..=i + 1);
    }
    let mut config_path = root.join("config.toml");
    if let Some(i) = args.iter().position(|s| s == "--config") {
        config_path = PathBuf::from(args.get(i + 1).context("--config требует путь")?);
        args.drain(i..=i + 1);
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!(
            "Voice Control\n\nБез аргументов: окно и значок в трее; прослушивание включено.\n--list-devices          Микрофоны\n--status                Состояние медиасеанса и наличие Discord (без действий)\n--self-check            Проверка модели, DLL и микрофона (без действий)\n--recognize-file WAV --json  Распознать WAV без выполнения команд\n--console [--dry-run] [--run-for SECONDS]  Слушать из консоли\n--root DIR --config FILE  Пути к ресурсам и настройкам\n"
        );
        return Ok(());
    }
    if args == ["--list-devices"] {
        for name in audio::list()? {
            println!("{name}");
        }
        return Ok(());
    }
    if args == ["--status"] {
        println!(
            "{}",
            serde_json::json!({"media":windows_controls::get_media_status(),"discord_running":windows_controls::discord_running()})
        );
        return Ok(());
    }
    let mut config = Config::load(&config_path)?;
    config.resolve(&root);
    if args == ["--self-check"] {
        let _recognizer = recognizer::Recognizer::new(
            &config.library_path,
            &config.model_path,
            &config.grammar(),
        )?;
        let mic = audio::Microphone::open(&config.microphone)?;
        let chunk = mic
            .chunks
            .recv_timeout(std::time::Duration::from_secs(3))
            .context("Микрофон не передаёт звук")?;
        println!(
            "{}",
            serde_json::json!({"ok":true,"microphone":mic.description,"pcm_rate":audio::RATE,"chunk_samples":chunk.len(),"discord_running":windows_controls::discord_running()})
        );
        return Ok(());
    }
    if let Some(i) = args.iter().position(|a| a == "--recognize-file") {
        let path = PathBuf::from(
            args.get(i + 1)
                .context("Укажите WAV после --recognize-file")?,
        );
        if args.iter().any(|s| {
            !["--recognize-file", "--json"].contains(&s.as_str()) && s != &path.to_string_lossy()
        }) {
            bail!("Неизвестные параметры");
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&recognize_file(&path, &config)?)?
        );
        return Ok(());
    }
    let console = args.iter().any(|a| a == "--console");
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let duration: Option<u64> = args
        .iter()
        .position(|a| a == "--run-for")
        .map(|i| -> Result<_> {
            Ok(args
                .get(i + 1)
                .context("--run-for требует секунды")?
                .parse()?)
        })
        .transpose()?;
    for (i, arg) in args.iter().enumerate() {
        if !["--console", "--dry-run", "--run-for"].contains(&arg.as_str())
            && (i == 0 || args[i - 1] != "--run-for")
        {
            bail!("Неизвестный аргумент: {arg}");
        }
    }
    let log_path = root.join("voice-control.log");
    let Some(_instance) = instance::Instance::acquire()? else {
        return Ok(());
    };
    let shared = Arc::new(Mutex::new(app::AppState::default()));
    let (tx, rx) = crossbeam_channel::unbounded();
    let shared_worker = shared.clone();
    let log_worker = log_path.clone();
    let ui_config = config_path.clone();
    let worker = std::thread::spawn(move || {
        app::worker(shared_worker, rx, root, config_path, log_worker, dry_run)
    });
    if console {
        let started = std::time::Instant::now();
        let mut previous = String::new();
        loop {
            if let Ok(state) = shared.lock() {
                let status = format!("{} | {} | {}", state.status, state.last_action, state.error);
                if status != previous {
                    println!("{status}");
                    previous = status;
                }
            }
            if duration.is_some_and(|d| started.elapsed().as_secs() >= d) || worker.is_finished() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
    } else {
        // UI zoom is independent of the speech worker's model and command settings.
        let result = ui::run(
            shared,
            tx.clone(),
            ui_config,
            log_path,
            config.ui_scale_percent,
        );
        let _ = tx.send(app::Control::Quit);
        let _ = worker.join();
        return result;
    }
    let _ = tx.send(app::Control::Quit);
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("Поток распознавания завершился с ошибкой"))?;
    Ok(())
}

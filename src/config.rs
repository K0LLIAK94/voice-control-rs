use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Empty means the Windows default microphone.
    pub microphone: String,
    pub model_path: PathBuf,
    pub library_path: PathBuf,
    pub min_confidence: f32,
    pub cooldown_ms: u64,
    /// Interface zoom, independent of the monitor's Windows DPI setting.
    pub ui_scale_percent: u32,
    /// Recognition-only phrases keep negations and similar sounding words out of command routing.
    pub ignored_phrases: Vec<String>,
    pub commands: Commands,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Commands {
    pub play: Vec<String>,
    pub pause: Vec<String>,
    pub mute: Vec<String>,
    pub unmute: Vec<String>,
}

impl Default for Commands {
    fn default() -> Self {
        Self {
            play: vec!["играй".into(), "продолжи".into()],
            pause: vec!["пауза".into(), "поставь на паузу".into()],
            mute: vec!["мут".into(), "выключи микрофон".into()],
            // The Russian acoustic model spells the gaming term as two words.
            unmute: vec!["раз мут".into(), "включи микрофон".into()],
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            microphone: String::new(),
            model_path: "models/vosk-model-small-ru-0.22".into(),
            library_path: "runtime/libvosk.dll".into(),
            min_confidence: 0.65,
            cooldown_ms: 900,
            ui_scale_percent: 100,
            ignored_phrases: [
                "мутант",
                "не ставь на паузу",
                "не играй",
                "не пауза",
                "не мут",
                "не раз мут",
                "не включай микрофон",
                "не выключай микрофон",
                "не надо",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            commands: Commands::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, toml::to_string_pretty(&Self::default())?)?;
        }
        let value: Self = toml::from_str(&std::fs::read_to_string(path)?)
            .with_context(|| format!("Ошибка настроек {}", path.display()))?;
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> Result<()> {
        if !(75..=200).contains(&self.ui_scale_percent) {
            bail!("ui_scale_percent: допустимо 75..200");
        }
        if !self.min_confidence.is_finite() || !(0.0..=1.0).contains(&self.min_confidence) {
            bail!("min_confidence должен быть от 0 до 1");
        }
        if !(100..=10_000).contains(&self.cooldown_ms) {
            bail!("cooldown_ms: допустимо 100..10000");
        }
        let groups = [
            &self.commands.play,
            &self.commands.pause,
            &self.commands.mute,
            &self.commands.unmute,
        ];
        let mut seen = std::collections::HashSet::new();
        for group in groups {
            if group.is_empty() {
                bail!("Список команд не может быть пустым");
            }
            for text in group {
                let text = crate::commands::normalize(text);
                if text.is_empty() || text.contains("[unk]") || !seen.insert(text.clone()) {
                    bail!("Пустая или повторяющаяся команда: {text}");
                }
            }
        }
        Ok(())
    }

    pub fn grammar(&self) -> Vec<String> {
        let c = &self.commands;
        c.play
            .iter()
            .chain(&c.pause)
            .chain(&c.mute)
            .chain(&c.unmute)
            .chain(&self.ignored_phrases)
            .map(|s| crate::commands::normalize(s))
            .chain(std::iter::once("[unk]".into()))
            .collect()
    }

    pub fn resolve(&mut self, root: &Path) {
        if self.model_path.is_relative() {
            self.model_path = root.join(&self.model_path);
        }
        if self.library_path.is_relative() {
            self.library_path = root.join(&self.library_path);
        }
    }
}

pub fn save_ui_scale(path: &Path, percent: u32) -> Result<()> {
    if !(75..=200).contains(&percent) {
        bail!("Масштаб интерфейса должен быть от 75 до 200%");
    }
    let mut document = std::fs::read_to_string(path)?.parse::<toml_edit::DocumentMut>()?;
    document["ui_scale_percent"] = toml_edit::value(i64::from(percent));
    let temporary = path.with_extension(format!("ui-{}.tmp", std::process::id()));
    std::fs::write(&temporary, document.to_string())
        .context("Не удалось сохранить масштаб интерфейса")?;
    std::fs::rename(&temporary, path).context("Не удалось обновить настройки масштаба")
}

use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Play,
    Pause,
    Mute,
    Unmute,
}

pub fn normalize(text: &str) -> String {
    text.trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | ',' | '!' | '?' | '…'))
        .to_lowercase()
        .replace('ё', "е")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn parse(text: &str, config: &Config) -> Option<Command> {
    let text = normalize(text);
    // Match whole utterances. In particular, 'размут' must never match 'мут'.
    let text = if text == "размут" {
        "раз мут"
    } else {
        &text
    };
    let c = &config.commands;
    [
        (&c.play, Command::Play),
        (&c.pause, Command::Pause),
        (&c.mute, Command::Mute),
        (&c.unmute, Command::Unmute),
    ]
    .into_iter()
    .find_map(|(words, command)| {
        words
            .iter()
            .any(|s| normalize(s) == text)
            .then_some(command)
    })
}

#[derive(Debug, Deserialize)]
pub struct Word {
    pub conf: f32,
}
#[derive(Debug, Deserialize)]
pub struct Recognition {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub result: Vec<Word>,
}
impl Recognition {
    pub fn confidence(&self) -> f32 {
        self.result
            .iter()
            .map(|w| w.conf)
            .reduce(f32::min)
            .unwrap_or(0.0)
    }
    pub fn command(&self, config: &Config) -> Option<Command> {
        if self.confidence() < config.min_confidence {
            return None;
        }
        parse(&self.text, config)
    }
}

pub struct Dispatcher {
    last: Option<(Command, Instant)>,
    cooldown: Duration,
}
impl Dispatcher {
    pub fn new(milliseconds: u64) -> Self {
        Self {
            last: None,
            cooldown: Duration::from_millis(milliseconds),
        }
    }
    pub fn allow(&mut self, command: Command, now: Instant) -> bool {
        if self.last.is_some_and(|(last, at)| {
            last == command && now.saturating_duration_since(at) < self.cooldown
        }) {
            return false;
        }
        self.last = Some((command, now));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commands_are_whole_utterances_and_unmute_is_not_mute() {
        let config = Config::default();
        assert_eq!(parse("  ИГРАЙ  ", &config), Some(Command::Play));
        assert_eq!(parse("размут", &config), Some(Command::Unmute));
        assert_eq!(parse("раз мут", &config), Some(Command::Unmute));
        assert_eq!(parse("мут", &config), Some(Command::Mute));
        for speech in [
            "пожалуйста не ставь на паузу",
            "пауза мут",
            "мутант",
            "сегодня играй хорошо",
            "[unk]",
        ] {
            assert_eq!(parse(speech, &config), None, "{speech}");
        }
    }
    #[test]
    fn low_confidence_and_empty_recognition_are_ignored() {
        let config = Config::default();
        for json in [
            r#"{"text":"мут"}"#,
            r#"{"text":"мут","result":[{"conf":0.4}]}"#,
            r#"{"text":"мут","result":[{"conf":0.99},{"conf":0.4}]}"#,
        ] {
            let r: Recognition = serde_json::from_str(json).unwrap();
            assert_eq!(r.command(&config), None);
        }
    }
    #[test]
    fn duplicate_filter_does_not_block_opposite_commands() {
        let mut dispatcher = Dispatcher::new(900);
        let now = Instant::now();
        assert!(dispatcher.allow(Command::Mute, now));
        assert!(!dispatcher.allow(Command::Mute, now + Duration::from_millis(100)));
        assert!(dispatcher.allow(Command::Unmute, now + Duration::from_millis(200)));
        assert!(dispatcher.allow(Command::Mute, now + Duration::from_millis(300)));
        assert!(dispatcher.allow(Command::Mute, now + Duration::from_millis(1300)));
    }
}

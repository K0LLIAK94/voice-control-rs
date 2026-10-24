//! Segmentation reconstructed from Laitis's HH / 4G / tG / WG state machine.
//! Retains its 200ms pre-roll, candidate/final silence windows and adaptive
//! profiles. The meter deliberately uses absolute full PCM16 samples instead
//! of Laitis's positive, high-byte-only meter. Its own TTS/output-volume gates
//! are omitted: this application does not synthesize speech.
use std::collections::VecDeque;

const FLOOR: [f32; 5] = [0.0, 1.0, 4.0, 6.0, 10.0];
const RISE: [f32; 5] = [0.5, 2.0, 3.0, 5.0, 15.0];
const PRE_ROLL_CHUNKS: usize = 5;
const PREPARING_BYTES: usize = 5333;
const QUIET_BYTES: usize = 64_000;
const MAX_SAMPLES: usize = 160_000;

enum Phase {
    Waiting,
    Preparing { bytes: usize, silence: usize },
    Recording { silence: usize },
}

pub struct Vad {
    phase: Phase,
    preroll: VecDeque<Vec<i16>>,
    recording: Vec<i16>,
    previous_level: f32,
    start_level: f32,
    profile: usize,
    quiet_bytes: usize,
}

impl Default for Vad {
    fn default() -> Self {
        Self {
            phase: Phase::Waiting,
            preroll: VecDeque::new(),
            recording: vec![],
            previous_level: 1.0,
            start_level: 0.0,
            profile: 0,
            quiet_bytes: 0,
        }
    }
}

impl Vad {
    pub fn speaking(&self) -> bool {
        !matches!(self.phase, Phase::Waiting)
    }
    fn remember(&mut self, samples: &[i16]) {
        self.preroll.push_back(samples.into());
        if self.preroll.len() > PRE_ROLL_CHUNKS {
            self.preroll.pop_front();
        }
    }

    fn raise_profile(&mut self) {
        self.profile = (self.profile + 1).min(FLOOR.len() - 1);
        self.quiet_bytes = 0;
    }

    fn adapt_to_quiet(&mut self, level: f32, bytes: usize) {
        if level < FLOOR[self.profile] / 2.0 {
            self.quiet_bytes += bytes;
            if self.quiet_bytes > QUIET_BYTES {
                self.profile = self.profile.saturating_sub(1);
                self.quiet_bytes = 0;
            }
        } else {
            self.quiet_bytes = 0;
        }
    }

    /// Accept one logical 40ms/640-sample capture block.
    pub fn push(&mut self, samples: &[i16]) -> Option<Vec<i16>> {
        if samples.is_empty() {
            return None;
        }
        let level = samples
            .iter()
            .map(|sample| (*sample as f32).abs())
            .fold(0.0, f32::max)
            / 327.0;
        let floor = FLOOR[self.profile];
        let rise = RISE[self.profile];
        let onset = level > floor && level > self.previous_level + rise;
        self.previous_level = level;
        // Original PG(capacity=4) also retains five blocks due to its > check.
        self.remember(samples);
        match &mut self.phase {
            Phase::Waiting => {
                if onset {
                    if level > (floor + rise) * 6.0 {
                        self.raise_profile();
                    }
                    self.start_level = level;
                    self.recording = self.preroll.drain(..).flatten().collect();
                    self.phase = Phase::Preparing {
                        bytes: 0,
                        silence: 0,
                    };
                }
            }
            Phase::Preparing { bytes, silence } => {
                self.recording.extend_from_slice(samples);
                *bytes += std::mem::size_of_val(samples);
                // A quieter voiced frame still confirms the candidate. Only
                // falling below the noise floor counts as preparation silence.
                if level <= floor {
                    *silence += 1;
                } else {
                    *silence = 0;
                }
                if *silence >= 3 {
                    self.phase = Phase::Waiting;
                    self.recording.clear();
                    self.start_level = 0.0;
                } else if *bytes > PREPARING_BYTES {
                    self.phase = Phase::Recording { silence: 0 };
                    // WG starts a fresh tail once its candidate is accepted.
                    self.preroll.clear();
                }
            }
            Phase::Recording { silence } => {
                self.recording.extend_from_slice(samples);
                if level < self.start_level && !onset {
                    *silence += 1;
                } else {
                    *silence = 0;
                }
                let too_long = self.recording.len() >= MAX_SAMPLES;
                if *silence >= 13 || too_long {
                    self.phase = Phase::Waiting;
                    self.start_level = 0.1;
                    if too_long {
                        self.raise_profile();
                    }
                    return Some(std::mem::take(&mut self.recording));
                }
            }
        }
        self.adapt_to_quiet(level, std::mem::size_of_val(samples));
        None
    }

    pub fn finish(&mut self) -> Option<Vec<i16>> {
        let valid = matches!(self.phase, Phase::Recording { .. });
        self.phase = Phase::Waiting;
        self.preroll.clear();
        self.previous_level = 1.0;
        self.start_level = 0.0;
        self.quiet_bytes = 0;
        if valid {
            Some(std::mem::take(&mut self.recording))
        } else {
            self.recording.clear();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_does_not_form_an_utterance_and_clicks_are_rejected() {
        let mut vad = Vad::default();
        for _ in 0..100 {
            assert!(vad.push(&[0; 640]).is_none());
        }
        vad.push(&[10000; 640]);
        for _ in 0..20 {
            assert!(vad.push(&[0; 640]).is_none());
        }
        assert!(vad.finish().is_none());
    }
    #[test]
    fn preroll_speech_and_trailing_silence_are_kept() {
        let mut vad = Vad::default();
        for _ in 0..8 {
            vad.push(&[0; 640]);
        }
        for _ in 0..10 {
            assert!(vad.push(&[10000; 640]).is_none());
        }
        for _ in 0..12 {
            assert!(vad.push(&[0; 640]).is_none());
        }
        let result = vad.push(&[0; 640]).unwrap();
        assert_eq!(result.len(), (4 + 10 + 13) * 640);
        assert_eq!(&result[..4 * 640], &[0; 4 * 640]);
    }
    #[test]
    fn quieter_voice_after_a_plosive_still_confirms_the_candidate() {
        let mut vad = Vad::default();
        vad.push(&[10000; 640]);
        for _ in 0..5 {
            assert!(vad.push(&[4000; 640]).is_none());
        }
        // The former Preparing check discarded these energetic voiced frames.
        assert!(matches!(vad.phase, Phase::Recording { .. }));
        assert_eq!(vad.finish().unwrap().len(), 6 * 640);
    }
    #[test]
    fn repeated_clicks_adapt_the_floor_without_emitting_commands() {
        let mut vad = Vad::default();
        for _ in 0..4 {
            assert!(vad.push(&[32700; 640]).is_none());
            for _ in 0..3 {
                assert!(vad.push(&[0; 640]).is_none());
            }
        }
        assert_eq!(vad.profile, 4);
        // Noise below the learned floor must not activate or lower that floor.
        for _ in 0..100 {
            assert!(vad.push(&[2600; 640]).is_none());
        }
        assert!(matches!(vad.phase, Phase::Waiting));
        assert_eq!(vad.profile, 4);
        assert!(vad.finish().is_none());
    }
    #[test]
    fn quiet_room_reduces_an_elevated_noise_profile() {
        let mut vad = Vad {
            profile: 2,
            ..Vad::default()
        };
        for _ in 0..50 {
            vad.push(&[0; 640]);
        }
        assert_eq!(vad.profile, 2);
        vad.push(&[0; 640]);
        assert_eq!(vad.profile, 1);
    }
}

// Native WinMM capture retains the original Laitis audio contract.
pub use crate::winmm::{Microphone, list};
pub const RATE: u32 = 16_000;
pub const CHUNK: usize = 640;
// Streaming mono conversion and low-pass FIR decimation. State is retained across callbacks.
// The low-pass avoids aliasing when a 48kHz microphone feeds a 16kHz recognizer.
pub struct Resampler {
    channels: usize,
    rate: u32,
    phase: u64,
    history: [f32; 63],
    taps: [f32; 63],
    cursor: usize,
    frame: Vec<f32>,
}
impl Resampler {
    pub fn new(rate: u32, channels: usize) -> Self {
        assert!(rate > 0 && channels > 0);
        let cutoff = 0.45 * (RATE as f32 / rate as f32).min(1.0);
        let mut taps = [0.0; 63];
        for (i, tap) in taps.iter_mut().enumerate() {
            let x = i as f32 - 31.0;
            let sinc = if x == 0.0 {
                2.0 * cutoff
            } else {
                (2.0 * std::f32::consts::PI * cutoff * x).sin() / (std::f32::consts::PI * x)
            };
            *tap = sinc * (0.54 - 0.46 * (2.0 * std::f32::consts::PI * i as f32 / 62.0).cos());
        }
        let sum: f32 = taps.iter().sum();
        for tap in &mut taps {
            *tap /= sum;
        }
        Self {
            channels,
            rate,
            phase: 0,
            history: [0.0; 63],
            taps,
            cursor: 0,
            frame: Vec::with_capacity(channels),
        }
    }
    pub fn push(&mut self, input: &[f32], mut output: impl FnMut(f32)) {
        for &sample in input {
            self.frame
                .push(if sample.is_finite() { sample } else { 0.0 });
            if self.frame.len() != self.channels {
                continue;
            }
            let mono = self.frame.iter().sum::<f32>() / self.channels as f32;
            self.frame.clear();
            self.history[self.cursor] = mono;
            self.cursor = (self.cursor + 1) % 63;
            self.phase += RATE as u64;
            while self.phase >= self.rate as u64 {
                self.phase -= self.rate as u64;
                let sample = self
                    .taps
                    .iter()
                    .enumerate()
                    .map(|(i, t)| t * self.history[(self.cursor + i) % 63])
                    .sum();
                output(sample);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stereo_resampling_is_chunk_boundary_independent() {
        let input: Vec<f32> = (0..96_000).map(|n| (n as f32 * 0.05).sin()).collect();
        let mut whole = Resampler::new(48_000, 2);
        let mut a = vec![];
        whole.push(&input, |s| a.push(s));
        let mut split = Resampler::new(48_000, 2);
        let mut b = vec![];
        for part in input.chunks(137) {
            split.push(part, |s| b.push(s));
        }
        assert_eq!(a.len(), 16_000);
        assert_eq!(a, b);
    }
    #[test]
    fn lowpass_rejects_aliasing_above_recognizer_nyquist() {
        let amplitude = |hz: f32| {
            let input: Vec<f32> = (0..48_000)
                .map(|n| (2.0 * std::f32::consts::PI * hz * n as f32 / 48_000.0).sin())
                .collect();
            let mut resampler = Resampler::new(48_000, 1);
            let mut out = vec![];
            resampler.push(&input, |s| out.push(s));
            (out[100..].iter().map(|s| s * s).sum::<f32>() / (out.len() - 100) as f32).sqrt()
        };
        assert!(amplitude(1000.0) > 0.6);
        assert!(amplitude(12_000.0) < 0.01);
    }
}

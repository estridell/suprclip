//! The "saved!" ding: a tiny synthesized two-note chime, played through PipeWire.

use std::path::Path;
use std::process::{Command, Stdio};

const RATE: u32 = 48_000;

/// Renders the built-in ding as a 16-bit mono WAV.
pub fn ding_wav() -> Vec<u8> {
    let len = (RATE as f32 * 0.45) as usize;
    // (frequency, start offset in seconds): a quick rising fifth.
    let notes = [(1046.5_f32, 0.0_f32), (1568.0, 0.07)];
    let samples: Vec<i16> = (0..len)
        .map(|i| {
            let t = i as f32 / RATE as f32;
            let mut v = 0.0;
            for (freq, offset) in notes {
                let t = t - offset;
                if t < 0.0 {
                    continue;
                }
                let attack = (t / 0.004).min(1.0);
                let env = attack * (-t * 9.0).exp();
                let phase = std::f32::consts::TAU * freq * t;
                v += env * (phase.sin() + 0.25 * (2.0 * phase).sin());
            }
            (v * 0.3 * i16::MAX as f32) as i16
        })
        .collect();

    let data_len = (samples.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&RATE.to_le_bytes());
    wav.extend_from_slice(&(RATE * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    wav
}

/// Plays a sound file without blocking. Tries pw-play, then paplay.
pub fn play(file: &Path, volume: f32) {
    let attempts = [
        ("pw-play", vec![format!("--volume={volume}")]),
        (
            "paplay",
            vec![format!("--volume={}", (volume * 65536.0) as u32)],
        ),
    ];
    for (program, args) in attempts {
        let child = Command::new(program)
            .args(args)
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = child {
            std::thread::spawn(move || child.wait());
            return;
        }
    }
    eprintln!("suprclip: couldn't play sound (install pipewire or pulseaudio-utils)");
}

/// Fire-and-forget desktop notification.
pub fn notify(summary: &str, body: &str) {
    let child = Command::new("notify-send")
        .args(["-a", "suprclip", summary, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        std::thread::spawn(move || child.wait());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wav_header_is_consistent() {
        let wav = super::ding_wav();
        assert_eq!(&wav[0..4], b"RIFF");
        let riff_len = u32::from_le_bytes(wav[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff_len + 8, wav.len());
    }
}

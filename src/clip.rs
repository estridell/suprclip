//! Turning the segment ring into a clip file.

use crate::config::Config;
use crate::recorder::Paths;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub file: String,
    pub start: f64,
    pub end: f64,
}

impl Segment {
    fn duration(&self) -> f64 {
        self.end - self.start
    }
}

/// Parses ffmpeg's csv segment list. Only newline-terminated lines count, since ffmpeg may be
/// rewriting the file while we read it.
pub fn parse_list(text: &str) -> Vec<Segment> {
    let complete = match text.rfind('\n') {
        Some(i) => &text[..i],
        None => return Vec::new(),
    };
    complete
        .lines()
        .filter_map(|line| {
            let mut parts = line.rsplitn(3, ',');
            let end = parts.next()?.parse().ok()?;
            let start = parts.next()?.parse().ok()?;
            let file = parts.next()?.to_string();
            Some(Segment { file, start, end })
        })
        .collect()
}

pub fn read_list(path: &Path) -> Vec<Segment> {
    // An empty read means we caught ffmpeg mid-rewrite; try again briefly.
    for _ in 0..10 {
        let segments = parse_list(&std::fs::read_to_string(path).unwrap_or_default());
        if !segments.is_empty() {
            return segments;
        }
        sleep(Duration::from_millis(20));
    }
    Vec::new()
}

pub fn buffered_seconds(paths: &Paths) -> f64 {
    read_list(&paths.list).iter().map(Segment::duration).sum()
}

/// Newest segments covering at least `seconds`, oldest first.
pub fn select(segments: &[Segment], seconds: f64) -> Vec<Segment> {
    let mut picked = Vec::new();
    let mut total = 0.0;
    for seg in segments.iter().rev() {
        if total >= seconds {
            break;
        }
        total += seg.duration();
        picked.push(seg.clone());
    }
    picked.reverse();
    picked
}

pub struct Saved {
    pub path: PathBuf,
    pub seconds: f64,
}

pub fn save(cfg: &Config, paths: &Paths, seconds: u32, output: Option<PathBuf>) -> Result<Saved> {
    // Wait for the segment in progress to land so the clip ends at the moment you asked.
    let last_end = read_list(&paths.list).last().map(|s| s.end);
    let deadline = Instant::now() + Duration::from_millis(2500);
    while Instant::now() < deadline {
        if read_list(&paths.list).last().map(|s| s.end) > last_end {
            break;
        }
        sleep(Duration::from_millis(50));
    }

    let segments: Vec<_> = select(&read_list(&paths.list), seconds as f64)
        .into_iter()
        .filter(|s| paths.buffer.join(&s.file).exists())
        .collect();
    if segments.is_empty() {
        bail!("nothing recorded yet");
    }
    let total: f64 = segments.iter().map(Segment::duration).sum();

    let concat = paths.buffer.join("concat.txt");
    let listing: String = segments
        .iter()
        .map(|s| format!("file '{}'\n", paths.buffer.join(&s.file).display()))
        .collect();
    std::fs::write(&concat, listing)?;

    let path = match output {
        Some(p) => p,
        None => {
            let dir = cfg.output_dir();
            std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            unique_path(&dir, &format!("suprclip_{}", timestamp()), &cfg.container)
        }
    };

    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(["-f", "concat", "-safe", "0", "-i"])
        .arg(&concat)
        .args(["-map", "0", "-c", "copy"]);
    if matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("mp4" | "mov")
    ) {
        cmd.args(["-movflags", "+faststart"]);
    }
    let out = cmd
        .arg(&path)
        .stdout(Stdio::null())
        .output()
        .context("running ffmpeg")?;
    let _ = std::fs::remove_file(&concat);
    if !out.status.success() {
        bail!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(Saved {
        path,
        seconds: total,
    })
}

fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}_{n}.{ext}"));
        n += 1;
    }
    path
}

/// Local time as `2026-10-26_23-14-05`.
fn timestamp() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        let mut buf = [0u8; 32];
        let n = libc::strftime(
            buf.as_mut_ptr().cast(),
            buf.len(),
            c"%Y-%m-%d_%H-%M-%S".as_ptr(),
            &tm,
        );
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(file: &str, start: f64, end: f64) -> Segment {
        Segment {
            file: file.into(),
            start,
            end,
        }
    }

    #[test]
    fn parses_complete_lines_only() {
        let text = "seg0001.ts,0.000000,1.050000\nseg0002.ts,1.050000,2.050000\nseg0003.ts,2.05";
        assert_eq!(
            parse_list(text),
            vec![seg("seg0001.ts", 0.0, 1.05), seg("seg0002.ts", 1.05, 2.05)]
        );
        assert!(parse_list("seg0001.ts,0.0,1.0").is_empty());
    }

    #[test]
    fn selects_newest_covering_duration() {
        let list: Vec<_> = (0..10)
            .map(|i| seg(&format!("s{i}"), i as f64, i as f64 + 1.0))
            .collect();
        let picked = select(&list, 3.0);
        assert_eq!(
            picked.iter().map(|s| s.file.as_str()).collect::<Vec<_>>(),
            ["s7", "s8", "s9"]
        );
        assert_eq!(select(&list, 100.0).len(), 10);
    }

    #[test]
    fn timestamp_shape() {
        assert_eq!(timestamp().len(), "2026-10-26_23-14-05".len());
    }
}

//! The capture pipeline: wf-recorder encodes the screen to an MPEG-TS stream, and ffmpeg
//! (stream copy, no re-encode) slices it into a ring of 1-second segments on tmpfs.

use crate::config::Config;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};

/// Segment length in seconds. Keyframes are placed every second so cuts land exactly.
pub const SEGMENT_SECONDS: u32 = 1;
/// Extra segments kept on disk beyond the list, so files being clipped aren't overwritten.
const WRAP_MARGIN: u32 = 15;

pub struct Paths {
    pub dir: PathBuf,
    pub buffer: PathBuf,
    pub list: PathBuf,
    pub socket: PathBuf,
    pub wf_log: PathBuf,
    pub ffmpeg_log: PathBuf,
    pub ding: PathBuf,
}

impl Paths {
    pub fn new() -> Self {
        let dir = match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(d) => PathBuf::from(d).join("suprclip"),
            None => std::env::temp_dir().join(format!("suprclip-{}", unsafe { libc::getuid() })),
        };
        let buffer = dir.join("buffer");
        Self {
            list: buffer.join("segments.csv"),
            socket: dir.join("ctl.sock"),
            wf_log: dir.join("wf-recorder.log"),
            ffmpeg_log: dir.join("ffmpeg.log"),
            ding: dir.join("ding.wav"),
            buffer,
            dir,
        }
    }
}

pub struct Pipeline {
    recorder: Child,
    segmenter: Child,
}

impl Pipeline {
    pub fn spawn(cfg: &Config, monitor: &str, paths: &Paths) -> Result<Self> {
        let mut recorder = command("wf-recorder")
            .args(recorder_args(cfg, monitor))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(log_file(&paths.wf_log)?)
            .spawn()
            .context("failed to start wf-recorder (is it installed?)")?;
        let stream = recorder.stdout.take().expect("piped stdout");

        let segmenter = command("ffmpeg")
            .args(segmenter_args(cfg))
            .current_dir(&paths.buffer)
            .stdin(stream)
            .stdout(Stdio::null())
            .stderr(log_file(&paths.ffmpeg_log)?)
            .spawn();
        let segmenter = match segmenter {
            Ok(s) => s,
            Err(e) => {
                let _ = recorder.kill();
                let _ = recorder.wait();
                return Err(e).context("failed to start ffmpeg (is it installed?)");
            }
        };
        Ok(Self {
            recorder,
            segmenter,
        })
    }

    pub fn pids(&self) -> [u32; 2] {
        [self.recorder.id(), self.segmenter.id()]
    }

    /// Blocks until the pipeline dies. Returns wf-recorder's exit status.
    pub fn wait(mut self) -> ExitStatus {
        let status = self
            .recorder
            .wait()
            .unwrap_or_else(|_| ExitStatus::default());
        // ffmpeg sees EOF and exits by itself; make sure in case it's wedged.
        let _ = self.segmenter.kill();
        let _ = self.segmenter.wait();
        status
    }
}

/// Children get their own process group (so a terminal Ctrl-C reaches only us and we shut
/// them down in order) and die with us even if we're SIGKILLed.
fn command(program: &str) -> Command {
    let mut cmd = Command::new(program);
    unsafe {
        cmd.pre_exec(|| {
            libc::setpgid(0, 0);
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd
}

fn log_file(path: &Path) -> Result<File> {
    File::create(path).with_context(|| format!("creating {}", path.display()))
}

pub fn recorder_args(cfg: &Config, monitor: &str) -> Vec<String> {
    let v = &cfg.video;
    let mut args: Vec<String> = vec!["-y".into(), "-o".into(), monitor.into()];
    args.extend([
        "-c".into(),
        v.codec.clone(),
        "-r".into(),
        cfg.fps.to_string(),
    ]);
    if let Some(fmt) = &v.pixel_format {
        args.extend(["-x".into(), fmt.clone()]);
    }
    for (k, val) in codec_params(cfg) {
        args.extend(["-p".into(), format!("{k}={val}")]);
    }
    if cfg.audio.enabled {
        let device = match cfg.audio.device.as_str() {
            "default" => "@DEFAULT_MONITOR@",
            d => d,
        };
        args.push(format!("--audio={device}"));
        args.extend(["-C".into(), cfg.audio.codec.clone()]);
        args.extend(["-P".into(), format!("b={}", cfg.audio.bitrate)]);
    }
    args.extend(["-m", "mpegts", "-f", "/dev/stdout"].map(String::from));
    args
}

/// Per-encoder quality settings, overridable via `[video.params]`.
fn codec_params(cfg: &Config) -> BTreeMap<String, String> {
    let v = &cfg.video;
    let q = v.quality.to_string();
    let mut p = BTreeMap::new();
    let mut set = |k: &str, val: &str| {
        p.insert(k.to_string(), val.to_string());
    };
    set("g", &(cfg.fps * SEGMENT_SECONDS).to_string());

    let preset = |default: &str| v.preset.clone().unwrap_or_else(|| default.into());
    if v.codec.ends_with("_nvenc") {
        set("preset", &preset("p4"));
        set("rc", "vbr");
        set("cq", &q);
        set("b", "0");
    } else if v.codec.ends_with("_vaapi") {
        set("qp", &q);
    } else {
        match v.codec.as_str() {
            "libx264" | "libx265" => {
                set("preset", &preset("veryfast"));
                set("crf", &q);
            }
            "libsvtav1" => {
                set("preset", &preset("10"));
                set("crf", &q);
            }
            _ => {
                if let Some(pr) = &v.preset {
                    set("preset", pr);
                }
            }
        }
    }
    p.extend(v.params.clone());
    p
}

fn segmenter_args(cfg: &Config) -> Vec<String> {
    let listed = cfg.buffer_seconds().div_ceil(SEGMENT_SECONDS) + 2;
    let wrap = listed + WRAP_MARGIN;
    [
        "-hide_banner",
        "-loglevel",
        "warning",
        "-f",
        "mpegts",
        "-i",
        "pipe:0",
        "-map",
        "0",
        "-c",
        "copy",
        "-f",
        "segment",
        "-segment_format",
        "mpegts",
        "-segment_time",
        &SEGMENT_SECONDS.to_string(),
        "-segment_wrap",
        &wrap.to_string(),
        "-segment_list",
        "segments.csv",
        "-segment_list_type",
        "csv",
        "-segment_list_size",
        &listed.to_string(),
        "seg%04d.ts",
    ]
    .map(String::from)
    .to_vec()
}

/// Picks the monitor to record. "auto" = the focused Hyprland monitor.
pub fn resolve_monitor(wanted: &str) -> Result<String> {
    let monitors = hypr_monitors();
    if wanted != "auto" {
        if let Ok(list) = &monitors
            && !list.iter().any(|(name, _)| name == wanted)
        {
            let names: Vec<_> = list.iter().map(|(n, _)| n.as_str()).collect();
            bail!(
                "monitor {wanted:?} not found (available: {})",
                names.join(", ")
            );
        }
        return Ok(wanted.to_string());
    }
    let list = monitors.context("monitor = \"auto\" needs hyprctl; set `monitor` in the config")?;
    list.iter()
        .find(|(_, focused)| *focused)
        .or(list.first())
        .map(|(name, _)| name.clone())
        .context("hyprctl reported no monitors")
}

fn hypr_monitors() -> Result<Vec<(String, bool)>> {
    let out = Command::new("hyprctl").args(["monitors", "-j"]).output()?;
    if !out.status.success() {
        bail!("hyprctl failed");
    }
    let json: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    Ok(json
        .as_array()
        .context("unexpected hyprctl output")?
        .iter()
        .filter_map(|m| {
            let name = m["name"].as_str()?.to_string();
            Some((name, m["focused"].as_bool().unwrap_or(false)))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn nvenc_defaults() {
        let args = recorder_args(&Config::default(), "DP-4");
        assert!(has_pair(&args, "-o", "DP-4"));
        assert!(has_pair(&args, "-p", "cq=23"));
        assert!(has_pair(&args, "-p", "preset=p4"));
        assert!(has_pair(&args, "-p", "g=60"));
        assert!(args.contains(&"--audio=@DEFAULT_MONITOR@".to_string()));
    }

    #[test]
    fn user_params_override_defaults() {
        let mut cfg = Config::default();
        cfg.video.codec = "libx264".into();
        cfg.video.quality = 18;
        cfg.video.params.insert("preset".into(), "faster".into());
        cfg.audio.enabled = false;
        let args = recorder_args(&cfg, "DP-4");
        assert!(has_pair(&args, "-p", "crf=18"));
        assert!(has_pair(&args, "-p", "preset=faster"));
        assert!(!args.iter().any(|a| a.starts_with("--audio")));
    }
}

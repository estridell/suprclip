use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The template written by `suprclip config --init`. Kept in sync with `Default` by a test.
pub const DEFAULT_CONFIG: &str = r#"# suprclip config

# Seconds saved when you run `suprclip` / `suprclip clip`.
clip_seconds = 30
# Seconds of history kept in RAM. Raised to clip_seconds if lower.
buffer_seconds = 60
# Where clips are written. `~` is expanded.
output_dir = "~/Videos/suprclip"
# Container for saved clips: "mp4" or "mkv".
container = "mp4"
# Hyprland monitor to record (see `hyprctl monitors`), or "auto" for the
# monitor that is focused when recording starts.
monitor = "auto"
fps = 60
# Desktop notification (notify-send) when a clip is saved.
notify = false

[video]
# Any ffmpeg encoder: h264_nvenc, hevc_nvenc, av1_nvenc, libx264, libx265, libsvtav1, ...
codec = "h264_nvenc"
# Constant-quality level (cq for nvenc, crf for software encoders). Lower = better + bigger.
quality = 23
# Encoder preset. Unset = sensible per-encoder default (p4 for nvenc, veryfast for x264).
# preset = "p4"
# Output pixel format override (`ffmpeg -pix_fmts`). Unset = let wf-recorder pick.
# pixel_format = "yuv420p"

# Extra encoder options, passed as `-p key=value` and overriding the defaults above.
[video.params]
# tune = "hq"

[audio]
enabled = true
# "default" records whatever plays on your default output (follows device changes).
# Otherwise a PulseAudio/PipeWire source name from `pactl list short sources`.
device = "default"
codec = "aac"
bitrate = "160k"

[sound]
# Play a ding when a clip is saved.
enabled = true
# Custom sound file (wav/ogg/flac). Unset = built-in ding.
# file = "/usr/share/sounds/freedesktop/stereo/complete.oga"
volume = 0.6
"#;

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub clip_seconds: u32,
    pub buffer_seconds: u32,
    pub output_dir: String,
    pub container: String,
    pub monitor: String,
    pub fps: u32,
    pub video: Video,
    pub audio: Audio,
    pub sound: Sound,
    pub notify: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Video {
    pub codec: String,
    pub quality: u32,
    pub preset: Option<String>,
    pub pixel_format: Option<String>,
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Audio {
    pub enabled: bool,
    pub device: String,
    pub codec: String,
    pub bitrate: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Sound {
    pub enabled: bool,
    pub file: Option<PathBuf>,
    pub volume: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            clip_seconds: 30,
            buffer_seconds: 60,
            output_dir: "~/Videos/suprclip".into(),
            container: "mp4".into(),
            monitor: "auto".into(),
            fps: 60,
            video: Video::default(),
            audio: Audio::default(),
            sound: Sound::default(),
            notify: false,
        }
    }
}

impl Default for Video {
    fn default() -> Self {
        Self {
            codec: "h264_nvenc".into(),
            quality: 23,
            preset: None,
            pixel_format: None,
            params: BTreeMap::new(),
        }
    }
}

impl Default for Audio {
    fn default() -> Self {
        Self {
            enabled: true,
            device: "default".into(),
            codec: "aac".into(),
            bitrate: "160k".into(),
        }
    }
}

impl Default for Sound {
    fn default() -> Self {
        Self {
            enabled: true,
            file: None,
            volume: 0.6,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let cfg: Config = match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text)
                .with_context(|| format!("invalid config {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<()> {
        if self.clip_seconds == 0 {
            bail!("clip_seconds must be > 0");
        }
        if self.fps == 0 {
            bail!("fps must be > 0");
        }
        if !matches!(self.container.as_str(), "mp4" | "mkv") {
            bail!("container must be \"mp4\" or \"mkv\"");
        }
        Ok(())
    }

    pub fn buffer_seconds(&self) -> u32 {
        self.buffer_seconds.max(self.clip_seconds)
    }

    pub fn output_dir(&self) -> PathBuf {
        expand_tilde(&self.output_dir)
    }
}

pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".config"));
    base.join("suprclip").join("config.toml")
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~") {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            home().join(rest.trim_start_matches('/'))
        }
        _ => PathBuf::from(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_matches_defaults() {
        let parsed: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(parsed, Config::default());
    }

    #[test]
    fn partial_config_fills_defaults() {
        let cfg: Config = toml::from_str("clip_seconds = 15\n[video]\nquality = 18\n").unwrap();
        assert_eq!(cfg.clip_seconds, 15);
        assert_eq!(cfg.video.quality, 18);
        assert_eq!(cfg.video.codec, "h264_nvenc");
        assert!(cfg.audio.enabled);
    }

    #[test]
    fn buffer_never_shorter_than_clip() {
        let cfg = Config {
            clip_seconds: 90,
            buffer_seconds: 60,
            ..Config::default()
        };
        assert_eq!(cfg.buffer_seconds(), 90);
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(toml::from_str::<Config>("clip_secs = 10").is_err());
    }
}

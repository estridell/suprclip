mod clip;
mod config;
mod daemon;
mod ipc;
mod recorder;
mod sound;

use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand};
use config::Config;
use ipc::{Request, Response};
use recorder::Paths;
use std::path::PathBuf;
use std::process::ExitCode;

/// Lightweight replay-buffer screen clipper for Hyprland.
///
/// Run `suprclip start` once, then `suprclip` (bind it to a key) saves the last 30 seconds.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// Config file [default: ~/.config/suprclip/config.toml]
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Start recording into the replay buffer (runs in the foreground)
    Start(StartArgs),
    /// Save the last N seconds (what plain `suprclip` does)
    Clip {
        /// Seconds to save [default: clip_seconds from the config]
        #[arg(short = 'd', long)]
        seconds: Option<u32>,
        /// Output file [default: <output_dir>/suprclip_<time>.mp4]
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Show what the recorder is doing
    Status,
    /// Stop recording and free the buffer
    Stop,
    /// Print the config path, or write a commented default config
    Config {
        /// Write the default config file
        #[arg(long)]
        init: bool,
        /// Overwrite an existing config with --init
        #[arg(long, requires = "init")]
        force: bool,
    },
}

/// Overrides for the config file.
#[derive(Args)]
struct StartArgs {
    /// Monitor to record ("auto" = focused)
    #[arg(short, long)]
    monitor: Option<String>,
    #[arg(long)]
    fps: Option<u32>,
    /// ffmpeg encoder, e.g. h264_nvenc, hevc_nvenc, av1_nvenc, libx264
    #[arg(long)]
    codec: Option<String>,
    /// Constant-quality level (lower = better)
    #[arg(short, long)]
    quality: Option<u32>,
    /// Default clip length in seconds
    #[arg(short = 'd', long)]
    seconds: Option<u32>,
    /// Seconds of history to keep
    #[arg(long)]
    buffer: Option<u32>,
    /// Don't record audio
    #[arg(long)]
    no_audio: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("suprclip: {e:#}");
            // Launched from a keybind there's no terminal to read errors in.
            if unsafe { libc::isatty(libc::STDERR_FILENO) } == 0 {
                sound::notify("suprclip", &format!("{e:#}"));
            }
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    let config_path = cli.config.unwrap_or_else(config::default_path);
    let paths = Paths::new();
    match cli.command.unwrap_or(Cmd::Clip {
        seconds: None,
        output: None,
    }) {
        Cmd::Start(args) => {
            let mut cfg = Config::load(&config_path)?;
            args.apply(&mut cfg);
            daemon::run(cfg)
        }
        Cmd::Clip { seconds, output } => {
            let output = output.map(std::path::absolute).transpose()?;
            match ipc::send(&paths.socket, &Request::Clip { seconds, output })? {
                Response::Saved { path, seconds } => {
                    println!("saved {seconds:.0}s to {}", path.display());
                    Ok(())
                }
                other => unexpected(other),
            }
        }
        Cmd::Status => match ipc::send(&paths.socket, &Request::Status)? {
            Response::Status(s) => {
                println!("recording   {} @ {} fps, {}", s.monitor, s.fps, s.codec);
                println!("audio       {}", if s.audio { "on" } else { "off" });
                println!(
                    "buffered    {:.0}s of {}s",
                    s.buffered_seconds, s.buffer_seconds
                );
                println!("clip length {}s", s.clip_seconds);
                println!("saving to   {}", s.output_dir.display());
                println!("logs        {}", s.runtime_dir.display());
                println!("pid         {}", s.pid);
                Ok(())
            }
            other => unexpected(other),
        },
        Cmd::Stop => match ipc::send(&paths.socket, &Request::Stop)? {
            Response::Stopping => Ok(()),
            other => unexpected(other),
        },
        Cmd::Config { init, force } => {
            if init {
                if config_path.exists() && !force {
                    bail!("{} already exists (use --force)", config_path.display());
                }
                if let Some(dir) = config_path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(&config_path, config::DEFAULT_CONFIG)?;
                println!("wrote {}", config_path.display());
            } else {
                let note = if config_path.exists() {
                    ""
                } else {
                    " (not created, using defaults)"
                };
                println!("{}{note}", config_path.display());
            }
            Ok(())
        }
    }
}

fn unexpected(resp: Response) -> Result<()> {
    match resp {
        Response::Error { message } => bail!("{message}"),
        other => bail!("unexpected response: {other:?}"),
    }
}

impl StartArgs {
    fn apply(self, cfg: &mut Config) {
        if let Some(m) = self.monitor {
            cfg.monitor = m;
        }
        if let Some(f) = self.fps {
            cfg.fps = f;
        }
        if let Some(c) = self.codec {
            cfg.video.codec = c;
        }
        if let Some(q) = self.quality {
            cfg.video.quality = q;
        }
        if let Some(s) = self.seconds {
            cfg.clip_seconds = s;
        }
        if let Some(b) = self.buffer {
            cfg.buffer_seconds = b;
        }
        if self.no_audio {
            cfg.audio.enabled = false;
        }
    }
}

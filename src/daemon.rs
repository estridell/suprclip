//! `suprclip start`: keeps the capture pipeline alive and answers clip requests.

use crate::clip;
use crate::config::Config;
use crate::ipc::{self, Request, Response, Status};
use crate::recorder::{self, Paths, Pipeline};
use crate::sound;
use anyhow::{Context, Result, bail};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

struct Daemon {
    cfg: Config,
    monitor: String,
    paths: Paths,
    stopping: AtomicBool,
    pids: Mutex<Vec<u32>>,
}

pub fn run(cfg: Config) -> Result<()> {
    let paths = Paths::new();
    if UnixStream::connect(&paths.socket).is_ok() {
        bail!("already recording (stop it with `suprclip stop`)");
    }
    let monitor = recorder::resolve_monitor(&cfg.monitor)?;

    std::fs::create_dir_all(&paths.dir)
        .with_context(|| format!("creating {}", paths.dir.display()))?;
    std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o700))?;
    let _ = std::fs::remove_file(&paths.socket);
    let listener = UnixListener::bind(&paths.socket).context("binding control socket")?;
    if cfg.sound.enabled && cfg.sound.file.is_none() {
        std::fs::write(&paths.ding, sound::ding_wav())?;
    }

    // Lives for the rest of the process; shared with the signal handler and supervisor.
    let d: &'static Daemon = Box::leak(Box::new(Daemon {
        cfg,
        monitor,
        paths,
        stopping: AtomicBool::new(false),
        pids: Mutex::new(Vec::new()),
    }));
    ctrlc::set_handler(move || d.shutdown(0)).context("installing signal handler")?;

    let c = &d.cfg;
    eprintln!(
        "suprclip: recording {} at {} fps ({}, quality {}{}), keeping the last {}s",
        d.monitor,
        c.fps,
        c.video.codec,
        c.video.quality,
        if c.audio.enabled { ", with audio" } else { "" },
        c.buffer_seconds(),
    );
    eprintln!(
        "suprclip: run `suprclip` to save the last {}s",
        c.clip_seconds
    );

    std::thread::spawn(move || d.supervise());
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => d.handle(stream),
            Err(e) => eprintln!("suprclip: accept failed: {e}"),
        }
    }
    Ok(())
}

impl Daemon {
    fn supervise(&self) {
        let mut backoff = Duration::from_secs(1);
        let mut first = true;
        loop {
            reset_dir(&self.paths.buffer);
            let started = Instant::now();
            match Pipeline::spawn(&self.cfg, &self.monitor, &self.paths) {
                Ok(pipeline) => {
                    *self.pids.lock().unwrap() = pipeline.pids().to_vec();
                    let status = pipeline.wait();
                    self.pids.lock().unwrap().clear();
                    if self.stopping.load(Ordering::SeqCst) {
                        return;
                    }
                    eprintln!("suprclip: recorder stopped ({status})");
                    for log in [&self.paths.wf_log, &self.paths.ffmpeg_log] {
                        eprint!("{}", tail(log, 6));
                    }
                }
                Err(e) => eprintln!("suprclip: {e:#}"),
            }
            // A pipeline that can't even get going is a config problem, not a hiccup.
            if first && started.elapsed() < Duration::from_secs(5) {
                eprintln!("suprclip: recorder failed to start, giving up");
                self.shutdown(1);
            }
            first = false;
            backoff = if started.elapsed() > Duration::from_secs(30) {
                Duration::from_secs(1)
            } else {
                (backoff * 2).min(Duration::from_secs(30))
            };
            eprintln!("suprclip: restarting in {}s", backoff.as_secs());
            sleep(backoff);
        }
    }

    fn handle(&self, stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let response = match ipc::read_line::<Request>(&stream) {
            Ok(Request::Clip { seconds, output }) => self.clip(seconds, output),
            Ok(Request::Status) => Response::Status(self.status()),
            Ok(Request::Stop) => {
                let _ = ipc::write_line(&stream, &Response::Stopping);
                self.shutdown(0);
            }
            Err(e) => Response::Error {
                message: format!("{e:#}"),
            },
        };
        let _ = ipc::write_line(&stream, &response);
    }

    fn clip(&self, seconds: Option<u32>, output: Option<PathBuf>) -> Response {
        let cfg = &self.cfg;
        let seconds = seconds.unwrap_or(cfg.clip_seconds);
        match clip::save(cfg, &self.paths, seconds, output) {
            Ok(saved) => {
                eprintln!(
                    "suprclip: saved {:.0}s to {}",
                    saved.seconds,
                    saved.path.display()
                );
                if cfg.sound.enabled {
                    let file = cfg.sound.file.as_deref().unwrap_or(&self.paths.ding);
                    sound::play(file, cfg.sound.volume);
                }
                if cfg.notify {
                    sound::notify("Clip saved", &saved.path.display().to_string());
                }
                Response::Saved {
                    path: saved.path,
                    seconds: saved.seconds,
                }
            }
            Err(e) => {
                let message = format!("{e:#}");
                eprintln!("suprclip: clip failed: {message}");
                if cfg.notify {
                    sound::notify("Clip failed", &message);
                }
                Response::Error { message }
            }
        }
    }

    fn status(&self) -> Status {
        let cfg = &self.cfg;
        Status {
            pid: std::process::id(),
            monitor: self.monitor.clone(),
            codec: cfg.video.codec.clone(),
            fps: cfg.fps,
            audio: cfg.audio.enabled,
            buffered_seconds: clip::buffered_seconds(&self.paths),
            buffer_seconds: cfg.buffer_seconds(),
            clip_seconds: cfg.clip_seconds,
            output_dir: cfg.output_dir(),
            runtime_dir: self.paths.dir.clone(),
        }
    }

    fn shutdown(&self, code: i32) -> ! {
        self.stopping.store(true, Ordering::SeqCst);
        for &pid in self.pids.lock().unwrap().iter() {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
        let _ = std::fs::remove_file(&self.paths.socket);
        let _ = std::fs::remove_dir_all(&self.paths.buffer);
        let _ = std::fs::remove_file(&self.paths.ding);
        if code == 0 {
            eprintln!("suprclip: stopped");
        }
        std::process::exit(code);
    }
}

fn reset_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::create_dir_all(dir);
}

fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let all: Vec<_> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    all[all.len().saturating_sub(lines)..]
        .iter()
        .map(|l| {
            format!(
                "  {}: {l}\n",
                path.file_stem().unwrap_or_default().to_string_lossy()
            )
        })
        .collect()
}

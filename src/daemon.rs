//! `suprclip start`: keeps the capture pipeline alive and answers clip requests.

use crate::clip;
use crate::config::Config;
use crate::ipc::{self, Request, Response, Status};
use crate::recorder::{self, Paths, Pipeline};
use crate::sound;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread::sleep;
use std::time::{Duration, Instant};

struct Daemon {
    cfg: Config,
    monitor: String,
    paths: Paths,
    stopping: AtomicBool,
    pids: Mutex<Vec<u32>>,
    /// Held while anything reads or wipes the segment buffer.
    buffer: Mutex<()>,
    _lock: File,
}

pub fn run(cfg: Config) -> Result<()> {
    let paths = Paths::new();
    std::fs::create_dir_all(&paths.dir)
        .with_context(|| format!("creating {}", paths.dir.display()))?;
    std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o700))?;
    // Held until we exit, so two `start`s can't both take over the socket.
    let lock = File::create(&paths.lock)?;
    if lock.try_lock().is_err() {
        bail!("already recording (stop it with `suprclip stop`)");
    }
    let monitor = recorder::resolve_monitor(&cfg.monitor)?;
    warn_if_window_too_big(&cfg, &paths.dir);

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
        buffer: Mutex::new(()),
        _lock: lock,
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
        c.seconds,
    );
    eprintln!("suprclip: run `suprclip` to save them");

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
            {
                let _buffer = self.lock_buffer();
                reset_dir(&self.paths.buffer);
            }
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
            Ok(Request::Clip { output }) => self.clip(output),
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

    fn clip(&self, output: Option<PathBuf>) -> Response {
        let cfg = &self.cfg;
        let saved = {
            let _buffer = self.lock_buffer();
            clip::save(cfg, &self.paths, output)
        };
        match saved {
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
            buffered_seconds: clip::buffered_seconds(&self.paths).min(cfg.seconds as f64),
            seconds: cfg.seconds,
            output_dir: cfg.output_dir(),
            runtime_dir: self.paths.dir.clone(),
        }
    }

    fn lock_buffer(&self) -> MutexGuard<'_, ()> {
        self.buffer.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn shutdown(&self, code: i32) -> ! {
        self.stopping.store(true, Ordering::SeqCst);
        // Let a clip that's being saved finish first.
        let _buffer = self.lock_buffer();
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

/// Rough upper bound for a busy 1080p60 game at the default quality.
const EST_BYTES_PER_SECOND: u64 = 3_000_000;

/// The buffer lives on tmpfs (RAM), so warn before a huge window fills it.
fn warn_if_window_too_big(cfg: &Config, dir: &Path) {
    let need = cfg.seconds as u64 * EST_BYTES_PER_SECOND;
    let Some(free) = free_bytes(dir) else { return };
    if need > free {
        eprintln!(
            "suprclip: warning: a {}s window can take ~{:.1} GB of RAM, but {} only has {:.1} GB free",
            cfg.seconds,
            need as f64 / 1e9,
            dir.display(),
            free as f64 / 1e9,
        );
    }
}

fn free_bytes(dir: &Path) -> Option<u64> {
    let path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
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

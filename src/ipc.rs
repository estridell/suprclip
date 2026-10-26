//! One JSON line each way over a unix socket.

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    Clip {
        seconds: Option<u32>,
        output: Option<PathBuf>,
    },
    Status,
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Saved { path: PathBuf, seconds: f64 },
    Status(Status),
    Stopping,
    Error { message: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub pid: u32,
    pub monitor: String,
    pub codec: String,
    pub fps: u32,
    pub audio: bool,
    pub buffered_seconds: f64,
    pub buffer_seconds: u32,
    pub clip_seconds: u32,
    pub output_dir: PathBuf,
    pub runtime_dir: PathBuf,
}

pub fn send(socket: &Path, req: &Request) -> Result<Response> {
    let stream = UnixStream::connect(socket)
        .map_err(|_| anyhow!("not recording (start it with `suprclip start`)"))?;
    write_line(&stream, req)?;
    read_line(&stream)
}

pub fn read_line<T: for<'de> Deserialize<'de>>(stream: &UnixStream) -> Result<T> {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    serde_json::from_str(&line).context("malformed message")
}

pub fn write_line<T: Serialize>(mut stream: &UnixStream, msg: &T) -> Result<()> {
    let mut line = serde_json::to_string(msg)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    Ok(())
}

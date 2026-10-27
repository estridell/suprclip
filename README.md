<img src="assets/icon.svg" width="64" height="64" alt="">

# suprclip

A tiny replay-buffer clipper for Hyprland. Leave it recording while you play. When something cool happens, run `suprclip` and the last 30 seconds land in `~/Videos/suprclip` with a little *ding*.

```sh
suprclip start        # keep a rolling 30s window in RAM (foreground)
suprclip start -d 90  # ...or a 90s one
suprclip              # save the window (same as `suprclip clip`)
suprclip status
suprclip stop
```

## How it works

`wf-recorder` grabs the monitor (wlr-screencopy) and encodes on the GPU with NVENC. `ffmpeg` stream-copies that into a ring of 1-second segments in `$XDG_RUNTIME_DIR` (RAM). Clipping waits for the segment in progress to finish, then joins the newest segments into an mp4 without re-encoding. That takes under a second. The suprclip process itself is about 4 MB and stays idle.

The clip ends right after you pressed the key. A failed recorder is restarted automatically.

The window lives in RAM, so its size scales with length: roughly 1–3 MB per second at 1080p60 and the default quality. 30s is about 30–90 MB. A 24-minute window (`-d 1440`) can reach ~4 GB. `start` warns you if the window might not fit in `$XDG_RUNTIME_DIR`, which is usually capped at 10% of RAM.

## Install

Needs `wf-recorder`, `ffmpeg`, and PipeWire (`pw-play`) or `paplay`. On Arch:

```sh
sudo pacman -S wf-recorder ffmpeg
cargo install --path .
```

Then add this to `~/.config/hypr/hyprland.conf`:

```ini
exec-once = suprclip start
bind = , F9, exec, suprclip
```

## Config

`suprclip config --init` writes a commented `~/.config/suprclip/config.toml`. The main knobs:

| key | default | |
| --- | --- | --- |
| `seconds` | `30` | the rolling window, i.e. how much each clip saves |
| `monitor` | `"auto"` | focused one at start, a connector (`"DP-1"`), or part of its description (`"VG279QM"`) |
| `fps` | `60` | |
| `video.codec` | `"h264_nvenc"` | any ffmpeg encoder: `hevc_nvenc`, `av1_nvenc`, `libx264`, ... |
| `video.quality` | `23` | cq/crf, lower is better and bigger |
| `video.preset` | per encoder | `p1`–`p7` for nvenc |
| `video.params` | | extra encoder options, e.g. `tune = "hq"` |
| `audio.enabled` | `true` | records desktop audio (default output's monitor) |
| `sound.file` | built-in ding | any wav/ogg/flac |
| `notify` | `false` | `notify-send` when saved |

`suprclip start` also takes overrides: `-d 60 --monitor DP-4 --fps 120 --codec hevc_nvenc --quality 20 --no-audio`.

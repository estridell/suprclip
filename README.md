<img src="assets/icon.svg" width="64" height="64" alt="">

# suprclip

A tiny replay-buffer clipper for Hyprland. Leave it recording while you play. When something cool happens, run `suprclip` and the last 30 seconds land in `~/Videos/suprclip` with a little *ding*.

```sh
suprclip start      # start recording into the buffer (foreground)
suprclip            # save the last 30s (same as `suprclip clip`)
suprclip clip -d 60 # save the last 60s
suprclip status
suprclip stop
```

## How it works

`wf-recorder` grabs the monitor (wlr-screencopy) and encodes on the GPU with NVENC. `ffmpeg` stream-copies that into a ring of 1-second segments in `$XDG_RUNTIME_DIR` (RAM). Clipping waits for the segment in progress to finish, then joins the newest segments into an mp4 without re-encoding. That takes under a second. The suprclip process itself is about 4 MB and stays idle.

The clip ends right after you pressed the key. A failed recorder is restarted automatically.

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
| `clip_seconds` | `30` | length of a clip |
| `buffer_seconds` | `60` | history kept in RAM (`suprclip clip -d` can go up to this) |
| `monitor` | `"auto"` | Hyprland output name, or the focused one at start |
| `fps` | `60` | |
| `video.codec` | `"h264_nvenc"` | any ffmpeg encoder: `hevc_nvenc`, `av1_nvenc`, `libx264`, ... |
| `video.quality` | `23` | cq/crf, lower is better and bigger |
| `video.preset` | per encoder | `p1`–`p7` for nvenc |
| `video.params` | | extra encoder options, e.g. `tune = "hq"` |
| `audio.enabled` | `true` | records desktop audio (default output's monitor) |
| `sound.file` | built-in ding | any wav/ogg/flac |
| `notify` | `false` | `notify-send` when saved |

`suprclip start` also takes overrides: `--monitor DP-4 --fps 120 --codec hevc_nvenc --quality 20 --no-audio`.

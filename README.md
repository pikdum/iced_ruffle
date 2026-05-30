# iced + ruffle

A proof-of-concept Flash player: [Ruffle](https://github.com/ruffle-rs/ruffle)
renders a `.swf` and the output is displayed, with input and audio, inside an
[iced](https://github.com/iced-rs/iced) GUI.

## Running

The dev shell (devenv) provides everything — toolchain and native libraries:

```sh
direnv reload          # or: devenv shell
cargo run              # opens empty; use the "Open…" button
cargo run -- cat.swf   # load a movie directly
```

`.swf` files are gitignored; bring your own.

## Features

- Open `.swf` files via a native file dialog (or a CLI argument).
- Mouse + keyboard input forwarded to the movie (games are interactive).
- Audio output (cpal).
- Resizable, letterboxed window.

## Architecture

Ruffle and iced both render with wgpu, but rather than share a device they're
bridged through a frame:

```
Ruffle (own wgpu device) --render--> offscreen texture --capture_frame--> RGBA
                                                                            |
iced shader widget  <--sample--  persistent texture (iced's device)  <--write_texture
```

- **Driver:** `Player::tick(dt)` advances the movie on a 60 fps subscription.
  This needs a real audio backend — under the null backend, `tick()` stalls on
  audio-synced content (e.g. embedded video), because it syncs to the audio
  clock. See `src/audio.rs`.
- **Display:** a custom iced `shader` widget (`src/frame_widget.rs`) owns one
  persistent wgpu texture and `write_texture`s a new frame into it only when the
  pixels change (tracked by a content hash). Using `image::Handle` instead mints
  a new texture every frame, thrashing iced's image cache and flickering. This
  is the pattern finn and `iced_video_player` use.
- **Input:** iced mouse/keyboard events become Ruffle `PlayerEvent`s; cursor
  coordinates are mapped through the letterbox into stage space.

The CPU readback + upload is one copy per changed frame. True zero-copy (Ruffle
rendering onto iced's device) is possible but fragile — it would require building
the whole player lazily inside the shader callback — and neither reference player
bothers, so we don't either.

## Why the dev shell carries extra weight

Discovered while getting Ruffle `master` to build and run; all handled in
`devenv.nix`:

- **Rust stable via rust-overlay** — Ruffle `master` uses `if let` guards,
  stabilized in Rust 1.95 (devenv's pinned nixpkgs ships 1.94).
- **JDK** — `ruffle_core`'s build script compiles the AVM2 playerglobal
  (ActionScript 3 standard library) with a Java tool.
- **pkg-config + alsa-lib** — to build cpal's `alsa-sys`.
- **zenity** — the file dialog's fallback when no `xdg-desktop-portal` is running.
- **LD_LIBRARY_PATH** — winit/wgpu/rfd `dlopen` Wayland/X11/Vulkan/GL/dbus libs
  at runtime.

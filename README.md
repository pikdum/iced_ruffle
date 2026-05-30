# iced_ruffle

A reusable [iced](https://github.com/iced-rs/iced) widget that plays Flash
(`.swf`) movies via [Ruffle](https://github.com/ruffle-rs/ruffle) — with input,
audio, and a resizable, letterboxed display.

## Usage

Hold a `RufflePlayer` in your application state and drop a `Ruffle` widget into
your `view`. The widget drives playback, decodes frames, forwards mouse/keyboard
input, plays audio, and schedules its own redraws — **no subscription or input
wiring needed in the host.**

```rust
use iced_ruffle::{Ruffle, RufflePlayer};

struct App { player: RufflePlayer }

impl App {
    fn view(&self) -> iced::Element<'_, Message> {
        Ruffle::new(&self.player).into()
    }
}

// load with RufflePlayer::from_path("movie.swf")? or ::from_bytes(name, &data)?
```

`RufflePlayer` also exposes `size()`, `paused()`/`set_paused()`, and `player()`
(the underlying `Arc<Mutex<ruffle_core::Player>>`) for advanced control.

## Example

A small player with an "Open…" file dialog lives in `examples/player.rs`:

```sh
direnv reload                            # devenv provides the toolchain + libs
cargo run --example player               # opens empty; click "Open…"
cargo run --example player -- cat.swf    # load a movie directly
```

`.swf` files are gitignored; bring your own.

## How it works

Ruffle and iced both render with wgpu, but rather than share a device they're
bridged through a frame:

```
Ruffle (own wgpu device) --render--> offscreen texture --capture_frame--> RGBA
                                                                            |
iced shader widget  <--sample--  persistent texture (iced's device)  <--write_texture
```

- **Display:** a custom iced `shader` widget owns one persistent wgpu texture on
  iced's device and `write_texture`s a new frame into it only when the pixels
  change (tracked by a content hash). Using `image::Handle` instead mints a new
  texture every frame, thrashing iced's image cache and flickering. This is the
  pattern [finn] and [`iced_video_player`] use.
- **Driver:** the widget ticks `Player::tick(dt)` on each redraw and requests the
  next one — self-contained. `tick` needs a real audio backend, or it stalls on
  audio-synced content (e.g. embedded video) because it syncs to the audio clock;
  `src/audio.rs` is a small cpal backend for that.
- **Input:** the widget translates iced mouse/keyboard events into Ruffle
  `PlayerEvent`s, mapping cursor coordinates through the letterbox into stage
  space.

The CPU readback + upload is one copy per changed frame. True zero-copy (Ruffle
rendering onto iced's device) is possible but fragile — it would require building
the whole player lazily inside the shader callback — and neither reference player
bothers, so we don't either.

[finn]: (internal project)
[`iced_video_player`]: https://github.com/jazzfool/iced_video_player

## Why the dev shell carries extra weight

All handled in `devenv.nix`; discovered while getting Ruffle `master` to build
and run:

- **Rust stable via rust-overlay** — Ruffle `master` uses `if let` guards,
  stabilized in Rust 1.95 (devenv's pinned nixpkgs ships 1.94).
- **JDK** — `ruffle_core`'s build script compiles the AVM2 playerglobal
  (ActionScript 3 standard library) with a Java tool.
- **pkg-config + alsa-lib** — to build cpal's `alsa-sys`.
- **zenity** — the file dialog's fallback when no `xdg-desktop-portal` is running.
- **LD_LIBRARY_PATH** — winit/wgpu/rfd `dlopen` Wayland/X11/Vulkan/GL/dbus libs
  at runtime.

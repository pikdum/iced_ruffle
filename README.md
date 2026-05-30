# iced_ruffle

An [iced](https://iced.rs) widget that plays Flash (`.swf`) movies via
[Ruffle](https://ruffle.rs) — with mouse/keyboard input, audio, and a resizable,
letterboxed display.

Hold a `RufflePlayer` in your state and drop a `Ruffle` widget in your `view`.
The widget drives playback, decodes frames, forwards input, plays audio, and
schedules its own redraws — no subscription or wiring needed.

## Usage

```rust
use iced_ruffle::{Ruffle, RufflePlayer};

struct App {
    player: RufflePlayer,
}

impl App {
    fn view(&self) -> iced::Element<'_, Message> {
        Ruffle::new(&self.player).into()
    }
}

// RufflePlayer::from_path("movie.swf")?  or  ::from_bytes(name, &bytes)?
```

See [`examples/player.rs`](examples/player.rs) for a complete app with a file
picker: `cargo run --example player -- movie.swf`.

## Requirements

- Ruffle is pulled from git (`master`), which currently needs **Rust ≥ 1.95** and
  a **JDK** at build time (it compiles the ActionScript playerglobal).
- Audio uses **cpal** (needs ALSA dev headers on Linux).
- `iced_ruffle` and `iced` must resolve to the **same wgpu version** (currently
  27); the widget renders on iced's own device.

## License

MIT OR Apache-2.0

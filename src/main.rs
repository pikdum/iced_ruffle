//! Minimal proof-of-concept: render a `.swf` file with Ruffle and display it in an
//! iced GUI.
//!
//! Strategy: Ruffle and iced both render with wgpu, but sharing a wgpu device
//! between two libraries is fiddly and version-sensitive. Instead we keep them
//! fully decoupled by going through CPU memory:
//!
//!   Ruffle  --render-->  offscreen wgpu texture  --capture_frame-->  RGBA pixels
//!                                                                       |
//!   iced  <--image widget--  image::Handle::from_rgba  <----------------+
//!
//! A 60fps timer subscription advances the movie at its native frame rate and,
//! whenever a new frame is produced, reads the pixels back and hands them to iced.
//!
//! Two non-obvious bits, both learned the hard way:
//!   * The movie is driven with `run_frame()`, not the real-time `tick()`.
//!     `tick()` syncs frame advancement to the audio playback clock, which never
//!     advances under `NullAudioBackend`, so playback stalls after a few frames.
//!     `run_frame()` advances one frame unconditionally; we pace it ourselves.
//!   * `go to bed.swf` is an embedded-video SWF, so it needs the software video
//!     backend (`ruffle_video_software`) — the default null backend renders blank.

use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iced::widget::image::Handle;
use iced::widget::{container, image, text};
use iced::{time, Element, Length, Subscription};

use ruffle_core::limits::ExecutionLimit;
use ruffle_core::{Player, PlayerBuilder};
use ruffle_render_wgpu::backend::WgpuRenderBackend;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_render_wgpu::wgpu;
use ruffle_video_software::backend::SoftwareVideoBackend;

const SWF_PATH: &str = "go to bed.swf";

/// Don't run more than this many movie frames in a single update, so a hitch in
/// the timer can't snowball into a spiral of catch-up frames.
const MAX_FRAMES_PER_UPDATE: u32 = 4;

struct App {
    player: Arc<Mutex<Player>>,
    /// Seconds of real time per movie frame (1 / frame_rate).
    frame_interval: f64,
    /// Real time not yet accounted for in advanced frames.
    accumulator: f64,
    last_tick: Instant,
    frame: Option<Handle>,
}

#[derive(Debug, Clone)]
enum Message {
    Tick(Instant),
}

/// Make sure the root timeline is actually playing — autoplay/`is_playing` alone
/// doesn't resume a root MovieClip that starts stopped.
fn force_root_clip_play(player: &mut Player) {
    if !player.is_playing() {
        player.set_is_playing(true);
    }
    player.mutate_with_update_context(|ctx| {
        if let Some(root_clip) = ctx.stage.root_clip() {
            if let Some(movie_clip) = root_clip.as_movie_clip() {
                if !movie_clip.playing() {
                    movie_clip.play();
                }
            }
        }
    });
}

impl App {
    fn new() -> Self {
        let movie = ruffle_core::tag_utils::movie_from_path(SWF_PATH, None)
            .unwrap_or_else(|e| panic!("failed to load {SWF_PATH}: {e:?}"));

        // The SWF carries its own stage dimensions (twips) and frame rate.
        let width = movie.width().to_pixels().ceil().max(1.0) as u32;
        let height = movie.height().to_pixels().ceil().max(1.0) as u32;
        let frame_rate = movie.frame_rate().to_f64().max(1.0);

        // Offscreen wgpu renderer that renders into a texture we can read back.
        let renderer = WgpuRenderBackend::for_offscreen(
            (width, height),
            wgpu::Backends::PRIMARY,
            wgpu::PowerPreference::HighPerformance,
        )
        .expect("failed to create offscreen wgpu renderer");

        let player = PlayerBuilder::new()
            .with_movie(movie)
            .with_renderer(renderer)
            // Software decoder for the embedded H.263/VP6 video.
            .with_video(SoftwareVideoBackend::new())
            .with_viewport_dimensions(width, height, 1.0)
            .with_autoplay(true)
            .build();

        Self {
            player,
            frame_interval: 1.0 / frame_rate,
            accumulator: 0.0,
            last_tick: Instant::now(),
            frame: None,
        }
    }

    fn update(&mut self, message: Message) {
        match message {
            Message::Tick(now) => {
                self.accumulator += now.duration_since(self.last_tick).as_secs_f64();
                self.last_tick = now;

                let mut player = self.player.lock().unwrap();

                // Advance as many whole movie frames as real time allows (capped).
                let mut advanced = 0;
                while self.accumulator >= self.frame_interval && advanced < MAX_FRAMES_PER_UPDATE {
                    force_root_clip_play(&mut player);
                    player.preload(&mut ExecutionLimit::none());
                    player.run_frame();
                    self.accumulator -= self.frame_interval;
                    advanced += 1;
                }
                // If we fell far behind, drop the backlog instead of catching up forever.
                if self.accumulator > self.frame_interval {
                    self.accumulator = 0.0;
                }

                if advanced > 0 && player.needs_render() {
                    player.render();
                    // `RenderBackend: Any`, so upcast and downcast to the concrete
                    // offscreen backend to read the rendered pixels back.
                    let renderer = <dyn Any>::downcast_mut::<WgpuRenderBackend<TextureTarget>>(
                        player.renderer_mut(),
                    );
                    if let Some(rgba) = renderer.and_then(|r| r.capture_frame()) {
                        let (w, h) = (rgba.width(), rgba.height());
                        self.frame = Some(Handle::from_rgba(w, h, rgba.into_raw()));
                    }
                }
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let content: Element<'_, Message> = match &self.frame {
            Some(handle) => image(handle.clone())
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => text("Loading…").into(),
        };

        container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        // ~60fps timer; the accumulator above advances movie frames at the
        // movie's own (slower) frame rate.
        time::every(time::Duration::from_millis(16)).map(Message::Tick)
    }
}

/// Cheaply parse the SWF just to learn the stage size, so we can size the window
/// before the GUI (and the real player) are built inside `boot`.
fn movie_dimensions() -> (u32, u32) {
    let movie = ruffle_core::tag_utils::movie_from_path(SWF_PATH, None)
        .unwrap_or_else(|e| panic!("failed to load {SWF_PATH}: {e:?}"));
    let width = movie.width().to_pixels().ceil().max(1.0) as u32;
    let height = movie.height().to_pixels().ceil().max(1.0) as u32;
    (width, height)
}

fn main() -> iced::Result {
    // Surface Ruffle's warnings (unsupported features, etc.) on the console.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let (w, h) = movie_dimensions();
    let title = format!("iced + ruffle — {SWF_PATH}");

    iced::application(App::new, App::update, App::view)
        .title(move |_state: &App| title.clone())
        .window_size((w as f32, h as f32))
        .subscription(App::subscription)
        .run()
}

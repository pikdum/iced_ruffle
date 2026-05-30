//! `iced_ruffle` — an [iced](https://iced.rs) widget that plays Flash (`.swf`)
//! movies via [Ruffle](https://ruffle.rs).
//!
//! Load a movie with [`RufflePlayer::from_path`] (or [`RufflePlayer::from_bytes`]),
//! hold it in your application state, and drop a [`Ruffle`] widget into your
//! `view`. The widget drives playback, decodes frames, forwards mouse/keyboard
//! input, plays audio, and schedules its own redraws — no subscription needed.
//!
//! ```no_run
//! use iced_ruffle::{Ruffle, RufflePlayer};
//!
//! struct App { player: RufflePlayer }
//!
//! impl App {
//!     fn view(&self) -> iced::Element<'_, ()> {
//!         Ruffle::new(&self.player).into()
//!     }
//! }
//! ```
//!
//! ## How it works
//!
//! Ruffle renders the movie offscreen on its own wgpu device; each changed frame
//! is read back to RGBA and uploaded into a *persistent* texture on iced's device
//! by a custom `shader` widget (uploading only on change avoids the image-cache
//! thrash that flickers). The frame is letterboxed to the widget bounds, and
//! cursor coordinates are mapped back through that letterbox into stage space.
//!
//! Playback uses `Player::tick`, which needs a real audio backend to advance
//! audio-synced content — this crate ships a small cpal backend for that.

mod audio;
mod frame_widget;
mod input;

use std::any::Any;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::widget::shader::{self, Action};
use iced::{keyboard, mouse, window, Element, Event, Length, Rectangle};

use ruffle_core::events::{MouseButton, MouseWheelDelta, PlayerEvent};
use ruffle_core::{FloatDuration, Player, PlayerBuilder, ViewportDimensions};
use ruffle_render_wgpu::backend::WgpuRenderBackend;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_render_wgpu::wgpu;
use ruffle_video_software::backend::SoftwareVideoBackend;

use frame_widget::{FrameData, FramePrimitive};
use input::{map_cursor, to_key_descriptor};

/// Errors that can occur loading a movie.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to load SWF: {0}")]
    Load(String),
    #[error("failed to create renderer: {0}")]
    Renderer(String),
}

/// A loaded Flash movie and its player. Hold one in your application state and
/// render it with [`Ruffle`]. Cheap to share by reference; all interior state is
/// behind locks so the widget can drive it through `&RufflePlayer`.
pub struct RufflePlayer {
    player: Arc<Mutex<Player>>,
    stage_w: u32,
    stage_h: u32,
    shared: Mutex<Shared>,
    /// The widget's last-known on-screen size in *physical* pixels plus the
    /// viewport scale factor `(width, height, scale)`. Written by the shader
    /// primitive's `prepare` (the only place that knows the real pixel size and
    /// HiDPI scale) and read by `advance` to size the offscreen render target.
    viewport: Arc<Mutex<(f32, f32, f32)>>,
}

struct Shared {
    last_tick: Option<Instant>,
    frame: Option<Arc<FrameData>>,
    frame_hash: u64,
    paused: bool,
    /// Current offscreen render-target size in pixels. Starts at the native
    /// stage size and is grown/shrunk to match the widget's on-screen size so
    /// vectors are rasterized crisply instead of being upscaled from a raster.
    render: (u32, u32),
}

impl RufflePlayer {
    /// Load a movie from a file path.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, Error> {
        let movie = ruffle_core::tag_utils::movie_from_path(path.as_ref(), None)
            .map_err(|e| Error::Load(format!("{e:?}")))?;
        build(movie)
    }

    /// Load a movie from raw SWF bytes. `name` is used as the movie's URL.
    pub fn from_bytes(name: &str, data: &[u8]) -> Result<Self, Error> {
        let movie = ruffle_core::tag_utils::SwfMovie::from_data(data, name.to_string(), None)
            .map_err(|e| Error::Load(format!("{e:?}")))?;
        build(movie)
    }

    /// The movie's native stage size in pixels.
    pub fn size(&self) -> (u32, u32) {
        (self.stage_w, self.stage_h)
    }

    /// The current offscreen render-target size in pixels. This is the stage
    /// scaled to the widget's on-screen size and is the coordinate space
    /// Ruffle's viewport (and thus its mouse mapping) lives in — so cursor
    /// positions must be mapped into *this* space, not the native stage size.
    fn render_size(&self) -> (u32, u32) {
        self.shared.lock().unwrap().render
    }

    /// Whether playback is paused.
    pub fn paused(&self) -> bool {
        self.shared.lock().unwrap().paused
    }

    /// Pause or resume playback.
    pub fn set_paused(&self, paused: bool) {
        {
            let mut shared = self.shared.lock().unwrap();
            shared.paused = paused;
            if !paused {
                // Avoid a large dt jump when resuming.
                shared.last_tick = None;
            }
        }
        self.player.lock().unwrap().set_is_playing(!paused);
    }

    /// Access the underlying Ruffle player for advanced control.
    pub fn player(&self) -> &Arc<Mutex<Player>> {
        &self.player
    }

    /// Advance the movie by real elapsed time and capture a new frame if the
    /// rendered output changed. Called by the widget each redraw.
    fn advance(&self) {
        let (view_w, view_h, scale) = *self.viewport.lock().unwrap();
        let mut shared = self.shared.lock().unwrap();
        let mut player = self.player.lock().unwrap();

        // Re-rasterize at the widget's on-screen resolution. Flash art is
        // vector; rendering at the native stage size and letting iced upscale
        // the raster is what makes the output jagged. Resizing the offscreen
        // target to the displayed size (and rebuilding the stage matrices via
        // `set_viewport_dimensions`) tessellates the vectors crisply at that
        // resolution — the same trick the Ruffle desktop app uses on resize.
        let target = self.target_render_size(view_w, view_h, scale);
        let resized = target != shared.render;
        if resized {
            player.set_viewport_dimensions(ViewportDimensions {
                width: target.0,
                height: target.1,
                scale_factor: scale.max(0.1) as f64,
            });
            shared.render = target;
        }

        if shared.paused {
            // Paused content still needs a fresh, correctly-sized frame after a
            // resize so it doesn't stay blurry until playback resumes.
            if resized {
                player.render();
                if let Some((hash, frame)) = capture(&mut player) {
                    shared.frame_hash = hash;
                    shared.frame = Some(Arc::new(frame));
                }
            }
            return;
        }

        let now = Instant::now();
        let dt = match shared.last_tick {
            Some(t) => FloatDuration::from_std(now.duration_since(t)),
            None => FloatDuration::ZERO,
        };
        shared.last_tick = Some(now);

        player.tick(dt);
        if resized || player.needs_render() {
            player.render();
            if let Some((hash, frame)) = capture(&mut player) {
                if shared.frame_hash != hash {
                    shared.frame_hash = hash;
                    shared.frame = Some(Arc::new(frame));
                }
            }
        }
    }

    /// Size of the offscreen render target for the current on-screen size:
    /// the stage scaled (preserving aspect ratio) to fit the widget's physical
    /// pixels. Keeping the stage's aspect ratio means the existing shader-side
    /// letterbox and stage-space cursor mapping stay valid — only the raster
    /// gets sharper. Falls back to the native stage size before the widget has
    /// reported a size, and caps each axis so a stray huge size can't allocate
    /// (and read back) an enormous texture.
    fn target_render_size(&self, view_w: f32, view_h: f32, scale: f32) -> (u32, u32) {
        const MAX_DIM: f32 = 4096.0;
        let (sw, sh) = (self.stage_w as f32, self.stage_h as f32);
        let (pw, ph) = (view_w * scale, view_h * scale);
        if pw < 1.0 || ph < 1.0 || sw < 1.0 || sh < 1.0 {
            return (self.stage_w.max(1), self.stage_h.max(1));
        }
        let fit = (pw / sw).min(ph / sh);
        let w = (sw * fit).round().clamp(1.0, MAX_DIM) as u32;
        let h = (sh * fit).round().clamp(1.0, MAX_DIM) as u32;
        (w, h)
    }

    /// The current frame and its content hash (the texture "version").
    fn frame(&self) -> (u64, Option<Arc<FrameData>>) {
        let shared = self.shared.lock().unwrap();
        (shared.frame_hash, shared.frame.clone())
    }

    /// Forward an input event to the player.
    fn dispatch(&self, event: PlayerEvent) {
        self.player.lock().unwrap().handle_event(event);
    }
}

fn build(movie: ruffle_core::tag_utils::SwfMovie) -> Result<RufflePlayer, Error> {
    let stage_w = movie.width().to_pixels().ceil().max(1.0) as u32;
    let stage_h = movie.height().to_pixels().ceil().max(1.0) as u32;

    let renderer = WgpuRenderBackend::for_offscreen(
        (stage_w, stage_h),
        wgpu::Backends::PRIMARY,
        wgpu::PowerPreference::HighPerformance,
    )
    .map_err(|e| Error::Renderer(e.to_string()))?;

    let mut builder = PlayerBuilder::new()
        .with_movie(movie)
        .with_renderer(renderer)
        .with_video(SoftwareVideoBackend::new())
        .with_viewport_dimensions(stage_w, stage_h, 1.0)
        .with_autoplay(true);

    match audio::CpalAudioBackend::new() {
        Ok(audio) => builder = builder.with_audio(audio),
        Err(e) => tracing::warn!("audio unavailable, continuing muted: {e}"),
    }

    let player = builder.build();
    force_root_clip_play(&mut player.lock().unwrap());

    Ok(RufflePlayer {
        player,
        stage_w,
        stage_h,
        shared: Mutex::new(Shared {
            last_tick: None,
            frame: None,
            frame_hash: 0,
            paused: false,
            render: (stage_w, stage_h),
        }),
        viewport: Arc::new(Mutex::new((0.0, 0.0, 1.0))),
    })
}

/// Make sure the root timeline is playing — autoplay/`is_playing` alone doesn't
/// resume a root MovieClip that starts stopped. Called once at load so we don't
/// override a later, intentional `stop()` from the movie's own code.
fn force_root_clip_play(player: &mut Player) {
    if !player.is_playing() {
        player.set_is_playing(true);
    }
    player.mutate_with_update_context(|ctx| {
        if let Some(root_clip) = ctx.stage.root_clip() {
            if let Some(mc) = root_clip.as_movie_clip() {
                if !mc.playing() {
                    mc.play();
                }
            }
        }
    });
}

/// Read back the rendered frame as RGBA plus a content hash.
fn capture(player: &mut Player) -> Option<(u64, FrameData)> {
    let renderer =
        <dyn Any>::downcast_mut::<WgpuRenderBackend<TextureTarget>>(player.renderer_mut());
    let rgba = renderer?.capture_frame()?;
    let (width, height) = (rgba.width(), rgba.height());
    let data = rgba.into_raw();
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    Some((
        hasher.finish(),
        FrameData {
            width,
            height,
            data,
        },
    ))
}

// ---------------------------------------------------------------------------
// Widget
// ---------------------------------------------------------------------------

/// An iced widget that displays and drives a [`RufflePlayer`].
pub struct Ruffle<'a, Message> {
    player: &'a RufflePlayer,
    width: Length,
    height: Length,
    _message: PhantomData<Message>,
}

impl<'a, Message> Ruffle<'a, Message> {
    pub fn new(player: &'a RufflePlayer) -> Self {
        Self {
            player,
            width: Length::Fill,
            height: Length::Fill,
            _message: PhantomData,
        }
    }

    pub fn width(mut self, width: impl Into<Length>) -> Self {
        self.width = width.into();
        self
    }

    pub fn height(mut self, height: impl Into<Length>) -> Self {
        self.height = height.into();
        self
    }
}

/// The internal `shader::Program` that draws frames and drives the player.
struct RuffleProgram<'a> {
    player: &'a RufflePlayer,
}

impl RuffleProgram<'_> {
    fn on_mouse<Message>(
        &self,
        event: &mouse::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        // Map into the render-target pixel space (what Ruffle's viewport uses),
        // not the native stage size — otherwise input is mis-scaled by the
        // stage-to-render ratio once we render above native resolution.
        let stage = self.player.render_size();
        let pos = cursor.position_in(bounds);
        match event {
            mouse::Event::CursorMoved { .. } => {
                if let Some(p) = pos {
                    let (x, y) = map_cursor(bounds.size(), stage, p);
                    self.player.dispatch(PlayerEvent::MouseMove { x, y });
                }
            }
            mouse::Event::ButtonPressed(b) => {
                if let (Some(p), Some(button)) = (pos, map_button(*b)) {
                    let (x, y) = map_cursor(bounds.size(), stage, p);
                    self.player.dispatch(PlayerEvent::MouseDown {
                        x,
                        y,
                        button,
                        index: None,
                    });
                }
            }
            mouse::Event::ButtonReleased(b) => {
                if let (Some(p), Some(button)) = (pos, map_button(*b)) {
                    let (x, y) = map_cursor(bounds.size(), stage, p);
                    self.player.dispatch(PlayerEvent::MouseUp { x, y, button });
                }
            }
            mouse::Event::WheelScrolled { delta } => {
                let delta = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => MouseWheelDelta::Lines(*y as f64),
                    mouse::ScrollDelta::Pixels { y, .. } => MouseWheelDelta::Pixels(*y as f64),
                };
                self.player.dispatch(PlayerEvent::MouseWheel { delta });
            }
            mouse::Event::CursorLeft => self.player.dispatch(PlayerEvent::MouseLeave),
            _ => return None,
        }
        Some(Action::request_redraw())
    }

    fn on_key<Message>(&self, event: &keyboard::Event) -> Option<Action<Message>> {
        match event {
            keyboard::Event::KeyPressed {
                key,
                location,
                text,
                ..
            } => {
                if let Some(desc) = to_key_descriptor(key, *location) {
                    self.player.dispatch(PlayerEvent::KeyDown { key: desc });
                }
                if let Some(text) = text {
                    for codepoint in text.chars() {
                        self.player.dispatch(PlayerEvent::TextInput { codepoint });
                    }
                }
            }
            keyboard::Event::KeyReleased { key, location, .. } => {
                if let Some(desc) = to_key_descriptor(key, *location) {
                    self.player.dispatch(PlayerEvent::KeyUp { key: desc });
                }
            }
            keyboard::Event::ModifiersChanged(_) => return None,
        }
        Some(Action::request_redraw())
    }
}

impl<Message> shader::Program<Message> for RuffleProgram<'_> {
    type State = ();
    type Primitive = FramePrimitive;

    fn draw(&self, _state: &(), _cursor: mouse::Cursor, _bounds: Rectangle) -> FramePrimitive {
        let (version, frame) = self.player.frame();
        FramePrimitive::new(version, frame, self.player.viewport.clone())
    }

    fn update(
        &self,
        _state: &mut (),
        event: &Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Window(window::Event::RedrawRequested(_)) => {
                self.player.advance();
                Some(if self.player.paused() {
                    // Idle: poll occasionally so resume/input stays responsive.
                    Action::request_redraw_at(Instant::now() + Duration::from_millis(50))
                } else {
                    Action::request_redraw()
                })
            }
            Event::Mouse(m) => self.on_mouse(m, bounds, cursor),
            Event::Keyboard(k) => self.on_key(k),
            _ => None,
        }
    }
}

fn map_button(button: mouse::Button) -> Option<MouseButton> {
    match button {
        mouse::Button::Left => Some(MouseButton::Left),
        mouse::Button::Right => Some(MouseButton::Right),
        mouse::Button::Middle => Some(MouseButton::Middle),
        _ => None,
    }
}

impl<'a, Message> From<Ruffle<'a, Message>> for Element<'a, Message>
where
    Message: 'a,
{
    fn from(ruffle: Ruffle<'a, Message>) -> Self {
        iced::widget::shader(RuffleProgram {
            player: ruffle.player,
        })
        .width(ruffle.width)
        .height(ruffle.height)
        .into()
    }
}

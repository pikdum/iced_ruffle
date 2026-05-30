//! Embed Ruffle (a Flash emulator) inside an iced GUI: open a `.swf`, play it,
//! and interact with it (mouse + keyboard), with audio and a resizable window.
//!
//! Ruffle and iced both render with wgpu, but rather than share a wgpu device
//! (fiddly and version-sensitive) the two are decoupled through CPU memory:
//!
//!   Ruffle  --render-->  offscreen wgpu texture  --capture_frame-->  RGBA pixels
//!                                                                       |
//!   iced  <--image widget--  image::Handle::from_rgba  <----------------+
//!
//! Input flows the other way: iced mouse/keyboard events are translated into
//! Ruffle `PlayerEvent`s and fed to the player.
//!
//! Notes:
//!   * The movie is driven by `Player::tick(dt)`, which advances frames on a
//!     timer AND keeps audio/video in sync. This needs a real audio backend —
//!     under the null backend, tick() stalls on audio-synced (e.g. embedded
//!     video) content. See `audio::CpalAudioBackend`.
//!   * Ruffle renders at the SWF's native stage size; iced scales the bitmap to
//!     fit the window (letterboxed). Cursor coordinates are mapped back into
//!     stage space for input.

mod audio;

use std::any::Any;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use iced::widget::image::{FilterMethod, Handle};
use iced::widget::{button, column, container, image, mouse_area, row, text};
use iced::{
    keyboard, mouse, time, window, Center, Color, ContentFit, Element, Length, Size, Subscription,
    Task,
};

use ruffle_core::events::{
    KeyDescriptor, KeyLocation, LogicalKey, MouseButton, MouseWheelDelta, NamedKey, PhysicalKey,
    PlayerEvent,
};
use ruffle_core::{FloatDuration, Player, PlayerBuilder};
use ruffle_render_wgpu::backend::WgpuRenderBackend;
use ruffle_render_wgpu::target::TextureTarget;
use ruffle_render_wgpu::wgpu;
use ruffle_video_software::backend::SoftwareVideoBackend;

/// Height reserved for the top toolbar (logical px). Kept fixed so we can map
/// cursor coordinates into the player area deterministically.
const TOOLBAR_H: f32 = 38.0;

/// A loaded movie and everything needed to drive and display it.
struct Session {
    player: Arc<Mutex<Player>>,
    stage_w: u32,
    stage_h: u32,
    last_tick: Instant,
    frame: Option<Handle>,
    /// Hash of the last displayed frame. Ruffle reports `needs_render` on nearly
    /// every tick even when the pixels are identical (e.g. a static movie), so we
    /// only hand iced a new texture when the content actually changed — otherwise
    /// re-uploading the bitmap every frame flickers.
    last_hash: Option<u64>,
}

struct App {
    session: Option<Session>,
    /// Current window size (logical px), tracked via resize events.
    window_size: Size,
    /// Last cursor position mapped into stage coordinates.
    cursor_stage: (f64, f64),
    status: String,
}

#[derive(Debug, Clone)]
enum Message {
    Tick(Instant),
    Open,
    Opened(Option<PathBuf>),
    Resized(Size),
    MouseMoved(iced::Point),
    MousePressed,
    MouseReleased,
    RightPressed,
    RightReleased,
    Scrolled(mouse::ScrollDelta),
    MouseExited,
    Key(keyboard::Event),
}

// ---------------------------------------------------------------------------
// Loading & driving the player
// ---------------------------------------------------------------------------

/// Make sure the root timeline starts playing — autoplay/`is_playing` alone
/// doesn't resume a root MovieClip that starts stopped. Called once at load so we
/// don't override a later, intentional `stop()` from the movie's own code.
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

fn load_session(path: impl AsRef<Path>) -> Result<Session, String> {
    let path = path.as_ref();
    let movie = ruffle_core::tag_utils::movie_from_path(path, None)
        .map_err(|e| format!("Failed to load {}: {e:?}", path.display()))?;

    let stage_w = movie.width().to_pixels().ceil().max(1.0) as u32;
    let stage_h = movie.height().to_pixels().ceil().max(1.0) as u32;

    let renderer = WgpuRenderBackend::for_offscreen(
        (stage_w, stage_h),
        wgpu::Backends::PRIMARY,
        wgpu::PowerPreference::HighPerformance,
    )
    .map_err(|e| format!("Failed to create renderer: {e}"))?;

    let mut builder = PlayerBuilder::new()
        .with_movie(movie)
        .with_renderer(renderer)
        .with_video(SoftwareVideoBackend::new())
        .with_viewport_dimensions(stage_w, stage_h, 1.0)
        .with_autoplay(true);

    match audio::CpalAudioBackend::new() {
        Ok(audio) => builder = builder.with_audio(audio),
        Err(e) => tracing::warn!("Audio unavailable, continuing muted: {e}"),
    }

    let player = builder.build();
    force_root_clip_play(&mut player.lock().unwrap());

    Ok(Session {
        player,
        stage_w,
        stage_h,
        last_tick: Instant::now(),
        frame: None,
        last_hash: None,
    })
}

/// Pull the freshly rendered frame out of the offscreen backend as an iced image,
/// along with a hash of its pixels (for change detection).
fn capture(player: &mut Player) -> Option<(u64, Handle)> {
    let renderer =
        <dyn Any>::downcast_mut::<WgpuRenderBackend<TextureTarget>>(player.renderer_mut());
    let rgba = renderer?.capture_frame()?;
    let (w, h) = (rgba.width(), rgba.height());
    let raw = rgba.into_raw();
    let mut hasher = DefaultHasher::new();
    raw.hash(&mut hasher);
    Some((hasher.finish(), Handle::from_rgba(w, h, raw)))
}

// ---------------------------------------------------------------------------
// Coordinate + key mapping
// ---------------------------------------------------------------------------

/// Map a cursor position (relative to the player area) into the SWF's stage
/// coordinate space, accounting for the letterbox produced by `ContentFit::Contain`.
fn map_cursor(window: Size, stage: (u32, u32), p: iced::Point) -> (f64, f64) {
    let area_w = window.width as f64;
    let area_h = (window.height - TOOLBAR_H).max(1.0) as f64;
    let (sw, sh) = (stage.0 as f64, stage.1 as f64);
    let scale = (area_w / sw).min(area_h / sh);
    let (disp_w, disp_h) = (sw * scale, sh * scale);
    let (off_x, off_y) = ((area_w - disp_w) / 2.0, (area_h - disp_h) / 2.0);
    let x = ((p.x as f64 - off_x) / scale).clamp(0.0, sw);
    let y = ((p.y as f64 - off_y) / scale).clamp(0.0, sh);
    (x, y)
}

/// Translate an iced key into a Ruffle `KeyDescriptor`, synthesizing the physical
/// key from the logical one for the common set (letters, digits, arrows, etc.).
fn to_key_descriptor(key: &keyboard::Key, location: keyboard::Location) -> Option<KeyDescriptor> {
    use keyboard::key::Named;

    let key_location = match location {
        keyboard::Location::Standard => KeyLocation::Standard,
        keyboard::Location::Left => KeyLocation::Left,
        keyboard::Location::Right => KeyLocation::Right,
        keyboard::Location::Numpad => KeyLocation::Numpad,
    };

    let (physical_key, logical_key) = match key {
        keyboard::Key::Character(s) => {
            let c = s.chars().next()?;
            let physical = match c.to_ascii_lowercase() {
                'a' => PhysicalKey::KeyA,
                'b' => PhysicalKey::KeyB,
                'c' => PhysicalKey::KeyC,
                'd' => PhysicalKey::KeyD,
                'e' => PhysicalKey::KeyE,
                'f' => PhysicalKey::KeyF,
                'g' => PhysicalKey::KeyG,
                'h' => PhysicalKey::KeyH,
                'i' => PhysicalKey::KeyI,
                'j' => PhysicalKey::KeyJ,
                'k' => PhysicalKey::KeyK,
                'l' => PhysicalKey::KeyL,
                'm' => PhysicalKey::KeyM,
                'n' => PhysicalKey::KeyN,
                'o' => PhysicalKey::KeyO,
                'p' => PhysicalKey::KeyP,
                'q' => PhysicalKey::KeyQ,
                'r' => PhysicalKey::KeyR,
                's' => PhysicalKey::KeyS,
                't' => PhysicalKey::KeyT,
                'u' => PhysicalKey::KeyU,
                'v' => PhysicalKey::KeyV,
                'w' => PhysicalKey::KeyW,
                'x' => PhysicalKey::KeyX,
                'y' => PhysicalKey::KeyY,
                'z' => PhysicalKey::KeyZ,
                '0' => PhysicalKey::Digit0,
                '1' => PhysicalKey::Digit1,
                '2' => PhysicalKey::Digit2,
                '3' => PhysicalKey::Digit3,
                '4' => PhysicalKey::Digit4,
                '5' => PhysicalKey::Digit5,
                '6' => PhysicalKey::Digit6,
                '7' => PhysicalKey::Digit7,
                '8' => PhysicalKey::Digit8,
                '9' => PhysicalKey::Digit9,
                _ => PhysicalKey::Unknown,
            };
            (physical, LogicalKey::Character(c))
        }
        keyboard::Key::Named(named) => match named {
            Named::ArrowUp => (PhysicalKey::ArrowUp, LogicalKey::Named(NamedKey::ArrowUp)),
            Named::ArrowDown => (
                PhysicalKey::ArrowDown,
                LogicalKey::Named(NamedKey::ArrowDown),
            ),
            Named::ArrowLeft => (
                PhysicalKey::ArrowLeft,
                LogicalKey::Named(NamedKey::ArrowLeft),
            ),
            Named::ArrowRight => (
                PhysicalKey::ArrowRight,
                LogicalKey::Named(NamedKey::ArrowRight),
            ),
            Named::Space => (PhysicalKey::Space, LogicalKey::Character(' ')),
            Named::Enter => (PhysicalKey::Enter, LogicalKey::Named(NamedKey::Enter)),
            Named::Backspace => (
                PhysicalKey::Backspace,
                LogicalKey::Named(NamedKey::Backspace),
            ),
            Named::Tab => (PhysicalKey::Tab, LogicalKey::Named(NamedKey::Tab)),
            Named::Escape => (PhysicalKey::Escape, LogicalKey::Named(NamedKey::Escape)),
            Named::Delete => (PhysicalKey::Delete, LogicalKey::Named(NamedKey::Delete)),
            Named::Shift => {
                let p = if key_location == KeyLocation::Right {
                    PhysicalKey::ShiftRight
                } else {
                    PhysicalKey::ShiftLeft
                };
                (p, LogicalKey::Named(NamedKey::Shift))
            }
            Named::Control => {
                let p = if key_location == KeyLocation::Right {
                    PhysicalKey::ControlRight
                } else {
                    PhysicalKey::ControlLeft
                };
                (p, LogicalKey::Named(NamedKey::Control))
            }
            Named::Alt => {
                let p = if key_location == KeyLocation::Right {
                    PhysicalKey::AltRight
                } else {
                    PhysicalKey::AltLeft
                };
                (p, LogicalKey::Named(NamedKey::Alt))
            }
            _ => return None,
        },
        _ => return None,
    };

    Some(KeyDescriptor {
        physical_key,
        logical_key,
        key_location,
    })
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

impl App {
    fn new(initial: Option<Session>, window_size: Size) -> Self {
        let status = match &initial {
            Some(_) => String::new(),
            None => "Open a .swf file to begin".to_string(),
        };
        App {
            session: initial,
            window_size,
            cursor_stage: (0.0, 0.0),
            status,
        }
    }

    /// Send a player event to the current session (if any).
    fn dispatch(&self, event: PlayerEvent) {
        if let Some(s) = &self.session {
            s.player.lock().unwrap().handle_event(event);
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick(now) => {
                if let Some(s) = &mut self.session {
                    let dt = FloatDuration::from_std(now.duration_since(s.last_tick));
                    s.last_tick = now;
                    let mut player = s.player.lock().unwrap();
                    player.tick(dt);
                    let mut updated = None;
                    if player.needs_render() {
                        player.render();
                        if let Some((hash, handle)) = capture(&mut player) {
                            if s.last_hash != Some(hash) {
                                updated = Some((hash, handle));
                            }
                        }
                    }
                    drop(player);
                    if let Some((hash, handle)) = updated {
                        s.last_hash = Some(hash);
                        s.frame = Some(handle);
                    }
                }
            }
            Message::Open => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .add_filter("Flash movie", &["swf"])
                            .set_title("Open SWF")
                            .pick_file()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    Message::Opened,
                );
            }
            Message::Opened(Some(path)) => match load_session(&path) {
                Ok(session) => {
                    self.status = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.session = Some(session);
                }
                Err(e) => {
                    tracing::error!("{e}");
                    self.status = e;
                }
            },
            Message::Opened(None) => {}
            Message::Resized(size) => self.window_size = size,
            Message::MouseMoved(p) => {
                if let Some(stage) = self.session.as_ref().map(|s| (s.stage_w, s.stage_h)) {
                    let (x, y) = map_cursor(self.window_size, stage, p);
                    self.cursor_stage = (x, y);
                    self.dispatch(PlayerEvent::MouseMove { x, y });
                }
            }
            Message::MousePressed => {
                let (x, y) = self.cursor_stage;
                self.dispatch(PlayerEvent::MouseDown {
                    x,
                    y,
                    button: MouseButton::Left,
                    index: None,
                });
            }
            Message::MouseReleased => {
                let (x, y) = self.cursor_stage;
                self.dispatch(PlayerEvent::MouseUp {
                    x,
                    y,
                    button: MouseButton::Left,
                });
            }
            Message::RightPressed => {
                let (x, y) = self.cursor_stage;
                self.dispatch(PlayerEvent::MouseDown {
                    x,
                    y,
                    button: MouseButton::Right,
                    index: None,
                });
            }
            Message::RightReleased => {
                let (x, y) = self.cursor_stage;
                self.dispatch(PlayerEvent::MouseUp {
                    x,
                    y,
                    button: MouseButton::Right,
                });
            }
            Message::Scrolled(delta) => {
                let delta = match delta {
                    mouse::ScrollDelta::Lines { y, .. } => MouseWheelDelta::Lines(y as f64),
                    mouse::ScrollDelta::Pixels { y, .. } => MouseWheelDelta::Pixels(y as f64),
                };
                self.dispatch(PlayerEvent::MouseWheel { delta });
            }
            Message::MouseExited => self.dispatch(PlayerEvent::MouseLeave),
            Message::Key(event) => match event {
                keyboard::Event::KeyPressed {
                    key,
                    location,
                    text,
                    ..
                } => {
                    if let Some(desc) = to_key_descriptor(&key, location) {
                        self.dispatch(PlayerEvent::KeyDown { key: desc });
                    }
                    if let Some(text) = text {
                        for codepoint in text.chars() {
                            self.dispatch(PlayerEvent::TextInput { codepoint });
                        }
                    }
                }
                keyboard::Event::KeyReleased { key, location, .. } => {
                    if let Some(desc) = to_key_descriptor(&key, location) {
                        self.dispatch(PlayerEvent::KeyUp { key: desc });
                    }
                }
                keyboard::Event::ModifiersChanged(_) => {}
            },
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let toolbar = container(
            row![
                button(text("Open\u{2026}")).on_press(Message::Open),
                text(&self.status),
            ]
            .spacing(12)
            .align_y(Center),
        )
        .height(Length::Fixed(TOOLBAR_H))
        .padding([6, 10]);

        let stage: Element<'_, Message> = match self.session.as_ref().and_then(|s| s.frame.clone())
        {
            Some(handle) => image(handle)
                .content_fit(ContentFit::Contain)
                .filter_method(FilterMethod::Linear)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            None => text("No movie loaded").into(),
        };

        let player_area = mouse_area(
            container(stage)
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .style(|_| container::Style {
                    background: Some(Color::BLACK.into()),
                    ..container::Style::default()
                }),
        )
        .on_move(Message::MouseMoved)
        .on_press(Message::MousePressed)
        .on_release(Message::MouseReleased)
        .on_right_press(Message::RightPressed)
        .on_right_release(Message::RightReleased)
        .on_scroll(Message::Scrolled)
        .on_exit(Message::MouseExited);

        column![toolbar, player_area].into()
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            time::every(time::Duration::from_millis(16)).map(Message::Tick),
            keyboard::listen().map(Message::Key),
            window::resize_events().map(|(_id, size)| Message::Resized(size)),
        ])
    }
}

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    // Optional SWF path on the command line. Probe its size cheaply so the window
    // can open at the right dimensions; `boot` does the real load once.
    let arg = std::env::args().nth(1);
    let window_size = arg
        .as_ref()
        .and_then(|p| ruffle_core::tag_utils::movie_from_path(p, None).ok())
        .map(|m| {
            Size::new(
                m.width().to_pixels().ceil().max(1.0) as f32,
                m.height().to_pixels().ceil().max(1.0) as f32 + TOOLBAR_H,
            )
        })
        .unwrap_or(Size::new(960.0, 640.0));

    let boot = move || {
        let initial = arg.as_ref().and_then(|p| match load_session(p) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::error!("{e}");
                None
            }
        });
        App::new(initial, window_size)
    };

    iced::application(boot, App::update, App::view)
        .title("iced + ruffle")
        .window_size(window_size)
        .resizable(true)
        .subscription(App::subscription)
        .run()
}

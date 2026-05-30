//! Example host for the `iced_ruffle` widget: a tiny Flash player with an
//! "Open…" button. Note there's no subscription and no input handling here — the
//! `Ruffle` widget drives playback, input, audio, and redraws on its own.
//!
//!   cargo run --example player              # opens empty; click "Open…"
//!   cargo run --example player -- cat.swf   # load a movie directly

use std::path::PathBuf;

use iced::widget::{button, column, container, row, text};
use iced::{Center, Color, Element, Length, Task};
use iced_ruffle::{Ruffle, RufflePlayer};

const TOOLBAR_H: f32 = 38.0;

struct App {
    player: Option<RufflePlayer>,
    status: String,
}

#[derive(Debug, Clone)]
enum Message {
    Open,
    Opened(Option<PathBuf>),
}

impl App {
    fn new(initial: Option<RufflePlayer>) -> Self {
        let status = if initial.is_some() {
            String::new()
        } else {
            "Open a .swf file to begin".to_string()
        };
        App {
            player: initial,
            status,
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
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
            Message::Opened(Some(path)) => match RufflePlayer::from_path(&path) {
                Ok(player) => {
                    self.status = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.player = Some(player);
                }
                Err(e) => {
                    tracing::error!("{e}");
                    self.status = e.to_string();
                }
            },
            Message::Opened(None) => {}
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

        let stage: Element<'_, Message> = match &self.player {
            Some(player) => container(Ruffle::new(player))
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|_| container::Style {
                    background: Some(Color::BLACK.into()),
                    ..container::Style::default()
                })
                .into(),
            None => container(text("No movie loaded"))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into(),
        };

        column![toolbar, stage].into()
    }
}

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let arg = std::env::args().nth(1);
    let boot = move || {
        let initial = arg.as_ref().and_then(|p| match RufflePlayer::from_path(p) {
            Ok(player) => Some(player),
            Err(e) => {
                tracing::error!("{e}");
                None
            }
        });
        App::new(initial)
    };

    iced::application(boot, App::update, App::view)
        .title("iced_ruffle player")
        .window_size((960.0, 640.0))
        .resizable(true)
        .run()
}

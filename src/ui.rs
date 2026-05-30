//! A minimal Ruffle [`UiBackend`] whose only job is to capture the mouse
//! cursor the movie asks for, so the widget can reflect it (a hand over
//! buttons/links, an I-beam over text, etc.). Flash drives the cursor by
//! calling `set_mouse_cursor`; the default `NullUiBackend` drops it, so without
//! this the pointer never changes on hover. Everything else delegates to the
//! null backend — we don't implement clipboard, dialogs, fonts, or fullscreen.

use std::sync::{Arc, Mutex};

use ruffle_core::backend::ui::{
    DialogResultFuture, FileFilter, FontDefinition, FullscreenError, LanguageIdentifier,
    MouseCursor, NullUiBackend, UiBackend,
};
use ruffle_core::font::FontQuery;
use url::Url;

/// The cursor the movie currently wants, shared with the widget. Updated on the
/// player thread via `set_mouse_cursor`; read by the widget's `mouse_interaction`.
pub type CursorState = Arc<Mutex<MouseCursor>>;

/// A `UiBackend` that records the requested cursor and delegates the rest.
pub struct CursorUi {
    cursor: CursorState,
    inner: NullUiBackend,
}

impl CursorUi {
    pub fn new(cursor: CursorState) -> Self {
        Self {
            cursor,
            inner: NullUiBackend::new(),
        }
    }
}

impl UiBackend for CursorUi {
    fn set_mouse_cursor(&mut self, cursor: MouseCursor) {
        *self.cursor.lock().unwrap() = cursor;
    }

    fn mouse_visible(&self) -> bool {
        self.inner.mouse_visible()
    }

    fn set_mouse_visible(&mut self, visible: bool) {
        self.inner.set_mouse_visible(visible);
    }

    fn clipboard_content(&mut self) -> String {
        self.inner.clipboard_content()
    }

    fn set_clipboard_content(&mut self, content: String) {
        self.inner.set_clipboard_content(content);
    }

    fn set_fullscreen(&mut self, is_full: bool) -> Result<(), FullscreenError> {
        self.inner.set_fullscreen(is_full)
    }

    fn display_root_movie_download_failed_message(&self, invalid_swf: bool, fetched_error: String) {
        self.inner
            .display_root_movie_download_failed_message(invalid_swf, fetched_error);
    }

    fn message(&self, message: &str) {
        self.inner.message(message);
    }

    fn open_virtual_keyboard(&self) {
        self.inner.open_virtual_keyboard();
    }

    fn close_virtual_keyboard(&self) {
        self.inner.close_virtual_keyboard();
    }

    fn language(&self) -> LanguageIdentifier {
        self.inner.language()
    }

    fn display_unsupported_video(&self, url: Url) {
        self.inner.display_unsupported_video(url);
    }

    fn load_device_font(&self, query: &FontQuery, register: &mut dyn FnMut(FontDefinition)) {
        self.inner.load_device_font(query, register);
    }

    fn sort_device_fonts(
        &self,
        query: &FontQuery,
        register: &mut dyn FnMut(FontDefinition),
    ) -> Vec<FontQuery> {
        self.inner.sort_device_fonts(query, register)
    }

    fn display_file_open_dialog(&mut self, filters: Vec<FileFilter>) -> Option<DialogResultFuture> {
        self.inner.display_file_open_dialog(filters)
    }

    fn display_file_save_dialog(
        &mut self,
        file_name: String,
        title: String,
    ) -> Option<DialogResultFuture> {
        self.inner.display_file_save_dialog(file_name, title)
    }

    fn close_file_dialog(&mut self) {
        self.inner.close_file_dialog();
    }
}

//! The page's key presses as the app's keys, so every shortcut works as in the terminal.

use correctsound::app::keys::{KeyCode, KeyEvent, KeyModifiers};
use web_sys::KeyboardEvent;

/// The app's key for a browser key press; `None` for keys the app has no use for, and for
/// the browser's own shortcuts (with Ctrl, Alt or ⌘: zoom, reload, copy), which stay the
/// browser's.
#[must_use]
pub fn convert(e: &KeyboardEvent) -> Option<KeyEvent> {
    if e.ctrl_key() || e.alt_key() || e.meta_key() {
        return None;
    }
    let shift = e.shift_key();
    let code = match e.key().as_str() {
        "Enter" => KeyCode::Enter,
        "Escape" => KeyCode::Esc,
        "Tab" if shift => KeyCode::BackTab,
        "Tab" => KeyCode::Tab,
        "Backspace" => KeyCode::Backspace,
        "Delete" => KeyCode::Delete,
        "ArrowUp" => KeyCode::Up,
        "ArrowDown" => KeyCode::Down,
        "ArrowLeft" => KeyCode::Left,
        "ArrowRight" => KeyCode::Right,
        "PageUp" => KeyCode::PageUp,
        "PageDown" => KeyCode::PageDown,
        "Home" => KeyCode::Home,
        "End" => KeyCode::End,
        // A character: one, not a named key such as "Shift" or "F5".
        key => {
            let mut chars = key.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            KeyCode::Char(c)
        }
    };
    let modifiers = if shift {
        KeyModifiers::SHIFT
    } else {
        KeyModifiers::NONE
    };
    Some(KeyEvent::new(code, modifiers))
}

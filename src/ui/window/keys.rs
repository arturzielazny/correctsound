//! The window's key presses as the app's keys (the terminal's), so every shortcut works the
//! same in both.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// The app's key for a pressed `key` with `mods` held; `None` for keys the app has no use
/// for (a modifier on its own, function keys).
#[must_use]
pub fn convert(key: &Key, mods: ModifiersState) -> Option<KeyEvent> {
    let mut modifiers = KeyModifiers::NONE;
    if mods.shift_key() {
        modifiers |= KeyModifiers::SHIFT;
    }
    if mods.control_key() {
        modifiers |= KeyModifiers::CONTROL;
    }
    if mods.alt_key() {
        modifiers |= KeyModifiers::ALT;
    }
    let code = match key {
        Key::Character(s) => KeyCode::Char(s.chars().next()?),
        Key::Named(named) => match named {
            NamedKey::Enter => KeyCode::Enter,
            NamedKey::Escape => KeyCode::Esc,
            NamedKey::Tab if mods.shift_key() => KeyCode::BackTab,
            NamedKey::Tab => KeyCode::Tab,
            NamedKey::Backspace => KeyCode::Backspace,
            NamedKey::Delete => KeyCode::Delete,
            NamedKey::Space => KeyCode::Char(' '),
            NamedKey::ArrowUp => KeyCode::Up,
            NamedKey::ArrowDown => KeyCode::Down,
            NamedKey::ArrowLeft => KeyCode::Left,
            NamedKey::ArrowRight => KeyCode::Right,
            NamedKey::PageUp => KeyCode::PageUp,
            NamedKey::PageDown => KeyCode::PageDown,
            NamedKey::Home => KeyCode::Home,
            NamedKey::End => KeyCode::End,
            _ => return None,
        },
        _ => return None,
    };
    Some(KeyEvent::new(code, modifiers))
}

/// What a zoom shortcut does to the text size: Ctrl with +, − or 0, as in terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    In,
    Out,
    Reset,
}

/// The zoom a press asks for, if it is one.
#[must_use]
pub fn zoom(key: &Key, mods: ModifiersState) -> Option<Zoom> {
    if !mods.control_key() {
        return None;
    }
    match key {
        Key::Character(s) => match s.as_str() {
            "+" | "=" => Some(Zoom::In),
            "-" => Some(Zoom::Out),
            "0" => Some(Zoom::Reset),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_keys_are_the_terminals_keys() {
        let none = ModifiersState::empty();
        let key = |k: Key| convert(&k, none).map(|e| e.code);
        assert_eq!(key(Key::Character("h".into())), Some(KeyCode::Char('h')));
        assert_eq!(key(Key::Character("T".into())), Some(KeyCode::Char('T')));
        assert_eq!(key(Key::Named(NamedKey::Space)), Some(KeyCode::Char(' ')));
        assert_eq!(key(Key::Named(NamedKey::Enter)), Some(KeyCode::Enter));
        assert_eq!(key(Key::Named(NamedKey::ArrowLeft)), Some(KeyCode::Left));
        assert_eq!(key(Key::Named(NamedKey::Shift)), None);
        let ctrl_c =
            convert(&Key::Character("c".into()), ModifiersState::CONTROL).expect("Ctrl+C is a key");
        assert_eq!(ctrl_c.code, KeyCode::Char('c'));
        assert!(ctrl_c.modifiers.contains(KeyModifiers::CONTROL));
        assert_eq!(
            convert(&Key::Named(NamedKey::Tab), ModifiersState::SHIFT).map(|e| e.code),
            Some(KeyCode::BackTab)
        );
        assert_eq!(
            zoom(&Key::Character("=".into()), ModifiersState::CONTROL),
            Some(Zoom::In)
        );
        assert_eq!(zoom(&Key::Character("=".into()), none), None);
    }
}

use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Encodes a key press as the bytes an xterm-compatible terminal would send.
pub fn encode(key: &KeyEvent, mode: TermMode) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let mods = key.modifiers;
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let alt = mods.contains(KeyModifiers::ALT);
    let shift = mods.contains(KeyModifiers::SHIFT);
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let modifier_param = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);

    let with_alt = |mut bytes: Vec<u8>| {
        if alt {
            bytes.insert(0, 0x1b);
        }
        bytes
    };

    let cursor_key = |c: char| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[1;{modifier_param}{c}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };

    let tilde_key = |n: u8| -> Vec<u8> {
        if modifier_param > 1 {
            format!("\x1b[{n};{modifier_param}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };

    let bytes = match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                let b = ctrl_byte(c)?;
                with_alt(vec![b])
            } else {
                let c = if shift && c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c };
                let mut buf = [0u8; 4];
                with_alt(c.encode_utf8(&mut buf).as_bytes().to_vec())
            }
        }
        KeyCode::Enter => {
            if mode.contains(TermMode::LINE_FEED_NEW_LINE) {
                with_alt(b"\r\n".to_vec())
            } else {
                with_alt(b"\r".to_vec())
            }
        }
        KeyCode::Tab => {
            if shift {
                b"\x1b[Z".to_vec()
            } else {
                with_alt(b"\t".to_vec())
            }
        }
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if ctrl {
                with_alt(vec![0x08])
            } else {
                with_alt(vec![0x7f])
            }
        }
        KeyCode::Esc => with_alt(vec![0x1b]),
        KeyCode::Up => cursor_key('A'),
        KeyCode::Down => cursor_key('B'),
        KeyCode::Right => cursor_key('C'),
        KeyCode::Left => cursor_key('D'),
        KeyCode::Home => cursor_key('H'),
        KeyCode::End => cursor_key('F'),
        KeyCode::Insert => tilde_key(2),
        KeyCode::Delete => tilde_key(3),
        KeyCode::PageUp => tilde_key(5),
        KeyCode::PageDown => tilde_key(6),
        KeyCode::F(n @ 1..=4) => {
            let c = (b'P' + n - 1) as char;
            if modifier_param > 1 {
                format!("\x1b[1;{modifier_param}{c}").into_bytes()
            } else {
                format!("\x1bO{c}").into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => {
            let code = match n {
                5 => 15,
                6 => 17,
                7 => 18,
                8 => 19,
                9 => 20,
                10 => 21,
                11 => 23,
                _ => 24,
            };
            tilde_key(code)
        }
        KeyCode::Null => vec![0],
        _ => return None,
    };
    Some(bytes)
}

fn ctrl_byte(c: char) -> Option<u8> {
    let c = c.to_ascii_lowercase();
    Some(match c {
        'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 27,
        '\\' | '4' => 28,
        ']' | '5' => 29,
        '^' | '6' => 30,
        '_' | '/' | '-' | '7' => 31,
        '?' | '8' => 127,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn enc(code: KeyCode, mods: KeyModifiers) -> Vec<u8> {
        encode(&k(code, mods), TermMode::default()).unwrap()
    }

    #[test]
    fn printable_and_control() {
        let none = KeyModifiers::empty();
        assert_eq!(enc(KeyCode::Char('a'), none), b"a");
        assert_eq!(enc(KeyCode::Char('ñ'), none), "ñ".as_bytes());
        assert_eq!(enc(KeyCode::Char('a'), KeyModifiers::SHIFT), b"A");
        assert_eq!(enc(KeyCode::Char('c'), KeyModifiers::CONTROL), vec![3]);
        assert_eq!(enc(KeyCode::Char(' '), KeyModifiers::CONTROL), vec![0]);
        assert_eq!(enc(KeyCode::Char('b'), KeyModifiers::ALT), b"\x1bb");
        assert_eq!(enc(KeyCode::Char('x'), KeyModifiers::CONTROL | KeyModifiers::ALT), vec![0x1b, 24]);
        assert_eq!(enc(KeyCode::Enter, none), b"\r");
        assert_eq!(enc(KeyCode::Backspace, none), vec![0x7f]);
        assert_eq!(enc(KeyCode::Tab, KeyModifiers::SHIFT), b"\x1b[Z");
    }

    #[test]
    fn cursor_keys_respect_mode() {
        let none = KeyModifiers::empty();
        assert_eq!(enc(KeyCode::Up, none), b"\x1b[A");
        let app = encode(&k(KeyCode::Up, none), TermMode::default() | TermMode::APP_CURSOR).unwrap();
        assert_eq!(app, b"\x1bOA");
        assert_eq!(enc(KeyCode::Left, KeyModifiers::CONTROL), b"\x1b[1;5D");
        assert_eq!(enc(KeyCode::Right, KeyModifiers::ALT | KeyModifiers::SHIFT), b"\x1b[1;4C");
    }

    #[test]
    fn function_and_editing_keys() {
        let none = KeyModifiers::empty();
        assert_eq!(enc(KeyCode::F(1), none), b"\x1bOP");
        assert_eq!(enc(KeyCode::F(5), none), b"\x1b[15~");
        assert_eq!(enc(KeyCode::F(12), KeyModifiers::SHIFT), b"\x1b[24;2~");
        assert_eq!(enc(KeyCode::Delete, none), b"\x1b[3~");
        assert_eq!(enc(KeyCode::PageUp, KeyModifiers::CONTROL), b"\x1b[5;5~");
    }

    #[test]
    fn release_is_ignored() {
        let mut ev = k(KeyCode::Char('a'), KeyModifiers::empty());
        ev.kind = KeyEventKind::Release;
        assert!(encode(&ev, TermMode::default()).is_none());
    }
}

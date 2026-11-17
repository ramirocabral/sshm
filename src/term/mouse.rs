use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

pub fn app_wants_mouse(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Encodes a mouse event (coordinates relative to the pane) for an app that enabled mouse reporting.
pub fn encode(ev: &MouseEvent, col: u16, row: u16, mode: TermMode) -> Option<Vec<u8>> {
    if !app_wants_mouse(mode) {
        return None;
    }
    let button_code = |b: MouseButton| match b {
        MouseButton::Left => 0u8,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match ev.kind {
        MouseEventKind::Down(b) => (button_code(b), false),
        MouseEventKind::Up(b) => (button_code(b), true),
        MouseEventKind::Drag(b) => {
            if !mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION) {
                return None;
            }
            (button_code(b) + 32, false)
        }
        MouseEventKind::Moved => {
            if !mode.contains(TermMode::MOUSE_MOTION) {
                return None;
            }
            (3 + 32, false)
        }
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    let (x, y) = (col as u32 + 1, row as u32 + 1);
    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{code};{x};{y}{suffix}").into_bytes());
    }
    if release {
        code = 3 + (code & !3);
    }
    let mut out = b"\x1b[M".to_vec();
    out.push(32 + code);
    if mode.contains(TermMode::UTF8_MOUSE) {
        for v in [x, y] {
            let ch = char::from_u32(32 + v).unwrap_or(' ');
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
    } else {
        out.push((32 + x).min(255) as u8);
        out.push((32 + y).min(255) as u8);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(kind: MouseEventKind) -> MouseEvent {
        MouseEvent { kind, column: 0, row: 0, modifiers: KeyModifiers::empty() }
    }

    #[test]
    fn sgr_and_legacy() {
        let sgr = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        assert_eq!(encode(&ev(MouseEventKind::Down(MouseButton::Left)), 4, 2, sgr).unwrap(), b"\x1b[<0;5;3M");
        assert_eq!(encode(&ev(MouseEventKind::Up(MouseButton::Left)), 4, 2, sgr).unwrap(), b"\x1b[<0;5;3m");
        assert_eq!(encode(&ev(MouseEventKind::ScrollUp), 0, 0, sgr).unwrap(), b"\x1b[<64;1;1M");
        let legacy = TermMode::MOUSE_REPORT_CLICK;
        assert_eq!(encode(&ev(MouseEventKind::Down(MouseButton::Right)), 0, 0, legacy).unwrap(), vec![0x1b, b'[', b'M', 34, 33, 33]);
        assert!(encode(&ev(MouseEventKind::Moved), 0, 0, legacy).is_none());
        assert!(encode(&ev(MouseEventKind::Down(MouseButton::Left)), 0, 0, TermMode::default()).is_none());
    }
}

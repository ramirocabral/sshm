use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use zeroize::Zeroize;

#[derive(Debug, Clone, Default)]
pub struct TextInput {
    pub value: String,
    cursor: usize,
    pub masked: bool,
}

impl TextInput {
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        TextInput { value, cursor, masked: false }
    }

    pub fn masked() -> Self {
        TextInput { value: String::new(), cursor: 0, masked: true }
    }

    pub fn set(&mut self, value: impl Into<String>) {
        self.value.zeroize();
        self.value = value.into();
        self.cursor = self.value.chars().count();
    }

    pub fn clear(&mut self) {
        self.value.zeroize();
        self.value.clear();
        self.cursor = 0;
    }

    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor = self.value.chars().count();
    }

    fn byte_idx(&self, char_idx: usize) -> usize {
        self.value.char_indices().nth(char_idx).map(|(i, _)| i).unwrap_or(self.value.len())
    }

    pub fn insert_str(&mut self, s: &str) {
        let clean: String = s.chars().filter(|c| !c.is_control()).collect();
        let idx = self.byte_idx(self.cursor);
        self.value.insert_str(idx, &clean);
        self.cursor += clean.chars().count();
    }

    /// Returns true when the key edited the input.
    pub fn handle_key(&mut self, key: &KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char(c) if !ctrl => {
                let idx = self.byte_idx(self.cursor);
                self.value.insert(idx, c);
                self.cursor += 1;
            }
            KeyCode::Char('u') if ctrl => self.clear(),
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.value.chars().count(),
            KeyCode::Char('w') if ctrl => {
                let chars: Vec<char> = self.value.chars().collect();
                let mut start = self.cursor;
                while start > 0 && chars[start - 1] == ' ' {
                    start -= 1;
                }
                while start > 0 && chars[start - 1] != ' ' && chars[start - 1] != '/' {
                    start -= 1;
                }
                let (a, b) = (self.byte_idx(start), self.byte_idx(self.cursor));
                self.value.replace_range(a..b, "");
                self.cursor = start;
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let a = self.byte_idx(self.cursor - 1);
                    let b = self.byte_idx(self.cursor);
                    self.value.replace_range(a..b, "");
                    self.cursor -= 1;
                }
            }
            KeyCode::Delete => {
                if self.cursor < self.value.chars().count() {
                    let a = self.byte_idx(self.cursor);
                    let b = self.byte_idx(self.cursor + 1);
                    self.value.replace_range(a..b, "");
                }
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.value.chars().count()),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = self.value.chars().count(),
            _ => return false,
        }
        true
    }

    /// Draws the input on one line, underlined when focused; returns the cursor position then.
    pub fn render(&self, area: Rect, buf: &mut Buffer, focused: bool, style: Style) -> Option<Position> {
        let st = if focused { style.add_modifier(Modifier::UNDERLINED) } else { style };
        buf.set_style(area, st);
        self.render_plain(area, buf, focused, st)
    }

    /// Like `render` but without decoration; the caller styles the background.
    pub fn render_plain(&self, area: Rect, buf: &mut Buffer, show_cursor: bool, style: Style) -> Option<Position> {
        if area.width == 0 {
            return None;
        }
        let width = area.width as usize;
        let start = self.cursor.saturating_sub(width.saturating_sub(1));
        let visible: String = if self.masked {
            "•".repeat(self.value.chars().count().saturating_sub(start).min(width))
        } else {
            self.value.chars().skip(start).take(width).collect()
        };
        if !visible.is_empty() {
            buf.set_stringn(area.x, area.y, &visible, width, style);
        }
        show_cursor.then(|| Position::new(area.x + (self.cursor - start) as u16, area.y))
    }
}

impl Drop for TextInput {
    fn drop(&mut self) {
        if self.masked {
            self.value.zeroize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::empty())
    }

    #[test]
    fn editing() {
        let mut t = TextInput::new("hola");
        t.handle_key(&key(KeyCode::Backspace));
        t.handle_key(&key(KeyCode::Char('ñ')));
        assert_eq!(t.value, "holñ");
        t.handle_key(&key(KeyCode::Home));
        t.handle_key(&key(KeyCode::Delete));
        assert_eq!(t.value, "olñ");
        t.insert_str("x\ny");
        assert_eq!(t.value, "xyolñ");
        t.handle_key(&KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(t.value, "");
    }

    #[test]
    fn renders_the_tail_and_masks() {
        let area = Rect::new(0, 0, 4, 1);
        let row = |buf: &Buffer| (0..4).map(|x| buf[(x, 0)].symbol().to_string()).collect::<String>();
        let mut buf = Buffer::empty(area);
        let cursor = TextInput::new("abcdef").render(area, &mut buf, true, Style::default());
        assert_eq!((row(&buf), cursor), ("def ".to_string(), Some(Position::new(3, 0))));
        assert!(buf[(0, 0)].modifier.contains(Modifier::UNDERLINED));
        let mut pw = TextInput::masked();
        pw.insert_str("secret");
        let mut buf = Buffer::empty(area);
        assert_eq!(pw.render_plain(area, &mut buf, false, Style::default()), None);
        assert_eq!(row(&buf), "••• ");
    }
}

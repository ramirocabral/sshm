use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::widgets::TextInput;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub value: String,
    pub label: String,
    pub detail: String,
}

impl Choice {
    pub fn new(value: impl Into<String>, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Choice { value: value.into(), label: label.into(), detail: detail.into() }
    }
}

pub enum FieldKind {
    Text(TextInput),
    /// `stored`: a value already exists (shown as a placeholder); `cleared`: user asked to delete it.
    Secret { input: TextInput, stored: bool, cleared: bool },
    Toggle(bool),
    Select { options: Vec<Choice>, selected: usize },
    /// Free text with a list of suggestions.
    Combo { input: TextInput, options: Vec<Choice> },
}

pub struct Field {
    pub id: &'static str,
    pub label: &'static str,
    pub hint: String,
    pub section: &'static str,
    pub placeholder: String,
    pub visible: bool,
    pub kind: FieldKind,
}

impl Field {
    fn new(id: &'static str, label: &'static str, kind: FieldKind) -> Self {
        Field { id, label, hint: String::new(), section: "", placeholder: String::new(), visible: true, kind }
    }

    pub fn text(id: &'static str, label: &'static str, value: impl Into<String>) -> Self {
        Self::new(id, label, FieldKind::Text(TextInput::new(value)))
    }

    pub fn secret(id: &'static str, label: &'static str, stored: bool) -> Self {
        Self::new(id, label, FieldKind::Secret { input: TextInput::masked(), stored, cleared: false })
    }

    pub fn toggle(id: &'static str, label: &'static str, on: bool) -> Self {
        Self::new(id, label, FieldKind::Toggle(on))
    }

    pub fn select(id: &'static str, label: &'static str, options: Vec<Choice>, value: &str) -> Self {
        let selected = options.iter().position(|c| c.value == value).unwrap_or(0);
        Self::new(id, label, FieldKind::Select { options, selected })
    }

    pub fn combo(id: &'static str, label: &'static str, value: impl Into<String>, options: Vec<Choice>) -> Self {
        Self::new(id, label, FieldKind::Combo { input: TextInput::new(value), options })
    }

    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = h.into();
        self
    }

    pub fn section(mut self, s: &'static str) -> Self {
        self.section = s;
        self
    }

    pub fn placeholder(mut self, p: impl Into<String>) -> Self {
        self.placeholder = p.into();
        self
    }

    pub fn input(&self) -> Option<&TextInput> {
        match &self.kind {
            FieldKind::Text(i) | FieldKind::Combo { input: i, .. } | FieldKind::Secret { input: i, .. } => Some(i),
            _ => None,
        }
    }

    fn input_mut(&mut self) -> Option<&mut TextInput> {
        match &mut self.kind {
            FieldKind::Text(i) | FieldKind::Combo { input: i, .. } | FieldKind::Secret { input: i, .. } => Some(i),
            _ => None,
        }
    }
}

pub struct Popup {
    pub field: usize,
    pub selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// vim-like: letters are commands (j/k move, i edits…).
    Normal,
    /// Typing goes into the focused text field.
    Insert,
}

pub struct Form {
    pub title: String,
    pub submit_label: &'static str,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub popup: Option<Popup>,
    pub error: Option<String>,
    pub mode: Mode,
    pub dirty: bool,
    /// Waiting for y/n after asking to discard unsaved changes.
    pub confirm_discard: bool,
    pending_g: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Submit,
    Cancel,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FormEvent {
    None,
    Changed(&'static str),
    Submit,
    Cancel,
}

impl Form {
    pub fn new(title: impl Into<String>, submit_label: &'static str, fields: Vec<Field>) -> Self {
        Form {
            title: title.into(),
            submit_label,
            fields,
            focus: 0,
            popup: None,
            error: None,
            mode: Mode::Normal,
            dirty: false,
            confirm_discard: false,
            pending_g: false,
        }
    }

    fn idx(&self, id: &str) -> usize {
        self.fields.iter().position(|f| f.id == id).unwrap_or_else(|| panic!("unknown form field {id}"))
    }

    pub fn field(&self, id: &str) -> &Field {
        &self.fields[self.idx(id)]
    }

    pub fn field_mut(&mut self, id: &str) -> &mut Field {
        let i = self.idx(id);
        &mut self.fields[i]
    }

    /// Trimmed text of an input, or the selected value of a select.
    pub fn value(&self, id: &str) -> String {
        match &self.field(id).kind {
            FieldKind::Text(i) | FieldKind::Combo { input: i, .. } | FieldKind::Secret { input: i, .. } => i.value.trim().to_string(),
            FieldKind::Select { options, selected } => options.get(*selected).map(|c| c.value.clone()).unwrap_or_default(),
            FieldKind::Toggle(on) => on.to_string(),
        }
    }

    /// Untrimmed secret text (passwords may legitimately have spaces).
    pub fn raw(&self, id: &str) -> String {
        self.field(id).input().map(|i| i.value.clone()).unwrap_or_default()
    }

    pub fn toggled(&self, id: &str) -> bool {
        matches!(self.field(id).kind, FieldKind::Toggle(true))
    }

    pub fn set_value(&mut self, id: &str, value: &str) {
        match &mut self.field_mut(id).kind {
            FieldKind::Text(i) | FieldKind::Combo { input: i, .. } | FieldKind::Secret { input: i, .. } => i.set(value),
            FieldKind::Select { options, selected } => {
                if let Some(p) = options.iter().position(|c| c.value == value) {
                    *selected = p;
                }
            }
            FieldKind::Toggle(on) => *on = value == "true",
        }
    }

    pub fn set_visible(&mut self, id: &str, visible: bool) {
        self.field_mut(id).visible = visible;
        if self.focus < self.fields.len() && !self.fields[self.focus].visible {
            let vis = self.visible();
            self.focus = vis
                .iter()
                .rev()
                .find(|i| **i < self.focus)
                .or_else(|| vis.first())
                .copied()
                .unwrap_or_else(|| self.button_index(Button::Submit));
        }
    }

    /// Focus positions past the fields are the buttons: fields.len() = submit, +1 = cancel.
    pub fn button_index(&self, b: Button) -> usize {
        match b {
            Button::Submit => self.fields.len(),
            Button::Cancel => self.fields.len() + 1,
        }
    }

    pub fn focused_button(&self) -> Option<Button> {
        match self.focus.checked_sub(self.fields.len()) {
            Some(0) => Some(Button::Submit),
            Some(1) => Some(Button::Cancel),
            _ => None,
        }
    }

    pub fn focused_field(&self) -> Option<&Field> {
        self.fields.get(self.focus)
    }

    pub fn visible(&self) -> Vec<usize> {
        (0..self.fields.len()).filter(|i| self.fields[*i].visible).collect()
    }

    #[cfg(test)]
    pub fn focus_id(&mut self, id: &str) {
        self.focus = self.idx(id);
    }

    fn move_focus(&mut self, delta: isize) {
        let mut vis = self.visible();
        vis.push(self.button_index(Button::Submit));
        vis.push(self.button_index(Button::Cancel));
        let pos = vis.iter().position(|i| *i == self.focus).unwrap_or(0) as isize;
        let n = vis.len() as isize;
        self.focus = vis[((pos + delta).rem_euclid(n)) as usize];
    }

    /// Options shown in the popup of a field; combos filter them by what is typed.
    pub fn choices(&self, field: usize) -> Vec<&Choice> {
        match &self.fields[field].kind {
            FieldKind::Select { options, .. } => options.iter().collect(),
            FieldKind::Combo { input, options } => {
                let q = input.value.trim().to_lowercase();
                let exact = options.iter().any(|c| c.value.to_lowercase() == q);
                options
                    .iter()
                    .filter(|c| exact || q.is_empty() || c.value.to_lowercase().contains(&q) || c.label.to_lowercase().contains(&q))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    pub fn open_popup(&mut self) -> bool {
        let field = self.focus;
        let choices = self.choices(field);
        if choices.is_empty() {
            return false;
        }
        let selected = match &self.fields[field].kind {
            FieldKind::Select { selected, .. } => *selected,
            FieldKind::Combo { input, .. } => choices.iter().position(|c| c.value == input.value.trim()).unwrap_or(0),
            _ => 0,
        };
        self.popup = Some(Popup { field, selected });
        true
    }

    /// Picks the `n`th entry currently listed in the open popup.
    pub fn choose(&mut self, n: usize) -> FormEvent {
        let Some(p) = self.popup.take() else { return FormEvent::None };
        let value = match self.choices(p.field).get(n) {
            Some(c) => c.value.clone(),
            None => return FormEvent::None,
        };
        let field = &mut self.fields[p.field];
        match &mut field.kind {
            FieldKind::Select { options, selected } => {
                *selected = options.iter().position(|c| c.value == value).unwrap_or(*selected);
            }
            FieldKind::Combo { input, .. } => input.set(value),
            _ => {}
        }
        self.dirty = true;
        FormEvent::Changed(field.id)
    }

    pub fn paste(&mut self, text: &str) {
        if let Some(i) = self.fields.get_mut(self.focus).and_then(Field::input_mut) {
            i.insert_str(text);
            self.dirty = true;
            self.mode = Mode::Insert;
        }
    }

    /// Mouse click on a field row.
    pub fn click(&mut self, idx: usize) -> FormEvent {
        self.popup = None;
        if idx == self.button_index(Button::Submit) {
            return FormEvent::Submit;
        }
        if idx == self.button_index(Button::Cancel) {
            return FormEvent::Cancel;
        }
        if idx >= self.fields.len() || !self.fields[idx].visible {
            return FormEvent::None;
        }
        self.focus = idx;
        self.mode = if self.fields[idx].input().is_some() { Mode::Insert } else { Mode::Normal };
        match &mut self.fields[idx].kind {
            FieldKind::Toggle(on) => {
                *on = !*on;
                self.dirty = true;
                FormEvent::Changed(self.fields[idx].id)
            }
            FieldKind::Select { .. } => {
                self.open_popup();
                FormEvent::None
            }
            _ => FormEvent::None,
        }
    }

    fn cycle(&mut self, delta: isize) -> FormEvent {
        let field = &mut self.fields[self.focus];
        if let FieldKind::Select { options, selected } = &mut field.kind {
            let n = options.len() as isize;
            if n > 0 {
                *selected = ((*selected as isize + delta).rem_euclid(n)) as usize;
                self.dirty = true;
                return FormEvent::Changed(field.id);
            }
        }
        FormEvent::None
    }

    fn is_text(&self, idx: usize) -> bool {
        self.fields.get(idx).is_some_and(|f| f.input().is_some())
    }

    /// Moves focus; Insert mode carries over only between text fields.
    fn step(&mut self, delta: isize) {
        self.move_focus(delta);
        if !self.is_text(self.focus) {
            self.mode = Mode::Normal;
        }
    }

    fn jump(&mut self, last: bool) {
        let vis = self.visible();
        let target = if last { self.button_index(Button::Submit) } else { vis.first().copied().unwrap_or(0) };
        self.focus = target;
        self.mode = Mode::Normal;
    }

    fn request_cancel(&mut self) -> FormEvent {
        if self.dirty {
            self.confirm_discard = true;
            FormEvent::None
        } else {
            FormEvent::Cancel
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> FormEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let g_pending = std::mem::take(&mut self.pending_g);

        if self.confirm_discard {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => return FormEvent::Cancel,
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.confirm_discard = false,
                _ => {}
            }
            return FormEvent::None;
        }

        if let Some(p) = self.popup.as_ref() {
            let field = p.field;
            let typing = matches!(self.fields[field].kind, FieldKind::Combo { .. }) && self.mode == Mode::Insert;
            let n = self.choices(field).len();
            let last = n.saturating_sub(1);
            let p = self.popup.as_mut().expect("popup");
            let down = matches!(key.code, KeyCode::Down)
                || (ctrl && matches!(key.code, KeyCode::Char('j') | KeyCode::Char('n')))
                || (!typing && !ctrl && key.code == KeyCode::Char('j'));
            let up = matches!(key.code, KeyCode::Up)
                || (ctrl && matches!(key.code, KeyCode::Char('k') | KeyCode::Char('p')))
                || (!typing && !ctrl && key.code == KeyCode::Char('k'));
            if down {
                p.selected = (p.selected + 1).min(last);
            } else if up {
                p.selected = p.selected.saturating_sub(1);
            } else {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') if !typing || key.code == KeyCode::Esc => self.popup = None,
                    KeyCode::PageUp => p.selected = p.selected.saturating_sub(8),
                    KeyCode::PageDown => p.selected = (p.selected + 8).min(last),
                    KeyCode::Char('d') if ctrl => p.selected = (p.selected + 5).min(last),
                    KeyCode::Char('u') if ctrl => p.selected = p.selected.saturating_sub(5),
                    KeyCode::Char('g') if !typing => p.selected = 0,
                    KeyCode::Char('G') if !typing => p.selected = last,
                    KeyCode::Enter | KeyCode::Tab | KeyCode::Char(' ') if key.code != KeyCode::Char(' ') || !typing => {
                        let sel = p.selected;
                        let ev = self.choose(sel);
                        self.mode = Mode::Normal;
                        if key.code == KeyCode::Tab {
                            self.step(1);
                        }
                        return ev;
                    }
                    _ if typing => {
                        if let Some(i) = self.fields[field].input_mut()
                            && i.handle_key(key) {
                                self.dirty = true;
                            }
                        let n = self.choices(field).len();
                        if n == 0 {
                            self.popup = None;
                        } else if let Some(p) = self.popup.as_mut() {
                            p.selected = p.selected.min(n - 1);
                        }
                    }
                    _ => {}
                }
            }
            return FormEvent::None;
        }

        // Keys that work the same in both modes.
        match key.code {
            KeyCode::Char('s') if ctrl => return FormEvent::Submit,
            KeyCode::Tab | KeyCode::Down => {
                self.step(1);
                return FormEvent::None;
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.step(-1);
                return FormEvent::None;
            }
            KeyCode::Char('j') | KeyCode::Char('n') if ctrl => {
                self.step(1);
                return FormEvent::None;
            }
            KeyCode::Char('k') | KeyCode::Char('p') if ctrl => {
                self.step(-1);
                return FormEvent::None;
            }
            _ => {}
        }

        if let Some(b) = self.focused_button() {
            self.mode = Mode::Normal;
            return match key.code {
                KeyCode::Enter | KeyCode::Char(' ') => match b {
                    Button::Submit => FormEvent::Submit,
                    Button::Cancel => self.request_cancel(),
                },
                KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                    let other = if b == Button::Submit { Button::Cancel } else { Button::Submit };
                    self.focus = self.button_index(other);
                    FormEvent::None
                }
                KeyCode::Char('j') => {
                    self.step(1);
                    FormEvent::None
                }
                KeyCode::Char('k') => {
                    self.step(-1);
                    FormEvent::None
                }
                KeyCode::Char('g') if g_pending => {
                    self.jump(false);
                    FormEvent::None
                }
                KeyCode::Char('g') => {
                    self.pending_g = true;
                    FormEvent::None
                }
                KeyCode::Esc | KeyCode::Char('q') => self.request_cancel(),
                _ => FormEvent::None,
            };
        }

        let focus = self.focus;
        let id = self.fields[focus].id;
        let text_field = self.is_text(focus);
        if !text_field {
            self.mode = Mode::Normal;
        }

        if self.mode == Mode::Insert {
            if key.code == KeyCode::Esc {
                self.mode = Mode::Normal;
                return FormEvent::None;
            }
            if key.code == KeyCode::Enter {
                if matches!(self.fields[focus].kind, FieldKind::Combo { .. }) && self.open_popup() {
                    return FormEvent::None;
                }
                self.step(1);
                return FormEvent::None;
            }
            let field = &mut self.fields[focus];
            match &mut field.kind {
                FieldKind::Secret { input, stored, cleared } => {
                    if ctrl && key.code == KeyCode::Char('d') {
                        input.clear();
                        *cleared = *stored || *cleared;
                        *stored = false;
                        self.dirty = true;
                        return FormEvent::Changed(id);
                    }
                    if input.handle_key(key) {
                        *cleared = false;
                        self.dirty = true;
                    }
                }
                FieldKind::Text(input) | FieldKind::Combo { input, .. } => {
                    if input.handle_key(key) {
                        self.dirty = true;
                    }
                }
                _ => {}
            }
            return FormEvent::None;
        }

        // Normal mode.
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return self.request_cancel(),
            KeyCode::Char('j') => {
                self.step(1);
                return FormEvent::None;
            }
            KeyCode::Char('k') => {
                self.step(-1);
                return FormEvent::None;
            }
            KeyCode::Char('g') if g_pending => {
                self.jump(false);
                return FormEvent::None;
            }
            KeyCode::Char('g') => {
                self.pending_g = true;
                return FormEvent::None;
            }
            KeyCode::Char('G') => {
                self.jump(true);
                return FormEvent::None;
            }
            _ => {}
        }

        let field = &mut self.fields[focus];
        match &mut field.kind {
            FieldKind::Select { .. } => match key.code {
                KeyCode::Left | KeyCode::Char('h') => self.cycle(-1),
                KeyCode::Right | KeyCode::Char('l') => self.cycle(1),
                KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('i') => {
                    self.open_popup();
                    FormEvent::None
                }
                _ => FormEvent::None,
            },
            FieldKind::Toggle(on) => match key.code {
                KeyCode::Char(' ') | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                    *on = !*on;
                    self.dirty = true;
                    FormEvent::Changed(id)
                }
                KeyCode::Enter => {
                    self.step(1);
                    FormEvent::None
                }
                _ => FormEvent::None,
            },
            FieldKind::Text(_) | FieldKind::Combo { .. } | FieldKind::Secret { .. } => self.normal_text_key(key, focus),
        }
    }

    fn normal_text_key(&mut self, key: &KeyEvent, focus: usize) -> FormEvent {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let id = self.fields[focus].id;
        let is_combo = matches!(self.fields[focus].kind, FieldKind::Combo { .. });
        let Some(input) = self.fields[focus].input_mut() else { return FormEvent::None };
        match key.code {
            KeyCode::Enter if is_combo => {
                if !self.open_popup() {
                    self.mode = Mode::Insert;
                }
            }
            KeyCode::Enter | KeyCode::Char('i') => self.mode = Mode::Insert,
            KeyCode::Char('a') | KeyCode::Char('A') => {
                input.cursor_end();
                self.mode = Mode::Insert;
            }
            KeyCode::Char('I') => {
                input.cursor_home();
                self.mode = Mode::Insert;
            }
            KeyCode::Char('h') | KeyCode::Left => {
                input.handle_key(&KeyEvent::new(KeyCode::Left, KeyModifiers::empty()));
            }
            KeyCode::Char('l') | KeyCode::Right => {
                input.handle_key(&KeyEvent::new(KeyCode::Right, KeyModifiers::empty()));
            }
            KeyCode::Char('0') | KeyCode::Home => input.cursor_home(),
            KeyCode::Char('$') | KeyCode::End => input.cursor_end(),
            // S / C: clear and type; D (or Ctrl+D): clear, and on a password also delete the stored one.
            KeyCode::Char('S') | KeyCode::Char('C') => {
                if !input.value.is_empty() {
                    input.clear();
                    self.dirty = true;
                }
                self.mode = Mode::Insert;
                return FormEvent::Changed(id);
            }
            KeyCode::Char('D') => return self.delete_value(focus),
            KeyCode::Char('d') if ctrl => return self.delete_value(focus),
            _ => {}
        }
        FormEvent::None
    }

    fn delete_value(&mut self, focus: usize) -> FormEvent {
        let field = &mut self.fields[focus];
        match &mut field.kind {
            FieldKind::Secret { input, stored, cleared } => {
                input.clear();
                *cleared = *stored || *cleared;
                *stored = false;
            }
            FieldKind::Text(input) | FieldKind::Combo { input, .. } => input.clear(),
            _ => return FormEvent::None,
        }
        self.dirty = true;
        FormEvent::Changed(field.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::empty())
    }

    fn form() -> Form {
        Form::new(
            "t",
            "Save",
            vec![
                Field::text("name", "Name", "web"),
                Field::select(
                    "src",
                    "Source",
                    vec![Choice::new("vault", "Vault", ""), Choice::new("none", "None", ""), Choice::new("op", "1Password", "")],
                    "none",
                ),
                Field::combo("folder", "Folder", "", vec![Choice::new("prod", "prod", ""), Choice::new("lab/x", "lab/x", "")]),
                Field::toggle("auto", "Auto", false),
                Field::secret("pw", "Password", true),
            ],
        )
    }

    #[test]
    fn select_cycles_and_popup() {
        let mut f = form();
        f.handle_key(&key(KeyCode::Down));
        assert_eq!(f.value("src"), "none");
        assert_eq!(f.handle_key(&key(KeyCode::Right)), FormEvent::Changed("src"));
        assert_eq!(f.value("src"), "op");
        f.handle_key(&key(KeyCode::Enter));
        assert!(f.popup.is_some());
        f.handle_key(&key(KeyCode::Up));
        f.handle_key(&key(KeyCode::Up));
        assert_eq!(f.handle_key(&key(KeyCode::Enter)), FormEvent::Changed("src"));
        assert_eq!(f.value("src"), "vault");
    }

    #[test]
    fn combo_filters_and_picks() {
        let mut f = form();
        f.focus_id("folder");
        f.handle_key(&key(KeyCode::Char('i')));
        assert_eq!(f.mode, Mode::Insert);
        f.handle_key(&key(KeyCode::Char('l')));
        assert_eq!(f.choices(f.focus).len(), 1);
        f.handle_key(&key(KeyCode::Enter));
        assert_eq!(f.handle_key(&key(KeyCode::Enter)), FormEvent::Changed("folder"));
        assert_eq!(f.value("folder"), "lab/x");
    }

    #[test]
    fn toggle_secret_visibility_and_submit() {
        let mut f = form();
        f.focus_id("auto");
        f.handle_key(&key(KeyCode::Char(' ')));
        assert!(f.toggled("auto"));
        f.focus_id("pw");
        f.handle_key(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL));
        assert!(matches!(f.field("pw").kind, FieldKind::Secret { cleared: true, stored: false, .. }));
        f.set_visible("pw", false);
        assert_eq!(f.focus, f.visible().into_iter().last().unwrap());
        assert_eq!(f.handle_key(&key(KeyCode::Enter)), FormEvent::None);
        assert_eq!(f.focused_button(), Some(Button::Submit));
        f.handle_key(&key(KeyCode::Right));
        assert_eq!(f.focused_button(), Some(Button::Cancel));
        f.handle_key(&key(KeyCode::Left));
        assert_eq!(f.handle_key(&key(KeyCode::Enter)), FormEvent::Submit);
        assert_eq!(f.handle_key(&KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)), FormEvent::Submit);
        // Unsaved changes: cancelling asks first.
        assert_eq!(f.handle_key(&key(KeyCode::Esc)), FormEvent::None);
        assert!(f.confirm_discard);
        assert_eq!(f.handle_key(&key(KeyCode::Char('n'))), FormEvent::None);
        f.handle_key(&key(KeyCode::Char('q')));
        assert_eq!(f.handle_key(&key(KeyCode::Char('y'))), FormEvent::Cancel);
    }

    #[test]
    fn vim_normal_and_insert() {
        let mut f = form();
        assert_eq!(f.mode, Mode::Normal);
        f.handle_key(&key(KeyCode::Char('j')));
        assert_eq!(f.fields[f.focus].id, "src");
        f.handle_key(&key(KeyCode::Char('l')));
        assert_eq!(f.value("src"), "op");
        f.handle_key(&key(KeyCode::Char('k')));
        f.handle_key(&key(KeyCode::Char('A')));
        assert_eq!(f.mode, Mode::Insert);
        f.handle_key(&key(KeyCode::Char('j')));
        assert_eq!(f.value("name"), "webj");
        f.handle_key(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        assert_eq!(f.fields[f.focus].id, "src");
        assert_eq!(f.mode, Mode::Normal);
        f.handle_key(&key(KeyCode::Char('G')));
        assert_eq!(f.focused_button(), Some(Button::Submit));
        f.handle_key(&key(KeyCode::Char('g')));
        f.handle_key(&key(KeyCode::Char('g')));
        assert_eq!(f.fields[f.focus].id, "name");
        f.handle_key(&key(KeyCode::Char('S')));
        assert_eq!(f.mode, Mode::Insert);
        assert_eq!(f.value("name"), "");
        f.handle_key(&key(KeyCode::Esc));
        assert_eq!(f.mode, Mode::Normal);
    }
}

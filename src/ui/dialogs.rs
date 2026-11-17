use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use super::{ACCENT, MUTED};
use crate::app::App;
use crate::app::dialogs::*;
use crate::ssh::command::PromptKind;

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2)).max(10.min(area.width));
    let h = h.min(area.height.saturating_sub(2)).max(3.min(area.height));
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn frame(f: &mut Frame, rect: Rect, title: &str, color: Color) -> Rect {
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(format!(" {title} "), Style::default().fg(color).add_modifier(Modifier::BOLD)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

fn text(f: &mut Frame, x: u16, y: u16, s: &str, w: u16, style: Style) {
    f.buffer_mut().set_stringn(x, y, s, w as usize, style);
}

fn field_row(f: &mut Frame, inner: Rect, y: u16, label: &str, label_w: u16, focused: bool) -> Rect {
    let style = if focused { Style::default().fg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::default().fg(Color::Gray) };
    text(f, inner.x, y, label, label_w, style);
    Rect { x: inner.x + label_w + 1, y, width: inner.width.saturating_sub(label_w + 1), height: 1 }
}

fn hint(f: &mut Frame, inner: Rect, s: &str) {
    let y = inner.bottom().saturating_sub(1);
    text(f, inner.x, y, s, inner.width, Style::default().fg(MUTED));
}

pub fn draw(f: &mut Frame, app: &mut App, d: &Dialog, area: Rect) -> Option<Position> {
    match d {
        Dialog::Message { title, lines } => {
            let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(20).max(title.chars().count() + 4) as u16 + 4;
            let rect = centered(area, w.min(100), lines.len() as u16 + 4);
            let inner = frame(f, rect, title, ACCENT);
            let body: Vec<Line> = lines.iter().map(|l| Line::from(l.as_str())).collect();
            f.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
            hint(f, inner, "Enter/Esc closes");
            None
        }
        Dialog::Confirm(c) => {
            let rect = centered(area, 70, 8 + (c.body.len() as u16 / 60));
            let color = if matches!(c.action, Some(ConfirmAction::HostKey(_))) { Color::Yellow } else { Color::LightRed };
            let inner = frame(f, rect, &c.title, color);
            f.render_widget(Paragraph::new(c.body.clone()).wrap(Wrap { trim: false }), inner);
            hint(f, inner, "y/Enter yes · n/Esc no");
            None
        }
        Dialog::Unlock(u) => {
            let two = u.creating || u.changing;
            let title = if u.changing {
                "New master password"
            } else if u.creating {
                "Create vault"
            } else {
                "Unlock vault"
            };
            let rect = centered(area, 64, if two { 10 } else { 8 });
            let inner = frame(f, rect, title, Color::Yellow);
            let intro = if u.creating && !u.changing {
                "Choose the master password that encrypts your passwords."
            } else if u.changing {
                "Type the new master password twice."
            } else {
                "Master password:"
            };
            text(f, inner.x, inner.y, intro, inner.width, Style::default());
            let r1 = field_row(f, inner, inner.y + 2, "Password", 12, u.field == 0);
            let mut cursor = u.input.render(r1, f.buffer_mut(), u.field == 0, Style::default());
            if two {
                let r2 = field_row(f, inner, inner.y + 3, "Repeat", 12, u.field == 1);
                cursor = u.confirm.render(r2, f.buffer_mut(), u.field == 1, Style::default()).or(cursor);
            }
            if let Some(e) = &u.error {
                text(f, inner.x, inner.y + if two { 5 } else { 4 }, e, inner.width, Style::default().fg(Color::LightRed));
            }
            hint(f, inner, "Enter confirms · Esc cancels");
            cursor
        }
        Dialog::AskPass(a) => {
            let lines = a.prompt.lines().count() as u16;
            let rect = centered(area, 76, 8 + lines);
            let inner = frame(f, rect, &format!("{} · ssh asks", a.label), Color::Yellow);
            f.render_widget(Paragraph::new(a.prompt.clone()).wrap(Wrap { trim: false }), Rect { height: lines.max(1), ..inner });
            let y = inner.y + lines + 1;
            let mut input = a.input.clone();
            input.masked = a.kind != PromptKind::Other;
            let r = field_row(f, inner, y, "Answer", 10, true);
            let cursor = input.render(r, f.buffer_mut(), true, Style::default());
            if let Some(n) = &a.note {
                text(f, inner.x, y + 1, n, inner.width, Style::default().fg(Color::LightRed));
            }
            let mut h = "Enter sends · Esc cancels".to_string();
            if a.secret_kind.is_some() {
                h.push_str(&format!(" · Tab: save to vault [{}]", if a.save { "yes" } else { "no" }));
            }
            hint(f, inner, &h);
            cursor
        }
        Dialog::Input(d) => {
            let rect = centered(area, 70, 5);
            let inner = frame(f, rect, &d.title, ACCENT);
            let cursor = d.input.render(Rect { height: 1, ..inner }, f.buffer_mut(), true, Style::default());
            hint(f, inner, "Enter confirms · Esc cancels");
            cursor
        }
        Dialog::Palette(p) => {
            let matches = p.matches();
            let rect = centered(area, 80, matches.len() as u16 + 5);
            let inner = frame(f, rect, "Commands", ACCENT);
            text(f, inner.x, inner.y, ">", 1, Style::default().fg(ACCENT));
            let cursor = p.input.render(Rect { x: inner.x + 2, y: inner.y, width: inner.width.saturating_sub(2), height: 1 }, f.buffer_mut(), true, Style::default());
            for (i, c) in matches.iter().enumerate() {
                let y = inner.y + 2 + i as u16;
                if y >= inner.bottom() {
                    break;
                }
                let style = if i == p.selected { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
                text(f, inner.x, y, &format!(" {} ", c.label()), inner.width, style);
            }
            cursor
        }
        Dialog::HostForm(d) => draw_form(f, app, &d.form, area, 96),
        Dialog::Import(d) => draw_form(f, app, &d.form, area, 90),
        Dialog::Export(d) => draw_form(f, app, &d.form, area, 90),
        Dialog::Help(h) => {
            let rect = centered(area, 104, area.height.saturating_sub(4));
            let inner = frame(f, rect, "Help", ACCENT);
            let entries = filter_help(help_entries(app), &h.query);
            let mut cursor = None;
            let mut list = inner;
            if h.search.is_some() || !h.query.is_empty() {
                text(f, inner.x, inner.y, "/", 1, Style::default().fg(ACCENT).add_modifier(Modifier::BOLD));
                let r = Rect { x: inner.x + 2, y: inner.y, width: inner.width.saturating_sub(2), height: 1 };
                match &h.search {
                    Some(input) => cursor = input.render(r, f.buffer_mut(), true, Style::default()),
                    None => text(f, r.x, r.y, &h.query, r.width, Style::default().fg(Color::Yellow)),
                }
                let n = entries.iter().filter(|e| matches!(e, HelpLine::Row(..) | HelpLine::Text(_))).count();
                let count = format!("{n} matches");
                let cw = count.chars().count() as u16;
                text(f, inner.right().saturating_sub(cw), inner.y, &count, cw, Style::default().fg(MUTED));
                list = Rect { y: inner.y + 2, height: inner.height.saturating_sub(2), ..inner };
            }
            let lines: Vec<Line> = if entries.is_empty() {
                vec![Line::from(Span::styled("No shortcut matches the search", Style::default().fg(MUTED)))]
            } else {
                entries.iter().map(|e| e.render(&h.query)).collect()
            };
            let max_scroll = (lines.len() as u16).saturating_sub(list.height.saturating_sub(1));
            app.layout.help_max_scroll = max_scroll;
            f.render_widget(Paragraph::new(lines).scroll((h.scroll.min(max_scroll), 0)), Rect { height: list.height.saturating_sub(1), ..list });
            let footer = if h.search.is_some() {
                "type to filter · Enter keeps the filter · Esc clears it"
            } else if !h.query.is_empty() {
                "/ edit search · Esc clears the filter · j/k scroll"
            } else {
                "/ search · j/k scroll · Esc closes"
            };
            hint(f, inner, footer);
            cursor
        }
    }
}

fn draw_form(f: &mut Frame, app: &mut App, form: &crate::app::form::Form, area: Rect, width: u16) -> Option<Position> {
    let layout = super::form::draw(f, form, area, width);
    app.layout.form_rows = layout.rows;
    app.layout.popup_rows = layout.popup;
    layout.cursor
}

pub enum HelpLine {
    Head(String),
    Row(String, String),
    Text(String),
    Blank,
}

impl HelpLine {
    fn matches(&self, words: &[String]) -> bool {
        let hay = match self {
            HelpLine::Row(k, d) => format!("{k} {d}").to_lowercase(),
            HelpLine::Text(t) => t.to_lowercase(),
            _ => return false,
        };
        words.iter().all(|w| hay.contains(w.as_str()))
    }

    fn render(&self, query: &str) -> Line<'static> {
        match self {
            HelpLine::Head(t) => Line::from(Span::styled(t.clone(), Style::default().fg(ACCENT).add_modifier(Modifier::BOLD))),
            HelpLine::Row(k, d) => {
                let mut spans = vec![Span::styled(format!("  {k:<34} "), Style::default().fg(Color::Yellow))];
                spans.extend(highlight(d, query));
                Line::from(spans)
            }
            HelpLine::Text(t) => Line::from(highlight(t, query)),
            HelpLine::Blank => Line::from(""),
        }
    }
}

/// Splits `text` into spans, highlighting the first word of `query` wherever it occurs.
fn highlight(text: &str, query: &str) -> Vec<Span<'static>> {
    let Some(word) = query.split_whitespace().next().map(str::to_lowercase).filter(|w| !w.is_empty()) else {
        return vec![Span::raw(text.to_string())];
    };
    let lower = text.to_lowercase();
    if lower.len() != text.len() {
        return vec![Span::raw(text.to_string())];
    }
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(i) = lower[pos..].find(&word) {
        let start = pos + i;
        let end = start + word.len();
        if start > pos {
            spans.push(Span::raw(text[pos..start].to_string()));
        }
        spans.push(Span::styled(text[start..end].to_string(), Style::default().fg(Color::Black).bg(Color::Yellow)));
        pos = end;
    }
    spans.push(Span::raw(text[pos..].to_string()));
    spans
}

/// Keeps the entries matching every word of the query, with their section headers.
pub fn filter_help(entries: Vec<HelpLine>, query: &str) -> Vec<HelpLine> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return entries;
    }
    let mut out = Vec::new();
    let mut pending_head = None;
    for e in entries {
        match e {
            HelpLine::Head(_) => pending_head = Some(e),
            HelpLine::Blank => {}
            ref line if line.matches(&words) => {
                if let Some(h) = pending_head.take() {
                    if !out.is_empty() {
                        out.push(HelpLine::Blank);
                    }
                    out.push(h);
                }
                out.push(e);
            }
            _ => {}
        }
    }
    out
}

pub fn help_entries(app: &App) -> Vec<HelpLine> {
    let l = app.leader.label();
    let head = |s: &str| HelpLine::Head(s.to_string());
    let row = |k: String, d: &str| HelpLine::Row(k, d.to_string());
    vec![
        head("Inside a session (leader key)"),
        row(format!("{l} Tab"), "go to the host list / back"),
        row(format!("{l} \\"), "show/hide the host list"),
        row(format!("{l} 1..9  ·  Alt+1..9"), "go to tab N"),
        row(format!("{l} L / H  ·  N / B"), "next / previous tab"),
        row(format!("{l} hjkl  ·  Alt+arrows"), "move between panes"),
        row(format!("{l} o"), "next pane"),
        row(format!("{l} z"), "zoom the pane (full screen)"),
        row(format!("{l} d"), "duplicate the session into this tab (grid)"),
        row(format!("{l} t"), "local shell in a new tab"),
        row(format!("{l} s"), "SFTP to the current host"),
        row(format!("{l} b"), "broadcast: type into every pane of the tab"),
        row(format!("{l} x"), "exclude/include the pane in broadcast"),
        row(format!("{l} p"), "type the host password (sudo, etc.)"),
        row(format!("{l} P"), "type the root password (su -)"),
        row(format!("{l} ["), "copy mode (vi): v/V select, y copies, q exits"),
        row(format!("{l} r"), "reconnect"),
        row(format!("{l} w"), "close pane / tab"),
        row(format!("{l} ,"), "rename tab"),
        row(format!("{l} n / e"), "new host / edit the current host"),
        row(format!("{l} i"), "import"),
        row(format!("{l} :"), "command palette"),
        row(format!("{l} {l}"), "send the leader key to the session"),
        row(format!("{l} q"), "quit"),
        HelpLine::Blank,
        head("Host list"),
        row("j/k ↑↓ · g/G · Ctrl+d/u".into(), "move"),
        row("h/l ←→".into(), "collapse/expand folder"),
        row("Enter".into(), "connect (or open every marked host in a grid)"),
        row("Space".into(), "mark host (on a folder: marks all)"),
        row("o".into(), "open in the current tab"),
        row("/".into(), "search (fuzzy: name, host, user or tag); Ctrl+j/k moves while typing"),
        row("n / e / c / d".into(), "new / edit / duplicate / delete host"),
        row("s".into(), "SFTP"),
        row("y".into(), "copy user@host"),
        row("i / x".into(), "import / export"),
        row("t".into(), "local shell"),
        row("Esc / Tab".into(), "back to the session"),
        HelpLine::Blank,
        head("Mouse"),
        row("drag".into(), "select and copy (Shift+drag when the app uses the mouse)"),
        row("double/triple click".into(), "select word / line"),
        row("wheel".into(), "scrollback"),
        row("click on a tab".into(), "switch · middle click: close"),
        HelpLine::Blank,
        head("Forms (new/edit host, import, export) · vim-like modes"),
        row("j / k  ·  ↑↓ · Tab".into(), "NORMAL: move between fields"),
        row("gg / G".into(), "NORMAL: first field / the Save button"),
        row("h / l  ·  ←→".into(), "NORMAL: change a choice, flip a switch"),
        row("i  a  I  A  ·  Enter".into(), "NORMAL: edit the text field (INSERT mode)"),
        row("S".into(), "NORMAL: clear the field and type"),
        row("D".into(), "NORMAL: clear the field (on a password: delete the saved one)"),
        row("Enter / Space".into(), "NORMAL: open the list of choices or suggestions"),
        row("Esc".into(), "INSERT → NORMAL; in NORMAL: cancel (asks if there are changes)"),
        row("q".into(), "NORMAL: cancel"),
        row("Ctrl+j / Ctrl+k".into(), "move between fields in any mode (also Ctrl+n / Ctrl+p)"),
        row("j / k  ·  Ctrl+j / Ctrl+k".into(), "move in a list of choices (Ctrl+ while typing a filter)"),
        row("Ctrl+S  ·  Save button".into(), "save / run"),
        HelpLine::Blank,
        head("Help"),
        row("/".into(), "search the shortcuts"),
        row("j / k · g / G · Ctrl+d / Ctrl+u".into(), "scroll"),
        HelpLine::Blank,
        head("Passwords"),
        HelpLine::Text("  Passwords are stored encrypted in the vault (age + scrypt) under a master password.".into()),
        HelpLine::Text(format!("  On connect sshm answers ssh's password prompt; when sudo asks, {l} p types the password.")),
        HelpLine::Text("  Auto sudo (per host) answers sudo prompts by itself, once per prompt.".into()),
        HelpLine::Text("  Edit a host (e) to choose where its password comes from: vault, Bitwarden, 1Password or a command.".into()),
    ]
}

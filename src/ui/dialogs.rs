use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};

use super::theme::colors;
use crate::app::App;
use crate::app::dialogs::*;
use crate::app::keymap::{self, Ctx};
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
    let style = if focused { Style::default().fg(colors().accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(colors().subtle) };
    text(f, inner.x, y, label, label_w, style);
    Rect { x: inner.x + label_w + 1, y, width: inner.width.saturating_sub(label_w + 1), height: 1 }
}

fn hint(f: &mut Frame, inner: Rect, s: &str) {
    let y = inner.bottom().saturating_sub(1);
    text(f, inner.x, y, s, inner.width, Style::default().fg(colors().muted));
}

pub fn draw(f: &mut Frame, app: &mut App, d: &Dialog, area: Rect) -> Option<Position> {
    match d {
        Dialog::Message { title, lines } => {
            let w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(20).max(title.chars().count() + 4) as u16 + 4;
            let rect = centered(area, w.min(100), lines.len() as u16 + 4);
            let inner = frame(f, rect, title, colors().accent);
            let body: Vec<Line> = lines.iter().map(|l| Line::from(l.as_str())).collect();
            f.render_widget(Paragraph::new(body).wrap(Wrap { trim: false }), inner);
            hint(f, inner, "Enter/Esc closes");
            None
        }
        Dialog::Confirm(c) => {
            let rect = centered(area, 70, 8 + (c.body.len() as u16 / 60));
            let color = if matches!(c.action, Some(ConfirmAction::HostKey(_))) { colors().warning } else { colors().error };
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
            let inner = frame(f, rect, title, colors().warning);
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
            let status_y = inner.y + if two { 5 } else { 4 };
            if u.busy.is_some() {
                text(f, inner.x, status_y, if u.creating { "Creating the vault…" } else { "Unlocking…" }, inner.width, Style::default().fg(colors().muted));
            } else if let Some(e) = &u.error {
                text(f, inner.x, status_y, e, inner.width, Style::default().fg(colors().error));
            }
            hint(f, inner, "Enter confirms · Esc cancels");
            cursor
        }
        Dialog::AskPass(a) => {
            let lines = a.prompt.lines().count() as u16;
            let rect = centered(area, 76, 8 + lines);
            let inner = frame(f, rect, &format!("{} · ssh asks", a.label), colors().warning);
            f.render_widget(Paragraph::new(a.prompt.clone()).wrap(Wrap { trim: false }), Rect { height: lines.max(1), ..inner });
            let y = inner.y + lines + 1;
            let mut input = a.input.clone();
            input.masked = a.kind != PromptKind::Other;
            let r = field_row(f, inner, y, "Answer", 10, true);
            let cursor = input.render(r, f.buffer_mut(), true, Style::default());
            if let Some(n) = &a.note {
                text(f, inner.x, y + 1, n, inner.width, Style::default().fg(colors().error));
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
            let inner = frame(f, rect, &d.title, colors().accent);
            let cursor = d.input.render(Rect { height: 1, ..inner }, f.buffer_mut(), true, Style::default());
            hint(f, inner, "Enter confirms · Esc cancels");
            cursor
        }
        Dialog::Palette(p) => {
            let matches = p.matches();
            let rect = centered(area, 96, matches.len() as u16 + 5);
            let inner = frame(f, rect, "Commands", colors().accent);
            text(f, inner.x, inner.y, ">", 1, Style::default().fg(colors().accent));
            let cursor = p.input.render(Rect { x: inner.x + 2, y: inner.y, width: inner.width.saturating_sub(2), height: 1 }, f.buffer_mut(), true, Style::default());
            for (i, action) in matches.iter().enumerate() {
                let y = inner.y + 2 + i as u16;
                if y >= inner.bottom() {
                    break;
                }
                let row = Rect { x: inner.x, y, width: inner.width, height: 1 };
                let style = if i == p.selected { Style::default().add_modifier(Modifier::REVERSED) } else { Style::default() };
                f.buffer_mut().set_style(row, style);
                text(f, row.x, y, &format!(" {} ", action.title()), row.width, style);
                let keys = keymap::keys_for(*action, &app.leader);
                let kw = keys.chars().count() as u16 + 1;
                if !keys.is_empty() && action.title().chars().count() as u16 + kw + 3 < row.width {
                    text(f, row.right() - kw, y, &keys, kw, style.fg(colors().muted));
                }
            }
            cursor
        }
        Dialog::HostForm(d) => draw_form(f, app, &d.form, area, 96),
        Dialog::Import(d) => draw_form(f, app, &d.form, area, 90),
        Dialog::Export(d) => draw_form(f, app, &d.form, area, 90),
        Dialog::Move(d) => draw_form(f, app, &d.form, area, 80),
        Dialog::Theme(d) => draw_form(f, app, &d.form, area, 80),
        Dialog::Help(h) => {
            let rect = centered(area, 104, area.height.saturating_sub(4));
            let inner = frame(f, rect, "Help", colors().accent);
            let entries = filter_help(help_entries(app), &h.query);
            let mut cursor = None;
            let mut list = inner;
            if h.search.is_some() || !h.query.is_empty() {
                text(f, inner.x, inner.y, "/", 1, Style::default().fg(colors().accent).add_modifier(Modifier::BOLD));
                let r = Rect { x: inner.x + 2, y: inner.y, width: inner.width.saturating_sub(2), height: 1 };
                match &h.search {
                    Some(input) => cursor = input.render(r, f.buffer_mut(), true, Style::default()),
                    None => text(f, r.x, r.y, &h.query, r.width, Style::default().fg(colors().warning)),
                }
                let n = entries.iter().filter(|e| matches!(e, HelpLine::Row(..) | HelpLine::Text(_))).count();
                let count = format!("{n} matches");
                let cw = count.chars().count() as u16;
                text(f, inner.right().saturating_sub(cw), inner.y, &count, cw, Style::default().fg(colors().muted));
                list = Rect { y: inner.y + 2, height: inner.height.saturating_sub(2), ..inner };
            }
            let lines: Vec<Line> = if entries.is_empty() {
                vec![Line::from(Span::styled("No shortcut matches the search", Style::default().fg(colors().muted)))]
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
            HelpLine::Head(t) => Line::from(Span::styled(t.clone(), Style::default().fg(colors().accent).add_modifier(Modifier::BOLD))),
            HelpLine::Row(k, d) => {
                let mut spans = vec![Span::styled(format!("  {k:<34} "), Style::default().fg(colors().warning))];
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
        spans.push(Span::styled(text[start..end].to_string(), Style::default().fg(colors().on_color).bg(colors().warning)));
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
    let row = |k: &str, d: &str| HelpLine::Row(k.to_string(), d.to_string());
    let bound = |ctx: Ctx, prefix: &str| -> Vec<HelpLine> {
        keymap::help_rows(ctx, &app.leader).into_iter().map(|r| HelpLine::Row(format!("{prefix}{}", r.keys), r.desc.to_string())).collect()
    };
    let mut out = vec![head("Inside a session (leader key)")];
    out.extend(bound(Ctx::Leader, &format!("{l} ")));
    if app.cfg.alt_shortcuts {
        out.extend([HelpLine::Blank, head("Without the leader (alt_shortcuts)")]);
        out.extend(bound(Ctx::Alt, ""));
    }
    out.extend([HelpLine::Blank, head("Host list")]);
    out.extend(bound(Ctx::Motion, ""));
    out.extend(bound(Ctx::Hosts, ""));
    out.extend([HelpLine::Blank, head("Visual selection (v / V in the host list)")]);
    out.extend(bound(Ctx::Visual, ""));
    out.extend([
        HelpLine::Blank,
        head("Mouse"),
        row("drag", "select and copy (Shift+drag when the app uses the mouse)"),
        row("double/triple click", "select word / line"),
        row("wheel", "scrollback"),
        row("click on a tab", "switch · middle click: close"),
        HelpLine::Blank,
        head("Forms (new/edit host, import, export) · vim-like modes"),
        row("j / k  ·  ↑↓ · Tab", "NORMAL: move between fields"),
        row("gg / G", "NORMAL: first field / the Save button"),
        row("h / l  ·  ←→", "NORMAL: change a choice, flip a switch"),
        row("i  a  I  A  ·  Enter", "NORMAL: edit the text field (INSERT mode)"),
        row("S", "NORMAL: clear the field and type"),
        row("D", "NORMAL: clear the field (on a password: delete the saved one)"),
        row("Enter / Space", "NORMAL: open the list of choices or suggestions"),
        row("Esc", "INSERT → NORMAL; in NORMAL: cancel (asks if there are changes)"),
        row("q", "NORMAL: cancel"),
        row("Ctrl+j / Ctrl+k", "move between fields in any mode (also Ctrl+n / Ctrl+p)"),
        row("j / k  ·  Ctrl+j / Ctrl+k", "move in a list of choices (Ctrl+ while typing a filter)"),
        row("w  ·  Save button  ·  Ctrl+S", "NORMAL: save / run (Ctrl+S only if the terminal lets it through)"),
        HelpLine::Blank,
        head("Help"),
        row("/", "search the shortcuts"),
        row("j / k · g / G · Ctrl+d / Ctrl+u", "scroll"),
        HelpLine::Blank,
        head("Passwords"),
        HelpLine::Text("  Each host picks where its passwords come from: e edits the host to change it.".into()),
        HelpLine::Text("  System keyring (secret-tool): the default for new hosts when secret-tool is installed.".into()),
        HelpLine::Text("  sshm's vault: encrypted with age + scrypt under one master password.".into()),
        HelpLine::Text("  Bitwarden (bw or rbw), 1Password (op), or any command that prints the password.".into()),
        HelpLine::Text(format!("  On connect sshm answers ssh's password prompt; when sudo asks, {l} p types the password.")),
        HelpLine::Text("  Auto sudo (per host) answers every sudo prompt in that session, nested ssh hops included.".into()),
    ]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::app_with_hosts;

    #[test]
    fn help_lists_every_bound_key_and_filters() {
        let (app, _dir) = app_with_hosts(&[]);
        let entries = help_entries(&app);
        let rows: Vec<(String, String)> =
            entries.iter().filter_map(|e| if let HelpLine::Row(k, d) = e { Some((k.clone(), d.clone())) } else { None }).collect();
        assert!(rows.iter().any(|(k, d)| k == "Ctrl+Space ?" && d == "this help"));
        assert!(rows.iter().any(|(k, _)| k == "Alt+1..9"));
        assert!(rows.iter().any(|(k, _)| k == "n / e / c / d"));
        assert!(entries.iter().any(|e| matches!(e, HelpLine::Text(t) if t.contains("keyring") )));
        let found = filter_help(help_entries(&app), "zoom");
        assert!(matches!(found.as_slice(), [HelpLine::Head(_), HelpLine::Row(k, _)] if k == "Ctrl+Space z"));
    }
}

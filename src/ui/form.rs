use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

use super::theme::colors;
use crate::app::form::{Button, FieldKind, Form, Mode};


pub struct FormLayout {
    pub cursor: Option<Position>,
    pub rows: Vec<(usize, Rect)>,
    pub popup: Vec<(usize, Rect)>,
}

enum Row {
    Section(&'static str),
    Gap,
    Field(usize),
}

fn put(f: &mut Frame, x: u16, y: u16, s: &str, w: u16, style: Style) {
    if w > 0 {
        f.buffer_mut().set_stringn(x, y, s, w as usize, style);
    }
}

pub fn draw(f: &mut Frame, form: &Form, area: Rect, width: u16) -> FormLayout {
    let mut rows = Vec::new();
    let mut section = "";
    for i in form.visible() {
        let field = &form.fields[i];
        if !field.section.is_empty() && field.section != section {
            if !rows.is_empty() {
                rows.push(Row::Gap);
            }
            rows.push(Row::Section(field.section));
            section = field.section;
        }
        rows.push(Row::Field(i));
    }

    const FOOTER: u16 = 4;
    let want_h = rows.len() as u16 + FOOTER + 3;
    let w = width.min(area.width.saturating_sub(2)).max(20.min(area.width));
    let h = want_h.min(area.height.saturating_sub(2)).max(6.min(area.height));
    let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(colors().accent))
        .title(Span::styled(format!(" {} ", form.title), Style::default().fg(colors().accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(rect).inner(ratatui::layout::Margin { horizontal: 1, vertical: 0 });
    f.render_widget(block, rect);

    let body_h = inner.height.saturating_sub(FOOTER + 1) as usize;
    let focus_row = rows.iter().position(|r| matches!(r, Row::Field(i) if *i == form.focus)).unwrap_or(0);
    let scroll = if body_h > 0 && focus_row >= body_h { focus_row + 1 - body_h } else { 0 };

    let label_w = form.fields.iter().map(|f| f.label.chars().count()).max().unwrap_or(10) as u16 + 1;
    let input_x = inner.x + 2 + label_w + 1;
    let input_w = inner.right().saturating_sub(input_x);

    let mut out = FormLayout { cursor: None, rows: Vec::new(), popup: Vec::new() };
    let mut focus_box = None;

    for (r, row) in rows.iter().enumerate().skip(scroll).take(body_h) {
        let y = inner.y + (r - scroll) as u16;
        match row {
            Row::Gap => {}
            Row::Section(name) => {
                let title = format!("{name} ");
                let tw = title.chars().count() as u16;
                put(f, inner.x, y, &title, inner.width, Style::default().fg(colors().accent).add_modifier(Modifier::BOLD));
                let line = "─".repeat(inner.width.saturating_sub(tw) as usize);
                put(f, inner.x + tw, y, &line, inner.width.saturating_sub(tw), Style::default().fg(colors().muted));
            }
            Row::Field(i) => {
                let field = &form.fields[*i];
                let focused = *i == form.focus;
                out.rows.push((*i, Rect { x: inner.x, y, width: inner.width, height: 1 }));
                if focused {
                    put(f, inner.x, y, "›", 1, Style::default().fg(colors().accent).add_modifier(Modifier::BOLD));
                }
                let label_style =
                    if focused { Style::default().fg(colors().accent).add_modifier(Modifier::BOLD) } else { Style::default().fg(colors().subtle) };
                put(f, inner.x + 2, y, field.label, label_w, label_style);

                let bx = Rect { x: input_x, y, width: input_w, height: 1 };
                if focused {
                    focus_box = Some(bx);
                }
                let bg = if focused { colors().panel_focus } else { colors().panel };
                let boxed = Style::default().bg(bg);
                let text_area = Rect { x: bx.x + 1, width: bx.width.saturating_sub(3), ..bx };
                match &field.kind {
                    FieldKind::Text(input) | FieldKind::Combo { input, .. } | FieldKind::Secret { input, .. } => {
                        f.buffer_mut().set_style(bx, boxed);
                        let placeholder = match &field.kind {
                            FieldKind::Secret { stored: true, .. } => "stored · type to replace · Ctrl+D deletes".to_string(),
                            FieldKind::Secret { cleared: true, .. } => "will be deleted when you save".to_string(),
                            FieldKind::Secret { .. } if !field.placeholder.is_empty() => field.placeholder.clone(),
                            FieldKind::Secret { .. } => "not set".to_string(),
                            _ => field.placeholder.clone(),
                        };
                        if input.value.is_empty() {
                            put(f, text_area.x, y, &placeholder, text_area.width, boxed.fg(colors().muted).add_modifier(Modifier::ITALIC));
                        }
                        let show_cursor = focused
                            && form.mode == Mode::Insert
                            && (form.popup.is_none() || matches!(field.kind, FieldKind::Combo { .. }));
                        if let Some(p) = input.render_plain(text_area, f.buffer_mut(), show_cursor, boxed) {
                            out.cursor = Some(p);
                        }
                        if let FieldKind::Combo { options, .. } = &field.kind
                            && !options.is_empty() {
                                put(f, bx.right().saturating_sub(2), y, "▾", 1, boxed.fg(if focused { colors().accent } else { colors().muted }));
                            }
                    }
                    FieldKind::Select { options, selected } => {
                        f.buffer_mut().set_style(bx, boxed);
                        if let Some(c) = options.get(*selected) {
                            let lw = c.label.chars().count() as u16;
                            put(f, text_area.x, y, &c.label, text_area.width, boxed.fg(colors().text).add_modifier(Modifier::BOLD));
                            if !c.detail.is_empty() && lw + 3 < text_area.width {
                                let d = format!(" · {}", c.detail);
                                put(f, text_area.x + lw, y, &d, text_area.width.saturating_sub(lw + 2), boxed.fg(colors().muted));
                            }
                        }
                        let arrows = if focused { "◂▸" } else { " ▾" };
                        put(f, bx.right().saturating_sub(3), y, arrows, 2, boxed.fg(if focused { colors().accent } else { colors().muted }));
                    }
                    FieldKind::Toggle(on) => {
                        let (txt, style) = if *on {
                            (" ON ", Style::default().fg(colors().on_color).bg(colors().success).add_modifier(Modifier::BOLD))
                        } else {
                            (" OFF ", Style::default().fg(colors().subtle).bg(colors().panel_focus))
                        };
                        put(f, bx.x, y, txt, bx.width, style);
                        if focused {
                            put(f, bx.x + 6, y, "Space toggles", bx.width.saturating_sub(6), Style::default().fg(colors().muted));
                        }
                    }
                }
            }
        }
    }

    if scroll > 0 {
        put(f, inner.right().saturating_sub(1), inner.y, "▲", 1, Style::default().fg(colors().muted));
    }
    if scroll + body_h < rows.len() {
        put(f, inner.right().saturating_sub(1), inner.y + body_h.saturating_sub(1) as u16, "▼", 1, Style::default().fg(colors().muted));
    }

    let hint_y = inner.bottom().saturating_sub(4);
    let hint = match (form.focused_field(), form.focused_button()) {
        (Some(field), _) => field.hint.clone(),
        (None, Some(Button::Submit)) => format!("Enter: {}", form.submit_label.to_lowercase()),
        (None, _) => "Enter: close without saving".into(),
    };
    put(f, inner.x, hint_y, &hint, inner.width, Style::default().fg(colors().subtle));
    if form.confirm_discard {
        put(f, inner.x, hint_y + 1, "Discard unsaved changes?  y yes · n no", inner.width, Style::default().fg(colors().warning).add_modifier(Modifier::BOLD));
    } else if let Some(e) = &form.error {
        put(f, inner.x, hint_y + 1, &format!("✗ {e}"), inner.width, Style::default().fg(colors().error).add_modifier(Modifier::BOLD));
    }

    let buttons_y = inner.bottom().saturating_sub(2);
    let mut bx = inner.right();
    for (b, label) in [(Button::Cancel, "Cancel".to_string()), (Button::Submit, form.submit_label.to_string())] {
        let text = format!("  {label}  ");
        let w = text.chars().count() as u16;
        bx = bx.saturating_sub(w);
        let focused = form.focused_button() == Some(b);
        let style = match (focused, b) {
            (true, _) => Style::default().fg(colors().on_color).bg(colors().accent).add_modifier(Modifier::BOLD),
            (false, Button::Submit) => Style::default().fg(colors().accent).bg(colors().panel_focus).add_modifier(Modifier::BOLD),
            (false, Button::Cancel) => Style::default().fg(colors().subtle).bg(colors().panel_focus),
        };
        put(f, bx, buttons_y, &text, w, style);
        out.rows.push((form.button_index(b), Rect { x: bx, y: buttons_y, width: w, height: 1 }));
        bx = bx.saturating_sub(2);
    }

    let insert = form.mode == Mode::Insert;
    let action = match (form.focused_field().map(|f| &f.kind), form.focused_button(), insert) {
        (_, Some(_), _) => "Enter activates · h/l switch",
        (_, _, true) => "Esc normal mode · Enter next",
        (Some(FieldKind::Select { .. }), _, _) => "h/l change · Enter list",
        (Some(FieldKind::Combo { options, .. }), _, _) if !options.is_empty() => "i edit · Enter suggestions · D clear",
        (Some(FieldKind::Toggle(_)), _, _) => "Space or h/l toggles",
        (Some(FieldKind::Secret { .. }), _, _) => "i type · D delete the saved one",
        _ => "i edit · S replace · D clear",
    };
    let moves = if insert { "Ctrl+j/k move" } else { "j/k move" };
    let quit = if insert { "" } else { " · q cancel" };
    let (badge, badge_style) = if insert {
        (" INSERT ", Style::default().fg(colors().on_color).bg(colors().success).add_modifier(Modifier::BOLD))
    } else {
        (" NORMAL ", Style::default().fg(colors().on_color).bg(colors().accent).add_modifier(Modifier::BOLD))
    };
    let keys = Line::from(vec![
        Span::styled(badge, badge_style),
        Span::styled(format!(" {action} · {moves} · "), Style::default().fg(colors().muted)),
        Span::styled(format!("Ctrl+S {}", form.submit_label.to_lowercase()), Style::default().fg(colors().accent)),
        Span::styled(quit.to_string(), Style::default().fg(colors().muted)),
    ]);
    f.render_widget(Paragraph::new(keys), Rect { x: inner.x, y: inner.bottom().saturating_sub(1), width: inner.width, height: 1 });

    if let (Some(p), Some(anchor)) = (&form.popup, focus_box) {
        let choices = form.choices(p.field);
        let max_items = 10usize.min(choices.len());
        let label_max = choices.iter().map(|c| c.label.chars().count()).max().unwrap_or(10);
        let detail_max = choices.iter().map(|c| c.detail.chars().count()).max().unwrap_or(0);
        let pw = ((label_max + detail_max + 6) as u16).max(anchor.width.min(40)).min(area.right().saturating_sub(anchor.x));
        let ph = max_items as u16 + 2;
        let below = anchor.y + 1;
        let py = if below + ph <= area.bottom() { below } else { anchor.y.saturating_sub(ph) };
        let prect = Rect { x: anchor.x, y: py, width: pw, height: ph };
        f.render_widget(Clear, prect);
        let pblock = Block::default().borders(Borders::ALL).border_type(BorderType::Rounded).border_style(Style::default().fg(colors().accent));
        let pin = pblock.inner(prect);
        f.render_widget(pblock, prect);
        let start = if p.selected >= max_items { p.selected + 1 - max_items } else { 0 };
        for (row, (idx, c)) in choices.iter().enumerate().skip(start).take(max_items).enumerate() {
            let y = pin.y + row as u16;
            let r = Rect { x: pin.x, y, width: pin.width, height: 1 };
            let selected = idx == p.selected;
            let base = if selected { Style::default().fg(colors().on_color).bg(colors().accent) } else { Style::default() };
            f.buffer_mut().set_style(r, base);
            put(f, pin.x + 1, y, &c.label, pin.width.saturating_sub(1), base.add_modifier(Modifier::BOLD));
            if !c.detail.is_empty() {
                let x = pin.x + 1 + label_max as u16 + 2;
                if x < pin.right() {
                    put(f, x, y, &c.detail, pin.right() - x, if selected { base } else { Style::default().fg(colors().muted) });
                }
            }
            out.popup.push((idx, r));
        }
        if !matches!(form.fields[p.field].kind, FieldKind::Combo { .. }) {
            out.cursor = None;
        }
    }
    out
}

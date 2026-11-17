use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use super::{ACCENT, MUTED};
use crate::app::sftp::{Panel, TransferState};
use crate::app::{App, Focus, TabKind};

pub fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n}B") } else { format!("{v:.1}{}", U[i]) }
}

fn fmt_time(secs: Option<u64>) -> String {
    let Some(s) = secs else { return String::new() };
    let days = s / 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    let hm = s % 86_400;
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", hm / 3600, (hm % 3600) / 60)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Main && app.dialogs.is_empty();
    let Some(TabKind::Sftp(view)) = app.tabs.get_mut(app.active).map(|t| &mut t.kind) else { return };
    let active_transfers: Vec<_> = view.transfers.iter().rev().take(6).collect();
    let transfers_h = if active_transfers.is_empty() { 0 } else { active_transfers.len() as u16 + 2 };
    let [panels, transfers, status] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(transfers_h), Constraint::Length(1)]).areas(area);
    let [left, right] = Layout::horizontal([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)]).areas(panels);

    let mut rects = Vec::new();
    let remote_active = view.remote_active;
    let remote_title = format!(" {} : {} ", view.name, view.remote.cwd);
    let local_title = format!(" local : {} ", view.local.cwd);
    rects.push((false, draw_panel(f, &mut view.local, left, &local_title, focused && !remote_active)));
    rects.push((true, draw_panel(f, &mut view.remote, right, &remote_title, focused && remote_active)));

    if transfers_h > 0 {
        let block = Block::default().borders(Borders::TOP).title(" Transfers (x clears finished ones) ").border_style(Style::default().fg(MUTED));
        let inner = block.inner(transfers);
        f.render_widget(block, transfers);
        for (i, t) in view.transfers.iter().rev().take(6).enumerate() {
            let row = Rect { x: inner.x, y: inner.y + i as u16, width: inner.width, height: 1 };
            let [label_r, gauge_r] = Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(row);
            let arrow = if t.upload { "↑" } else { "↓" };
            let (state, color) = match &t.state {
                TransferState::Queued => ("queued".to_string(), Color::Gray),
                TransferState::Running => {
                    let secs = t.started.elapsed().as_secs_f64().max(0.1);
                    (format!("{}/s", human((t.done as f64 / secs) as u64)), ACCENT)
                }
                TransferState::Done => ("done".to_string(), Color::Green),
                TransferState::Failed(e) => (format!("error: {e}"), Color::LightRed),
            };
            let label = format!("{arrow} {} · {} / {} · {state}", t.label, human(t.done), human(t.total));
            f.buffer_mut().set_stringn(label_r.x, label_r.y, &label, label_r.width as usize, Style::default().fg(color));
            let ratio = if t.total == 0 { if t.state == TransferState::Done { 1.0 } else { 0.0 } } else { (t.done as f64 / t.total as f64).min(1.0) };
            f.render_widget(Gauge::default().ratio(ratio).gauge_style(Style::default().fg(color).bg(Color::Indexed(237))), gauge_r);
        }
    }

    let help = "Tab side · Enter/l open · h/⌫ up · Space mark · F5/c copy to other side · F6/m rename · F7/n mkdir · F8/d delete · / go to · . hidden · r refresh · q close";
    let line = Line::from(vec![Span::styled(view.status.clone(), Style::default().fg(Color::White)), Span::raw("  "), Span::styled(help, Style::default().fg(MUTED))]);
    f.render_widget(Paragraph::new(line), status);
    app.layout.sftp_panels = rects;
}

fn draw_panel(f: &mut Frame, p: &mut Panel, area: Rect, title: &str, focused: bool) -> Rect {
    let color = if focused { ACCENT } else { MUTED };
    let block = Block::default().borders(Borders::ALL).border_style(Style::default().fg(color)).title(Span::styled(title.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if p.loading && p.entries.is_empty() {
        f.render_widget(Paragraph::new("Loading…").style(Style::default().fg(MUTED)), inner);
        return inner;
    }
    if let Some(e) = &p.error {
        f.render_widget(Paragraph::new(e.clone()).style(Style::default().fg(Color::LightRed)).wrap(ratatui::widgets::Wrap { trim: true }), inner);
        return inner;
    }
    let h = inner.height as usize;
    if p.selected < p.scroll {
        p.scroll = p.selected;
    } else if h > 0 && p.selected >= p.scroll + h {
        p.scroll = p.selected + 1 - h;
    }
    let w = inner.width as usize;
    let meta_w = 28usize.min(w / 2);
    for (row, (i, e)) in p.entries.iter().enumerate().skip(p.scroll).take(h).enumerate() {
        let y = inner.y + row as u16;
        let marked = p.marked.contains(&e.name);
        let mut style = if e.is_dir { Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD) } else { Style::default() };
        if e.is_link {
            style = style.fg(Color::Cyan);
        }
        if marked {
            style = style.fg(Color::Yellow);
        }
        if i == p.selected {
            style = if focused { style.add_modifier(Modifier::REVERSED) } else { style.add_modifier(Modifier::UNDERLINED) };
            f.buffer_mut().set_style(Rect { x: inner.x, y, width: inner.width, height: 1 }, style);
        }
        let name = format!("{}{}{}", if marked { "* " } else { "" }, e.name, if e.is_dir && e.name != ".." { "/" } else { "" });
        let meta = if e.name == ".." { String::new() } else if e.is_dir { fmt_time(e.mtime) } else { format!("{:>7} {}", human(e.size), fmt_time(e.mtime)) };
        f.buffer_mut().set_stringn(inner.x, y, &name, w.saturating_sub(meta_w + 1), style);
        let mx = inner.right().saturating_sub(meta.chars().count() as u16);
        if meta_w > 0 {
            f.buffer_mut().set_stringn(mx, y, &meta, meta_w, style.fg(if i == p.selected && focused { Color::Reset } else { MUTED }));
        }
    }
    inner
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        assert_eq!(human(512), "512B");
        assert_eq!(human(2048), "2.0K");
        assert_eq!(fmt_time(Some(0)), "1970-01-01 00:00");
        assert_eq!(fmt_time(Some(1_700_000_000)), "2023-11-14 22:13");
    }
}

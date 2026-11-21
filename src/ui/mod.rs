mod dialogs;
mod form;
mod sftp;
pub mod theme;
mod tunnels;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::app::keymap::{self, Ctx};
use crate::app::sidebar::Item;
use crate::app::{App, Focus, TabKind, ToastKind};
use crate::term::pane::GridSize;
use crate::term::render::{self, RenderOpts};
use theme::colors;

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let sidebar_w = if app.sidebar_visible { app.cfg.sidebar_width.min(main.width / 2) } else { 0 };
    let [side, content] = Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(1)]).areas(main);
    let [tabbar, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(content);

    app.layout.sidebar = side;
    app.layout.tabbar = tabbar;
    app.layout.body = body;
    app.layout.panes.clear();
    app.layout.sftp_panels.clear();
    app.layout.tunnel_list = Rect::default();

    if sidebar_w > 0 {
        draw_sidebar(f, app, side);
    }
    draw_tabbar(f, app, tabbar);
    let cursor = draw_body(f, app, body);
    draw_status(f, app, status);

    app.layout.form_rows.clear();
    app.layout.popup_rows.clear();
    if let Some(d) = app.dialogs.pop_front() {
        let pos = dialogs::draw(f, app, &d, area);
        app.dialogs.push_front(d);
        if let Some(p) = pos {
            f.set_cursor_position(p);
        }
    } else if let Some(p) = cursor {
        f.set_cursor_position(p);
    }
}

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar;
    if !focused {
        app.sidebar.visual = None;
    }
    let items = app.sidebar.items(&app.store);
    app.sidebar.clamp(items.len());
    let visual = app.sidebar.visual_range();
    let border = if focused { Style::default().fg(colors().accent) } else { Style::default().fg(colors().muted) };
    let title = match (&app.sidebar.search, app.sidebar.query.is_empty()) {
        _ if visual.is_some() => format!(" Hosts · {} selected ", app.sidebar.visual_hosts(&app.store, &items).len()),
        (Some(_), _) => " Search ".to_string(),
        (None, false) => format!(" Hosts · /{} ", app.sidebar.query),
        _ => format!(" Hosts ({}) ", app.store.hosts.len()),
    };
    let block =
        Block::default().borders(Borders::RIGHT).border_style(border).title(Span::styled(title, border.add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut list_area = inner;
    if let Some(search) = &app.sidebar.search {
        let [line, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
        f.buffer_mut().set_string(line.x, line.y, "/", Style::default().fg(colors().accent));
        let input_area = Rect { x: line.x + 1, width: line.width.saturating_sub(1), ..line };
        if let Some(p) = search.render(input_area, f.buffer_mut(), focused, Style::default()) {
            f.set_cursor_position(p);
        }
        list_area = rest;
    }
    app.layout.sidebar_list = list_area;
    app.sidebar.ensure_visible(list_area.height as usize);
    if items.is_empty() {
        let msg = if app.sidebar.query.is_empty() {
            vec![
                Line::from("No hosts yet."),
                Line::from(""),
                Line::from(Span::styled("n  new host", Style::default().fg(colors().accent))),
                Line::from(Span::styled("i  import", Style::default().fg(colors().accent))),
                Line::from(Span::styled("t  local shell", Style::default().fg(colors().accent))),
            ]
        } else {
            vec![Line::from("No matches")]
        };
        f.render_widget(Paragraph::new(msg), list_area);
        return;
    }
    let connected: std::collections::HashSet<&str> =
        app.panes.values().filter(|p| p.exited.is_none()).filter_map(|p| p.host_id.as_deref()).collect();
    for (row, (idx, item)) in items.iter().enumerate().skip(app.sidebar.scroll).take(list_area.height as usize).enumerate() {
        let y = list_area.y + row as u16;
        let selected = idx == app.sidebar.selected;
        let (text, style) = match item {
            Item::Folder { name, depth, expanded, count, .. } => {
                let arrow = if *expanded { "▾" } else { "▸" };
                (
                    format!("{}{arrow} {name} ({count})", "  ".repeat(*depth)),
                    Style::default().fg(colors().folder).add_modifier(Modifier::BOLD),
                )
            }
            Item::Host { id, depth, .. } => {
                let Some(h) = item.host(&app.store) else { continue };
                let mark = if app.sidebar.marked.contains(id) {
                    "✓"
                } else if connected.contains(id.as_str()) {
                    "●"
                } else {
                    " "
                };
                let indent = if app.sidebar.query.is_empty() { "  ".repeat(*depth) } else { String::new() };
                let mut label = if app.sidebar.query.is_empty() { h.name.clone() } else { h.path() };
                if app.tunnels.is_on(id) {
                    label.push_str(" ⇄");
                }
                let style = if app.sidebar.marked.contains(id) {
                    Style::default().fg(colors().success)
                } else if connected.contains(id.as_str()) {
                    Style::default().fg(colors().accent)
                } else {
                    Style::default()
                };
                (format!("{indent}{mark} {label}"), style)
            }
        };
        let in_visual = visual.is_some_and(|(lo, hi)| (lo..=hi).contains(&idx));
        let style = if selected {
            if focused { style.add_modifier(Modifier::REVERSED) } else { style.add_modifier(Modifier::UNDERLINED) }
        } else if in_visual {
            style.bg(colors().panel_focus)
        } else {
            style
        };
        let line_rect = Rect { x: list_area.x, y, width: list_area.width, height: 1 };
        if selected || in_visual {
            f.buffer_mut().set_style(line_rect, style);
        }
        f.buffer_mut().set_stringn(list_area.x, y, &text, list_area.width as usize, style);
    }

    if let Some(h) = items.get(app.sidebar.selected).and_then(|i| i.host(&app.store))
        && list_area.height > 6
        && items.len() < (list_area.height as usize).saturating_sub(3)
    {
        let y = list_area.bottom() - 2;
        let target = h.display_target();
        f.buffer_mut().set_stringn(list_area.x, y, &target, list_area.width as usize, Style::default().fg(colors().muted));
        if let Some(j) = &h.jump {
            f.buffer_mut().set_stringn(
                list_area.x,
                y + 1,
                format!("via {j}"),
                list_area.width as usize,
                Style::default().fg(colors().muted),
            );
        }
    }
}

fn draw_tabbar(f: &mut Frame, app: &mut App, area: Rect) {
    app.layout.tabs.clear();
    let buf = f.buffer_mut();
    buf.set_style(area, Style::default().bg(colors().panel));
    let mut x = area.x;
    for (i, tab) in app.tabs.iter().enumerate() {
        let active = i == app.active;
        let (label, broadcast, bell) = match &tab.kind {
            TabKind::Terms(t) => {
                let names: Vec<&str> = t.panes.iter().filter_map(|p| app.panes.get(p)).map(|p| p.name.as_str()).collect();
                let label = match (&tab.title, names.len()) {
                    (Some(t), _) => t.clone(),
                    (None, 1) => names[0].to_string(),
                    (None, n) => format!("{} +{}", names.first().unwrap_or(&""), n - 1),
                };
                let bell = t.panes.iter().any(|p| app.panes.get(p).is_some_and(|p| p.bell));
                (label, t.broadcast, bell)
            }
            TabKind::Sftp(v) => (tab.title.clone().unwrap_or_else(|| format!("sftp:{}", v.name)), false, false),
            TabKind::Tunnels(_) => (tab.title.clone().unwrap_or_else(|| tunnels::tab_label(app)), false, false),
        };
        let text = format!(" {}:{}{}{} ", i + 1, label, if broadcast { " ⚡" } else { "" }, if bell { " !" } else { "" });
        let w = (text.chars().count() as u16).min(area.right().saturating_sub(x));
        if w == 0 {
            break;
        }
        let style = if active {
            Style::default()
                .fg(colors().on_color)
                .bg(if broadcast { colors().broadcast } else { colors().accent })
                .add_modifier(Modifier::BOLD)
        } else if broadcast {
            Style::default().fg(colors().broadcast).bg(colors().panel)
        } else {
            Style::default().fg(colors().subtle).bg(colors().panel)
        };
        buf.set_stringn(x, area.y, &text, w as usize, style);
        app.layout.tabs.push((i, Rect { x, y: area.y, width: w, height: 1 }));
        x += w;
    }
    if app.tabs.is_empty() {
        buf.set_stringn(area.x + 1, area.y, "no sessions", area.width as usize, Style::default().fg(colors().muted).bg(colors().panel));
    }
}

/// Splits `area` into a grid for `n` panes (MultiExec-style tiling).
pub fn grid(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let mut out = Vec::with_capacity(n);
    let row_rects = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(area);
    for (r, row_rect) in row_rects.iter().enumerate() {
        let in_row = if r == rows - 1 { n - cols * (rows - 1) } else { cols };
        let col_rects = Layout::horizontal(vec![Constraint::Ratio(1, in_row as u32); in_row]).split(*row_rect);
        out.extend(col_rects.iter().copied());
    }
    out
}

fn draw_body(f: &mut Frame, app: &mut App, area: Rect) -> Option<Position> {
    let Some(tab) = app.tabs.get(app.active) else {
        draw_welcome(f, app, area);
        return None;
    };
    match &tab.kind {
        TabKind::Sftp(_) => {
            sftp::draw(f, app, area);
            None
        }
        TabKind::Tunnels(_) => {
            tunnels::draw(f, app, area);
            None
        }
        TabKind::Terms(t) => {
            let panes: Vec<_> = if t.zoomed { t.panes.get(t.focused).copied().into_iter().collect() } else { t.panes.clone() };
            let focused_id = t.panes.get(t.focused).copied();
            let broadcast = t.broadcast;
            let rects = grid(area, panes.len());
            let opts = RenderOpts { truecolor: app.truecolor };
            let main_focus = app.focus == Focus::Main && app.dialogs.is_empty();
            let mut cursor = None;
            let bordered = panes.len() > 1 || broadcast;
            for (id, rect) in panes.iter().zip(rects) {
                let Some(pane) = app.panes.get(id) else { continue };
                let is_focused = Some(*id) == focused_id;
                let inner = if bordered {
                    let in_broadcast = broadcast && !pane.excluded;
                    let color = if in_broadcast {
                        colors().broadcast
                    } else if is_focused && main_focus {
                        colors().accent
                    } else {
                        colors().muted
                    };
                    let mut title = format!(" {} ", pane.name);
                    if pane.excluded && broadcast {
                        title.push_str("[excluded] ");
                    }
                    if pane.copy_mode {
                        title.push_str("[copy] ");
                    }
                    let block = Block::default()
                        .borders(Borders::ALL)
                        .border_type(if is_focused { BorderType::Thick } else { BorderType::Plain })
                        .border_style(Style::default().fg(color))
                        .title(Span::styled(title, Style::default().fg(color).add_modifier(Modifier::BOLD)));
                    let inner = block.inner(rect);
                    f.render_widget(block, rect);
                    inner
                } else {
                    rect
                };
                if inner.width == 0 || inner.height == 0 {
                    continue;
                }
                pane.term.resize(GridSize { cols: inner.width, rows: inner.height });
                app.layout.panes.push((*id, inner));
                let emu = pane.term.lock();
                let pos = render::render(&emu, inner, f.buffer_mut(), &opts);
                drop(emu);
                if is_focused && main_focus && pane.exited.is_none() {
                    cursor = pos;
                }
                if let Some(code) = pane.exited {
                    let msg = match code {
                        Some(0) => " Session ended · r/Enter reconnects · w closes ".to_string(),
                        Some(c) => format!(" Session ended (exit {c}) · r/Enter reconnects · w closes "),
                        None => " Session ended · r/Enter reconnects · w closes ".to_string(),
                    };
                    let w = (msg.chars().count() as u16).min(inner.width);
                    let x = inner.x + inner.width.saturating_sub(w) / 2;
                    let y = inner.bottom().saturating_sub(1);
                    f.buffer_mut().set_stringn(x, y, &msg, w as usize, Style::default().fg(colors().on_color).bg(colors().warning));
                }
            }
            cursor
        }
    }
}

fn draw_welcome(f: &mut Frame, app: &App, area: Rect) {
    let leader = app.leader.label();
    let k = |s: &str| Span::styled(s.to_string(), Style::default().fg(colors().accent).add_modifier(Modifier::BOLD));
    let mut lines = vec![
        Line::from(Span::styled("sshm", Style::default().add_modifier(Modifier::BOLD).fg(colors().accent))),
        Line::from("SSH session manager"),
        Line::from(""),
    ];
    for row in keymap::help_rows(Ctx::Hosts, &app.leader).into_iter().filter(|r| r.welcome) {
        lines.push(Line::from(vec![k(&row.keys), Span::raw(format!("  {}", row.desc))]));
    }
    lines.extend([
        Line::from(""),
        Line::from(vec![k(&leader), Span::raw(" is the leader key inside a session:")]),
        Line::from(vec![k(&format!("{leader} ?")), Span::raw("  full help")]),
    ]);
    let h = lines.len() as u16;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let r = Rect { x: area.x, y, width: area.width, height: h.min(area.height) };
    f.render_widget(Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center), r);
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let buf = f.buffer_mut();
    buf.set_style(area, Style::default().bg(colors().status_bar));
    let mut spans = Vec::new();
    if app.leader_pending {
        spans.push(Span::styled(" LEADER ", Style::default().fg(colors().on_color).bg(colors().warning).add_modifier(Modifier::BOLD)));
        spans.push(Span::raw(" "));
    }
    if let Some(t) = app.active_term_tab()
        && t.broadcast
    {
        let n = t.panes.iter().filter(|p| app.panes.get(p).is_some_and(|p| !p.excluded)).count();
        spans.push(Span::styled(
            format!(" BROADCAST {n} "),
            Style::default().fg(colors().text).bg(colors().broadcast).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
    }
    let sidebar_visual = app.focus == Focus::Sidebar && app.sidebar.visual.is_some();
    if sidebar_visual {
        spans.push(Span::styled(" VISUAL ", Style::default().fg(colors().on_color).bg(colors().visual).add_modifier(Modifier::BOLD)));
        spans.push(Span::raw(" "));
    }
    if app.focused_pane().is_some_and(|p| p.copy_mode) {
        spans.push(Span::styled(" COPY ", Style::default().fg(colors().on_color).bg(colors().success).add_modifier(Modifier::BOLD)));
        spans.push(Span::raw(" "));
    }
    let leader = app.leader.label();
    if let Some((msg, _, kind)) = &app.toast {
        let style = match kind {
            ToastKind::Info => Style::default().fg(colors().text),
            ToastKind::Error => Style::default().fg(colors().error).add_modifier(Modifier::BOLD),
        };
        spans.push(Span::styled(msg.clone(), style));
    } else if app.focus == Focus::Main && app.focused_pane().is_some_and(|p| p.prompt.is_some() && p.host_id.is_some()) {
        spans.push(Span::styled(
            format!("Password prompt · {leader} p types it ({leader} P: root password)"),
            Style::default().fg(colors().warning).add_modifier(Modifier::BOLD),
        ));
    } else if app.leader_pending {
        spans.push(Span::styled(keymap::hints(Ctx::Leader, &app.leader), Style::default().fg(colors().subtle)));
    } else if sidebar_visual {
        spans.push(Span::styled(format!("j/k extend · {}", keymap::hints(Ctx::Visual, &app.leader)), Style::default().fg(colors().subtle)));
    } else if app.focus == Focus::Sidebar {
        spans.push(Span::styled(keymap::hints(Ctx::Hosts, &app.leader), Style::default().fg(colors().subtle)));
    } else if matches!(app.tabs.get(app.active).map(|t| &t.kind), Some(TabKind::Tunnels(_))) {
        spans.push(Span::styled(format!("j/k move · {}", keymap::hints(Ctx::Tunnels, &app.leader)), Style::default().fg(colors().subtle)));
    } else {
        spans.push(Span::styled(
            format!("{leader} is the leader key · {leader} ? help · {leader} Tab hosts"),
            Style::default().fg(colors().muted),
        ));
    }
    let vault = if app.vault.is_unlocked() {
        Span::styled(" vault unlocked ", Style::default().fg(colors().on_color).bg(colors().success))
    } else if app.vault.exists() {
        Span::styled(" vault locked ", Style::default().fg(colors().subtle).bg(colors().panel_focus))
    } else {
        Span::styled(" no vault ", Style::default().fg(colors().subtle).bg(colors().panel_focus))
    };
    let mut right = Vec::new();
    match app.tunnels.running() {
        0 => {}
        n => {
            let label = if n == 1 { " ⇄ 1 tunnel ".to_string() } else { format!(" ⇄ {n} tunnels ") };
            right.push(Span::styled(label, Style::default().fg(colors().on_color).bg(colors().accent)));
            right.push(Span::raw(" "));
        }
    }
    right.push(vault);
    let right = Line::from(right);
    let right_w = right.width() as u16;
    let left_w = area.width.saturating_sub(right_w + 1);
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { width: left_w, ..area });
    f.render_widget(Paragraph::new(right), Rect { x: area.right().saturating_sub(right_w), width: right_w, ..area });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_layout() {
        let area = Rect::new(0, 0, 100, 40);
        assert_eq!(grid(area, 1), vec![area]);
        let g = grid(area, 3);
        assert_eq!(g.len(), 3);
        assert_eq!(g[2].width, 100);
        let g = grid(area, 4);
        assert!(g.iter().all(|r| r.width == 50 && r.height == 20));
        assert_eq!(grid(area, 5).len(), 5);
    }
}

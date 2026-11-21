use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::theme::colors;
use crate::app::tunnels::{self, ConnState, TunnelState};
use crate::app::{App, Focus, TabKind};
use crate::store::hosts::{Host, Tunnel, TunnelStart};

/// "tunnels", or "tunnels · 2 on" while some run.
pub fn tab_label(app: &App) -> String {
    match app.tunnels.running() {
        0 => "tunnels".into(),
        n => format!("tunnels · {n} on"),
    }
}

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Main && app.dialogs.is_empty();
    let Some(TabKind::Tunnels(view)) = app.tabs.get(app.active).map(|t| &t.kind) else { return };
    let rows = tunnels::rows(&app.store, view.extra.as_deref());
    if rows.is_empty() {
        let k = |s: &str| Span::styled(s.to_string(), Style::default().fg(colors().accent).add_modifier(Modifier::BOLD));
        let lines = vec![
            Line::from("No tunnels yet."),
            Line::from(""),
            Line::from(vec![Span::raw("In the host list, "), k("T"), Span::raw(" on a host adds one through it,")]),
            Line::from(vec![Span::raw("or "), k("n"), Span::raw(" here, picking the host in the form.")]),
        ];
        let y = area.y + area.height.saturating_sub(lines.len() as u16) / 2;
        let r = Rect { y, height: (lines.len() as u16).min(area.height), ..area };
        f.render_widget(Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center), r);
        app.layout.tunnel_list = Rect::default();
        return;
    }

    let tunnels: Vec<(&Host, &Tunnel)> = app.store.hosts.iter().flat_map(|h| h.tunnels.iter().map(move |t| (h, t))).collect();
    let name_w = tunnels.iter().map(|(_, t)| t.name.trim().chars().count()).max().unwrap_or(0).clamp(4, 24);
    let sum_w = tunnels.iter().map(|(_, t)| t.summary().chars().count()).max().unwrap_or(0).clamp(10, 48);
    let header = format!("   {:<name_w$}  {:<6}  {:<sum_w$}  STATUS", "NAME", "TYPE", "LISTEN → DESTINATION");
    f.buffer_mut().set_stringn(area.x, area.y, &header, area.width as usize, Style::default().fg(colors().muted));
    let list = Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area };
    app.layout.tunnel_list = list;

    let Some(TabKind::Tunnels(view)) = app.tabs.get_mut(app.active).map(|t| &mut t.kind) else { return };
    view.selected = view.selected.min(rows.len() - 1);
    let h = list.height as usize;
    if view.selected < view.scroll {
        view.scroll = view.selected;
    } else if h > 0 && view.selected >= view.scroll + h {
        view.scroll = view.selected + 1 - h;
    }
    let (selected, scroll) = (view.selected, view.scroll);

    for (i, (host_id, tunnel_id)) in rows.iter().enumerate().skip(scroll).take(h) {
        let y = list.y + (i - scroll) as u16;
        let Some(host) = app.store.get(host_id) else { continue };
        let line = match tunnel_id.as_deref().and_then(|t| host.tunnels.iter().find(|x| x.id == t)) {
            None => host_line(app, host),
            Some(t) => tunnel_line(app, host, t, name_w, sum_w),
        };
        let row = Rect { y, height: 1, ..list };
        f.buffer_mut().set_line(list.x, y, &line, list.width);
        if i == selected {
            let m = if focused { Modifier::REVERSED } else { Modifier::UNDERLINED };
            f.buffer_mut().set_style(row, Style::default().add_modifier(m));
        }
    }
}

fn host_line<'a>(app: &App, host: &Host) -> Line<'a> {
    let mut spans = vec![
        Span::styled(format!("▾ {}", host.path()), Style::default().fg(colors().folder).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {}", host.display_target()), Style::default().fg(colors().muted)),
    ];
    let conn = app.tunnels.conns.get(&host.id);
    let status = match conn.map(|c| c.state) {
        Some(ConnState::Connecting) => Some(("connecting…", colors().warning)),
        Some(ConnState::Up) => Some(("connected", colors().success)),
        Some(ConnState::Retrying(_)) => Some(("connection lost · reconnecting…", colors().warning)),
        None if host.tunnels.is_empty() => Some(("no tunnels yet · n adds one", colors().muted)),
        None => None,
    };
    if let Some((text, color)) = status {
        spans.push(Span::styled(format!("  {text}"), Style::default().fg(color)));
    }
    if let Some(c) = conn
        && let Some(msg) = &c.message
    {
        // While up it is something that already happened, not the state of the connection.
        let msg = if c.state == ConnState::Up { format!(" · last error: {msg}") } else { format!(" · {msg}") };
        spans.push(Span::styled(msg, Style::default().fg(colors().subtle)));
    }
    Line::from(spans)
}

fn tunnel_line<'a>(app: &App, host: &Host, t: &Tunnel, name_w: usize, sum_w: usize) -> Line<'a> {
    let conn = app.tunnels.conns.get(&host.id).map(|c| c.state);
    let (glyph, status, color) = match app.tunnels.state(&host.id, &t.id) {
        Some(TunnelState::On) => ("●", "on".to_string(), colors().success),
        Some(TunnelState::Starting) => {
            let s = match conn {
                Some(ConnState::Up) => "starting…",
                Some(ConnState::Retrying(_)) => "reconnecting…",
                _ => "connecting…",
            };
            ("◌", s.to_string(), colors().warning)
        }
        Some(TunnelState::Failed(e)) => ("✗", format!("failed: {e}"), colors().error),
        None => ("○", "off".to_string(), colors().muted),
    };
    let start = match t.start {
        TunnelStart::Manual => "",
        TunnelStart::Launch => " · starts with sshm",
        TunnelStart::Connect => " · starts with a session",
    };
    let name: String = t.name.trim().chars().take(name_w).collect();
    let summary: String = t.summary().chars().take(sum_w).collect();
    Line::from(vec![
        Span::styled(format!(" {glyph} "), Style::default().fg(color)),
        Span::styled(format!("{name:<name_w$}  "), Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{:<6}  ", t.kind.label()), Style::default().fg(colors().subtle)),
        Span::raw(format!("{summary:<sum_w$}  ")),
        Span::styled(status, Style::default().fg(color)),
        Span::styled(start, Style::default().fg(colors().muted)),
    ])
}

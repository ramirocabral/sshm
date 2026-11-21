use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::point_to_viewport;
use alacritty_terminal::vte::ansi::{Color as AColor, CursorShape, NamedColor};
use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier};

use super::pane::Emulator;

pub struct RenderOpts {
    pub truecolor: bool,
}

/// Draws the visible part of the terminal into `area`; returns where the cursor should go.
pub fn render(emu: &Emulator, area: Rect, buf: &mut Buffer, opts: &RenderOpts) -> Option<Position> {
    let content = emu.term.renderable_content();
    let offset = content.display_offset;
    let selection = content.selection;

    for indexed in content.display_iter {
        let Some(vp) = point_to_viewport(offset, indexed.point) else { continue };
        if vp.line >= area.height as usize || vp.column.0 >= area.width as usize {
            continue;
        }
        let x = area.x + vp.column.0 as u16;
        let y = area.y + vp.line as u16;
        let cell = indexed.cell;
        let Some(out) = buf.cell_mut((x, y)) else { continue };

        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            out.reset();
            continue;
        }

        // alacritty keeps '\t' in the cell where a tab started; printed raw it moves the real cursor.
        let c = if cell.c.is_control() { ' ' } else { cell.c };
        out.reset();
        match cell.zerowidth() {
            Some(zw) => out.set_symbol(&std::iter::once(c).chain(zw.iter().copied()).collect::<String>()),
            None => out.set_char(c),
        };

        let mut fg = map_color(cell.fg, opts);
        let mut bg = map_color(cell.bg, opts);
        let mut modifier = Modifier::empty();
        if cell.flags.contains(Flags::BOLD) {
            modifier |= Modifier::BOLD;
        }
        if cell.flags.contains(Flags::DIM) || is_dim(cell.fg) {
            modifier |= Modifier::DIM;
        }
        if cell.flags.contains(Flags::ITALIC) {
            modifier |= Modifier::ITALIC;
        }
        if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            modifier |= Modifier::UNDERLINED;
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            modifier |= Modifier::CROSSED_OUT;
        }
        if cell.flags.contains(Flags::HIDDEN) {
            modifier |= Modifier::HIDDEN;
        }
        let selected = selection.is_some_and(|s| s.contains(indexed.point));
        if cell.flags.contains(Flags::INVERSE) != selected {
            std::mem::swap(&mut fg, &mut bg);
            if fg == Color::Reset && bg == Color::Reset {
                modifier |= Modifier::REVERSED;
            } else {
                fg = if fg == Color::Reset { Color::Black } else { fg };
                bg = if bg == Color::Reset { Color::White } else { bg };
            }
        }
        out.fg = fg;
        out.bg = bg;
        out.modifier = modifier;
    }

    if content.cursor.shape == CursorShape::Hidden {
        return None;
    }
    let vp = point_to_viewport(offset, content.cursor.point)?;
    if vp.line >= area.height as usize || vp.column.0 >= area.width as usize {
        return None;
    }
    Some(Position::new(area.x + vp.column.0 as u16, area.y + vp.line as u16))
}

fn is_dim(c: AColor) -> bool {
    matches!(
        c,
        AColor::Named(
            NamedColor::DimBlack
                | NamedColor::DimRed
                | NamedColor::DimGreen
                | NamedColor::DimYellow
                | NamedColor::DimBlue
                | NamedColor::DimMagenta
                | NamedColor::DimCyan
                | NamedColor::DimWhite
        )
    )
}

pub fn map_color(c: AColor, opts: &RenderOpts) -> Color {
    match c {
        AColor::Named(n) => match n {
            NamedColor::Black | NamedColor::DimBlack => Color::Indexed(0),
            NamedColor::Red | NamedColor::DimRed => Color::Indexed(1),
            NamedColor::Green | NamedColor::DimGreen => Color::Indexed(2),
            NamedColor::Yellow | NamedColor::DimYellow => Color::Indexed(3),
            NamedColor::Blue | NamedColor::DimBlue => Color::Indexed(4),
            NamedColor::Magenta | NamedColor::DimMagenta => Color::Indexed(5),
            NamedColor::Cyan | NamedColor::DimCyan => Color::Indexed(6),
            NamedColor::White | NamedColor::DimWhite => Color::Indexed(7),
            NamedColor::BrightBlack => Color::Indexed(8),
            NamedColor::BrightRed => Color::Indexed(9),
            NamedColor::BrightGreen => Color::Indexed(10),
            NamedColor::BrightYellow => Color::Indexed(11),
            NamedColor::BrightBlue => Color::Indexed(12),
            NamedColor::BrightMagenta => Color::Indexed(13),
            NamedColor::BrightCyan => Color::Indexed(14),
            NamedColor::BrightWhite => Color::Indexed(15),
            _ => Color::Reset,
        },
        AColor::Indexed(i) => Color::Indexed(i),
        AColor::Spec(rgb) => {
            if opts.truecolor {
                Color::Rgb(rgb.r, rgb.g, rgb.b)
            } else {
                Color::Indexed(rgb_to_256(rgb.r, rgb.g, rgb.b))
            }
        }
    }
}

/// Nearest xterm-256 color (6x6x6 cube or grayscale ramp).
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let to_cube = |v: u8| -> u8 { if v < 48 { 0 } else if v < 115 { 1 } else { (v - 35) / 40 } };
    let (cr, cg, cb) = (to_cube(r), to_cube(g), to_cube(b));
    let cube_val = |c: u8| if c == 0 { 0 } else { 55 + c as i32 * 40 };
    let cube_idx = 16 + 36 * cr + 6 * cg + cb;
    let cube_dist = sq(cube_val(cr) - r as i32) + sq(cube_val(cg) - g as i32) + sq(cube_val(cb) - b as i32);

    let avg = (r as i32 + g as i32 + b as i32) / 3;
    let gray_step = if avg > 238 { 23 } else if avg < 8 { 0 } else { ((avg - 8) / 10) as u8 };
    let gray_val = 8 + gray_step as i32 * 10;
    let gray_dist = sq(gray_val - r as i32) + sq(gray_val - g as i32) + sq(gray_val - b as i32);

    if gray_dist < cube_dist { 232 + gray_step } else { cube_idx }
}

fn sq(v: i32) -> i32 {
    v * v
}

pub fn truecolor_supported() -> bool {
    std::env::var("COLORTERM").map(|v| v.contains("truecolor") || v.contains("24bit")).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_render_as_blanks() {
        use super::super::pane::GridSize;
        let mut emu = Emulator::headless(GridSize { cols: 20, rows: 2 });
        emu.advance(b"ab\tXY");
        let area = Rect::new(0, 0, 20, 2);
        let mut buf = Buffer::empty(area);
        render(&emu, area, &mut buf, &RenderOpts { truecolor: false });
        let row: String = (0..20).map(|x| buf[(x, 0)].symbol()).collect();
        assert_eq!(row.trim_end(), "ab      XY");
    }

    #[test]
    fn combining_marks_stay_in_their_cell() {
        use super::super::pane::GridSize;
        let mut emu = Emulator::headless(GridSize { cols: 10, rows: 1 });
        emu.advance("e\u{301}x".as_bytes());
        let area = Rect::new(0, 0, 10, 1);
        let mut buf = Buffer::empty(area);
        render(&emu, area, &mut buf, &RenderOpts { truecolor: false });
        assert_eq!(buf[(0, 0)].symbol(), "e\u{301}");
        assert_eq!(buf[(1, 0)].symbol(), "x");
    }

    #[test]
    fn rgb_approximation() {
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(255, 0, 0), 196);
        assert_eq!(rgb_to_256(128, 128, 128), 244);
        assert_eq!(rgb_to_256(255, 255, 255), 231);
    }
}

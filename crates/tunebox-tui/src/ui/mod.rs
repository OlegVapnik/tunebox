mod artist;
mod content;
mod help;
mod hints;
mod home;
mod library;
mod menu;
mod picker;
mod player_bar;
mod settings;
mod sidebar;
mod star;
mod text;
mod topbar;
mod wordmark;
mod zen;

use ratatui::widgets::Widget;
use ratatui::{
    Frame,
    buffer::{Buffer, Cell, CellDiffOption},
    layout::{Constraint, Layout, Rect, Size},
    style::{Color, Modifier, Style},
};
use ratatui_image::Image;
use unicode_width::UnicodeWidthStr;

use crate::clock::Instant;
use crate::{
    app::{App, Focus, Hit, not_animated},
    graphics::INLINE_CELL,
    icons::icons,
    loader::{Graphic, Picture},
    overlay::Overlay,
    snapshot::Placement,
    theme,
    views::View,
};

const SIDEBAR_WIDTH: u16 = 32;
/// Below this width only the focused pane is shown.
const TWO_PANE_MIN_WIDTH: u16 = 90;
const PLAYER_HEIGHT: u16 = 6;
/// The character Kitty image cells are made of.
const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

pub fn draw(frame: &mut Frame, app: &mut App) {
    let p = theme::palette();
    app.begin_frame();
    let area = frame.area();
    app.screen = area;
    app.hits.clear();
    app.placements.clear();
    frame.buffer_mut().set_style(area, Style::new().bg(p.bg).fg(p.text));
    // Small windows get compact bars so there is room left for the content.
    let top_height = match area.height {
        30.. => topbar::HEIGHT,
        14.. => 1,
        _ => 0,
    };
    let player_height = match area.height {
        24.. => PLAYER_HEIGHT,
        10.. => 2,
        _ => 1,
    };
    let hints_height = u16::from(area.height >= 16);
    let [top, main, player, hints] = Layout::vertical([
        Constraint::Length(top_height),
        Constraint::Min(0),
        Constraint::Length(player_height),
        Constraint::Length(hints_height),
    ])
    .areas(area);

    if app.zen {
        zen::draw(frame, app, top.union(main).union(player));
    } else {
        if !top.is_empty() {
            topbar::draw(frame.buffer_mut(), app, top);
        }
        if main.width >= TWO_PANE_MIN_WIDTH {
            let [nav, content] =
                Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(main);
            sidebar::draw(frame, app, nav);
            draw_content(frame, app, content);
            // One hairline between the panes, so they read as the two panels they are. It
            // stands a column off the sidebar, clear of its scrollbar.
            let divider = nav.right() + 1;
            for y in main.top()..main.bottom() {
                frame.buffer_mut().set_string(divider, y, "│", Style::new().fg(p.raised).bg(p.bg));
            }
        } else if app.focus == Focus::Nav {
            sidebar::draw(frame, app, main);
        } else {
            draw_content(frame, app, main);
        }
        player_bar::draw(frame, app, player);
    }
    if !hints.is_empty() {
        hints::draw(frame.buffer_mut(), app, hints);
    }

    match &app.overlay {
        Some(Overlay::Help) => help::draw(frame.buffer_mut(), app, area),
        Some(Overlay::Menu(_)) => menu::draw(frame.buffer_mut(), app, area),
        Some(Overlay::Star { .. }) => star::draw(frame.buffer_mut(), app, area),
        Some(_) => picker::draw(frame, app, area),
        None => {}
    }

    hover(frame.buffer_mut(), app);
    app.sync_hover();

    let elapsed = app.last_draw.elapsed();
    app.last_draw = Instant::now();
    app.effects.process(elapsed, frame.buffer_mut(), not_animated);
    app.inline.prepare(frame.buffer_mut(), &app.placements);
    app.end_frame();
}

fn draw_content(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    match app.view {
        View::Home(_) => home::draw(frame, app, area),
        View::Library(_) => library::draw(frame, app, area),
        View::Artist(_) => artist::draw(frame, app, area),
        View::Settings(_) => settings::draw(frame, app, area),
        _ => content::draw(frame, app, area),
    }
}

/// Draws a cover centered in `slot`, or nothing while it loads. Returns whether it drew. With
/// cover art off, a plain tile takes its place.
pub(super) fn cover(frame: &mut Frame, app: &mut App, url: Option<&str>, size: Size, round: bool, slot: Rect) -> bool {
    cover_in(frame, app, url, size, round, (slot.x, slot.y as i32), slot.as_size(), slot)
}

/// Like [`cover`], for a slot at `(x, y)` that may reach past `clip` while scrolling: the
/// visible part of the cover is drawn.
#[allow(clippy::too_many_arguments)]
pub(super) fn cover_in(
    frame: &mut Frame,
    app: &mut App,
    url: Option<&str>,
    size: Size,
    round: bool,
    (x, y): (u16, i32),
    slot: Size,
    clip: Rect,
) -> bool {
    // Fill the slot with the pane background first, so transparent image corners (round
    // covers) and any smaller image never show the terminal's own (possibly light)
    // background or a stale raised tile behind the art.
    {
        let buf = frame.buffer_mut();
        let rect = Rect::new(x, y.max(0) as u16, slot.width, slot.height);
        let area = rect.intersection(buf.area);
        if !area.is_empty() {
            buf.set_style(area, Style::new().bg(theme::palette().bg));
        }
    }
    if !app.prefs.covers {
        let size = Size::new(size.width.min(slot.width), size.height.min(slot.height));
        tile(frame.buffer_mut(), (x, y), size, slot, clip, round);
        return true;
    }
    let placed = app.cover(url, size, round).and_then(|picture| picture_in(frame, picture, (x, y), slot, clip));
    match placed {
        Some(placement) => {
            app.placements.push(placement);
            true
        }
        None => false,
    }
}

/// Stands in for a cover with cover art off: one flat color and an icon, which costs nothing to
/// load or to send to the terminal.
fn tile(buf: &mut Buffer, (x, y): (u16, i32), size: Size, slot: Size, clip: Rect, round: bool) {
    let p = theme::palette();
    let left = x + slot.width.saturating_sub(size.width) / 2;
    let top = y + (slot.height.saturating_sub(size.height) / 2) as i32;
    let visible_top = top.max(clip.y as i32);
    let visible_bottom = (top + size.height as i32).min(clip.bottom() as i32);
    if visible_top >= visible_bottom {
        return;
    }
    let area = Rect::new(left, visible_top as u16, size.width, (visible_bottom - visible_top) as u16);
    let area = area.intersection(buf.area);
    buf.set_style(area, Style::new().bg(p.raised));
    let icon = if round { icons().artist } else { icons().note };
    let middle = top + (size.height / 2) as i32;
    if area.width >= 2 && (visible_top..visible_bottom).contains(&middle) {
        buf.set_string(left + (size.width - 1) / 2, middle as u16, icon, Style::new().fg(p.faint));
    }
}

/// Stands in for a cover nobody has: a gradient picked by the name, with its first letter. A
/// shelf of these reads as a designed placeholder rather than as a failed load.
pub(super) fn placeholder(buf: &mut Buffer, area: Rect, name: &str, round: bool) {
    let area = area.intersection(buf.area);
    if area.is_empty() || area.width == 0 {
        return;
    }
    let (from, to) = theme::placeholder_colors(name);
    let last = area.height.saturating_sub(1).max(1) as f32;
    for (row, y) in (area.top()..area.bottom()).enumerate() {
        let shade = from.mix(to, row as f32 / last);
        let mut band = Rect::new(area.x, y, area.width, 1);
        // Covers are rounded and portraits are circles, so clear the corner cells: a drawn
        // placeholder then has the same silhouette as a masked image.
        let height = area.height as usize;
        let depth = if round { 2 } else { 1 };
        let edge = (row < depth || row + depth >= height) && height > 2 * depth;
        if edge && band.width > 4 {
            let inset = if round { 2 } else { 1 };
            band = Rect::new(band.x + inset, y, band.width - 2 * inset, 1);
        }
        buf.set_style(band, Style::new().bg(shade.into()));
    }
    let initial = name.chars().find(|c| !c.is_whitespace()).map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    if initial.is_empty() || area.width < initial.width() as u16 {
        return;
    }
    let x = area.x + (area.width - initial.width() as u16) / 2;
    let y = area.y + area.height / 2;
    let ink = from.mix(theme::Rgb(255, 255, 255), 0.85);
    buf.set_string(x, y, initial, Style::new().fg(ink.into()).bold());
}

/// Draws an image centered in `slot`; returns where it went, for dev snapshots.
pub(super) fn picture_at(frame: &mut Frame, picture: &Picture, slot: Rect) -> Option<Placement> {
    picture_in(frame, picture, (slot.x, slot.y as i32), slot.as_size(), slot)
}

/// Draws the part of an image centered in a slot at `(x, y)` that lies inside `clip`.
/// Returns `None` when nothing was drawn.
pub(super) fn picture_in(
    frame: &mut Frame,
    picture: &Picture,
    (x, y): (u16, i32),
    slot: Size,
    clip: Rect,
) -> Option<Placement> {
    let size = picture.size();
    let size = Size::new(size.width.min(slot.width), size.height.min(slot.height));
    let left = x + slot.width.saturating_sub(size.width) / 2;
    let top = y + (slot.height.saturating_sub(size.height) / 2) as i32;
    let mut bottom = clip.bottom();
    if let Graphic::Inline(_) = picture.graphic {
        // Some terminals put the cursor below a Sixel or iTerm2 image, which in the last row
        // scrolls the whole screen.
        bottom = bottom.min(frame.area().bottom().saturating_sub(1));
    }
    let visible_top = top.max(clip.y as i32);
    let visible_bottom = (top + size.height as i32).min(bottom as i32);
    if visible_top >= visible_bottom || left + size.width > frame.area().right() {
        return None;
    }
    let skip = (visible_top - top) as u16;
    let area = Rect::new(left, visible_top as u16, size.width, (visible_bottom - visible_top) as u16);
    let (upload, inline) = match &picture.graphic {
        // Written after the frame, cut to the rows in view; the cells only mark where it goes.
        Graphic::Inline(image) => {
            let buf = frame.buffer_mut();
            for position in area.positions() {
                let cell = &mut buf[position];
                let bg = cell.bg;
                cell.reset();
                cell.set_symbol(INLINE_CELL).set_bg(bg).set_diff_option(CellDiffOption::Skip);
            }
            (None, Some(image.clone()))
        }
        Graphic::Cells(protocol) if area.height == size.height => {
            frame.render_widget(Image::new(protocol), area);
            (take_upload(&mut frame.buffer_mut()[(area.x, area.y)]), None)
        }
        // Kitty placeholders and half blocks are plain cells, so rows can be cut.
        Graphic::Cells(protocol) => {
            let mut scratch = Buffer::empty(Rect::new(0, 0, size.width, size.height));
            Image::new(protocol).render(scratch.area, &mut scratch);
            let upload = take_upload(&mut scratch[(0, 0)]);
            let buf = frame.buffer_mut();
            for row in 0..area.height {
                for column in 0..area.width {
                    buf[(area.x + column, area.y + row)] = scratch[(column, skip + row)].clone();
                }
            }
            (upload, None)
        }
    };
    let cells = match picture.source {
        Some(_) => area.rows().flat_map(|row| row.columns()).map(|p| frame.buffer_mut()[p].clone()).collect(),
        None => Vec::new(),
    };
    Some(Placement { area, size, skip, source: picture.source.clone(), cells, upload, inline })
}

/// The first time a Kitty image is drawn, the upload of its pixels rides on its top-left cell.
/// Takes it off, so that whatever is drawn over that cell, like an overlay, cannot lose it; the
/// caller writes it out after the frame.
fn take_upload(cell: &mut Cell) -> Option<String> {
    let symbol = cell.symbol();
    let start = symbol.find(KITTY_PLACEHOLDER).filter(|&start| start > 0)?;
    let (upload, placeholder) = (symbol[..start].to_owned(), symbol[start..].to_owned());
    cell.set_symbol(&placeholder);
    Some(upload)
}

pub(super) const PLAY_BUTTON_WIDTH: u16 = 3;

/// The play button of a hovered card, ending at column `right` in row `y`. On a cover, a dark
/// cell to its left sets it off from covers in the accent color.
pub(super) fn play_button(buf: &mut Buffer, app: &mut App, right: u16, y: u16, hit: Hit, on_cover: bool) {
    let p = theme::palette();
    let icons = icons();
    let area = Rect::new(right.saturating_sub(PLAY_BUTTON_WIDTH), y, PLAY_BUTTON_WIDTH, 1).intersection(buf.area);
    let accent = app.accent.color();
    if on_cover && let Some(edge) = area.x.checked_sub(1) {
        buf.set_string(edge, y, " ", Style::new().bg(p.bg));
    }
    // Blank the cells rather than only recoloring them: over a cover they hold image cells,
    // which would keep showing pieces of the picture around the icon.
    buf.set_string(area.x, y, " ".repeat(area.width as usize), Style::new().bg(accent));
    buf.set_string(area.x + 1, y, icons.play, Style::new().fg(p.bg).bg(accent));
    app.hits.push((area, hit));
}

/// Where each of `names`, written out joined by ", " from column `x`, lies on screen, cut
/// off at `width` columns: one rect per visible name, with its index.
pub(super) fn name_spans(x: u16, y: u16, names: &[String], width: usize) -> Vec<(Rect, usize)> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for (index, name) in names.iter().enumerate() {
        if offset >= width {
            break;
        }
        let name_width = name.width().min(width - offset);
        spans.push((Rect::new(x + offset as u16, y, name_width as u16, 1), index));
        offset += name.width() + 2;
    }
    spans
}

/// The equalizer that marks what plays; it stands still where motion is unwelcome.
pub(super) fn equalizer(app: &App) -> &'static str {
    text::equalizer(if app.calm() { 0 } else { app.frame() })
}

/// Whether the mouse is over `area` with nothing in front of it.
pub(super) fn hovered(app: &App, area: Rect) -> bool {
    app.overlay.is_none() && app.mouse.is_some_and(|position| area.contains(position))
}

/// A slim scrollbar in column `x`, shown only when the content is longer than the view.
pub(super) fn scrollbar(buf: &mut Buffer, x: u16, y: u16, height: u16, total: usize, offset: usize, visible: usize) {
    let p = theme::palette();
    if total <= visible || height < 3 || x >= buf.area.right() {
        return;
    }
    let rows = height as usize;
    let thumb = (rows * visible / total).clamp(1, rows);
    let max_offset = total - visible;
    let start = (rows - thumb) * offset.min(max_offset) / max_offset;
    for row in 0..rows {
        let (symbol, color) = if (start..start + thumb).contains(&row) { ("┃", p.thumb) } else { ("│", p.raised) };
        buf.set_string(x, y + row as u16, symbol, Style::new().fg(color));
    }
}

/// Lightens whatever clickable thing the mouse is over, and more so what was just clicked;
/// links get underlined instead.
fn hover(buf: &mut Buffer, app: &App) {
    let p = theme::palette();
    // Only while the target is still there: a click that opened another page leaves none.
    let pressed = app.pressed().map(|(hit, area)| (area, hit)).filter(|pressed| app.hits.contains(pressed));
    if let Some((area, _)) = pressed {
        tint(buf, app, area, 0.18);
    }
    let Some(position) = app.mouse else { return };
    // Only the topmost target counts: an overlay's body hides the rows and cards under it.
    let Some((area, hit)) = app.hits.iter().rev().find(|(area, _)| area.contains(position)) else { return };
    if !hit.is_interactive() {
        return;
    }
    if let Hit::Link(_) = hit {
        buf.set_style(area.intersection(buf.area), Style::new().fg(p.text).add_modifier(Modifier::UNDERLINED));
    } else if pressed.is_none_or(|(pressed, _)| pressed != *area) {
        tint(buf, app, *area, 0.06);
    }
}

/// Mixes some of the highlight color, white on dark themes, into the background of `area`.
fn tint(buf: &mut Buffer, app: &App, area: Rect, amount: f32) {
    let p = theme::palette();
    let area = area.intersection(buf.area);
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            // Leave image cells alone: their colors carry the picture.
            if cell.symbol().contains(KITTY_PLACEHOLDER)
                || app.placements.iter().any(|p| p.area.contains((x, y).into()))
            {
                continue;
            }
            let base = match cell.bg {
                Color::Rgb(r, g, b) => crate::theme::Rgb(r, g, b),
                _ => crate::theme::Rgb::of(p.bg),
            };
            cell.bg = base.mix(p.highlight, amount).into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_cover_gets_a_gradient_and_its_first_letter() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        let area = buf.area;
        placeholder(&mut buf, area, "morning run", false);
        assert_ne!(buf[(1, 0)].bg, buf[(1, 2)].bg, "the gradient runs top to bottom");
        assert_eq!(buf[(2, 1)].symbol(), "M", "the first letter, centered");
        // The same name always gets the same placeholder.
        let mut other = Buffer::empty(Rect::new(0, 0, 6, 3));
        placeholder(&mut other, area, "morning run", true);
        assert_eq!(buf[(4, 1)].bg, other[(4, 1)].bg);
    }
}

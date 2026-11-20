
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::{Rect, Size},
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::{player_bar, text};
use crate::{
    app::{App, Hit, Link},
    icons::icons,
    theme::{self, Rgb},
};

const WIDE_MIN_WIDTH: u16 = 100;
const WIDE_MIN_HEIGHT: u16 = 20;
const INFO_ROWS: u16 = 7;
const LYRIC_SPACING: f32 = 2.0;

pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    let p = theme::palette();
    let accent = app.accent.rgb();
    let background: Color = Rgb::of(p.bg).mix(accent, 0.09).into();
    frame.buffer_mut().set_style(area, Style::new().bg(background));
    app.zen_cover_want = None;

    let Some(track) = &app.now.track else {
        let message = "Nothing playing · z or esc to go back";
        let x = area.x + area.width.saturating_sub(message.width() as u16) / 2;
        frame.buffer_mut().set_string(x, area.y + area.height / 2, message, Style::new().fg(p.muted));
        return;
    };
    let title = track.name.clone();
    let by = [track.artists.join(", "), track.album.clone()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let duration_ms = track.duration_ms;
    let accent_color: Color = accent.into();

    let wide = app.prefs.covers && area.width >= WIDE_MIN_WIDTH && area.height >= WIDE_MIN_HEIGHT;
    let (info, lyrics_area) = if wide {
        let cover_height = (area.height - INFO_ROWS - 4).min(area.width / 5);
        let cover = Rect::new(
            area.x + 6,
            area.y + (area.height - cover_height - INFO_ROWS) / 2,
            cover_height * 2,
            cover_height,
        );
        draw_cover(frame, app, cover);
        let info = Rect::new(cover.x, cover.bottom() + 1, cover.width, INFO_ROWS - 1);
        let lyrics_x = cover.right() + 8;
        (info, Rect::new(lyrics_x, area.y + 1, area.right().saturating_sub(lyrics_x + 4), area.height - 2))
    } else {
        let info =
            Rect::new(area.x + 4, area.bottom().saturating_sub(INFO_ROWS), area.width.saturating_sub(8), INFO_ROWS - 1);
        (
            info,
            Rect::new(area.x + 4, area.y + 1, area.width.saturating_sub(8), area.height.saturating_sub(INFO_ROWS + 2)),
        )
    };

    let buf = frame.buffer_mut();
    let fits = |row: u16| info.y + row < area.bottom();
    if fits(1) {
        buf.set_string(info.x, info.y, "NOW PLAYING", Style::new().fg(p.faint).bold());
        let room = info.width.saturating_sub("NOW PLAYING".width() as u16 + 3) as usize;
        if let Some(label) = app.playing_context().and_then(|uri| app.context_label(uri))
            && room > 10
        {
            let label = text::truncate(&label, room);
            let width = label.width() as u16;
            let x = info.right().saturating_sub(width);
            buf.set_string(x, info.y, &label, Style::new().fg(p.faint));
            app.hits.push((Rect::new(x, info.y, width, 1), Hit::Link(Link::PlayingContext)));
        }
    }
    if fits(2) {
        buf.set_string(info.x, info.y + 1, text::truncate(&title, info.width as usize), Style::new().fg(p.text).bold());
        buf.set_string(info.x, info.y + 2, text::truncate(&by, info.width as usize), Style::new().fg(p.muted));
    }
    if fits(4) {
        let bar = Rect { y: info.y + 4, height: 1, ..info };
        let position = app.seek_preview.unwrap_or(app.now.position_ms());
        app.hits.extend(player_bar::progress(buf, bar, position, duration_ms, accent_color));
    }
    if fits(6) {
        let (controls, targets) = player_bar::controls(app, accent_color);
        let controls_width = controls.width() as u16;
        let controls_x = info.x + info.width.saturating_sub(controls_width) / 2;
        buf.set_line(controls_x, info.y + 6, &controls, controls_width);
        app.hits.extend(targets.into_iter().map(|(offset, width, command)| {
            (Rect::new(controls_x + offset, info.y + 6, width, 1), Hit::Command(command))
        }));
    }

    draw_lyrics(buf, app, lyrics_area, accent_color);
}

fn draw_cover(frame: &mut Frame, app: &mut App, area: Rect) {
    let name = app.now.track.as_ref().map(|t| t.name.clone()).unwrap_or_default();
    app.zen_cover_want = Some(Size::new(area.width, area.height));
    let url = app.now.track.as_ref().and_then(|t| t.cover_url_large.as_deref());
    let placed = match &app.zen_cover {
        Some(cover) if Some(cover.url.as_str()) == url => Some(super::picture_at(frame, &cover.picture, area)),
        _ => None,
    };
    match placed {
        Some(placement) => app.placements.extend(placement),
        None => {
            super::placeholder(frame, app, area, &name, false);
        }
    }
    app.hits.push((area, Hit::Cover));
}

fn draw_lyrics(buf: &mut Buffer, app: &App, area: Rect, accent: Color) {
    let p = theme::palette();
    if area.width < 10 || area.height < 3 {
        return;
    }
    let center = area.y + area.height / 2;
    let message = |buf: &mut Buffer, text: String| {
        buf.set_string(area.x, center, text, Style::new().fg(p.muted));
    };
    if area.height >= 6 {
        buf.set_string(area.x, area.y, "LYRICS", Style::new().fg(p.faint).bold());
    }
    let Some(state) = &app.lyrics else { return };
    let lyrics = match &state.result {
        None => return message(buf, format!("{}  Loading lyrics…", text::spinner(app.frame()))),
        Some(Err(_)) => return message(buf, "Lyrics are unavailable right now.".into()),
        Some(Ok(None)) => return message(buf, "No lyrics for this song.".into()),
        Some(Ok(Some(lyrics))) => lyrics,
    };

    let width = area.width as usize;
    let current = lyrics.current_line(app.now.position_ms());
    let top = area.y + if area.height >= 6 { 2 } else { 0 };
    let bottom = area.bottom().saturating_sub(1);
    let (anchor, scroll) = if lyrics.synced { (center as f32, app.lyrics_scroll) } else { (top as f32, 0.0) };
    for (index, line) in lyrics.lines.iter().enumerate() {
        let y = (anchor + (index as f32 - scroll) * LYRIC_SPACING).round();
        if y < top as f32 || y >= bottom as f32 {
            continue;
        }
        let words = if line.text.trim().is_empty() { icons().note } else { line.text.as_str() };
        let style = match current {
            Some(c) if c == index => Style::new().fg(accent).bold(),
            Some(c) => {
                let distance = (index as f32 - c as f32).abs();
                let fade = (distance / 6.0).min(1.0);
                let color: Color = Rgb::of(p.lyric_near).mix(Rgb::of(p.lyric_far), fade).into();
                Style::new().fg(color)
            }
            None => Style::new().fg(p.muted),
        };
        let prefix = if current == Some(index) { "▸ " } else { "  " };
        let line =
            Line::from(vec![Span::styled(prefix, accent), Span::styled(text::truncate(words, width - 2), style)]);
        buf.set_line(area.x, y as u16, &line, area.width);
    }

    let source = format!("Lyrics · {}", lyrics.provider);
    let x = area.right().saturating_sub(source.width() as u16);
    buf.set_string(x, area.bottom().saturating_sub(1), source, Style::new().fg(p.faint));
}

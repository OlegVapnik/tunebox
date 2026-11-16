//! The top bar: wordmark, home button and the search field, like the official client, the
//! switch between music services when there are several, and an update button while there is
//! a newer tunebox.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::{text, wordmark};
use crate::{
    app::{App, Focus, HintAction, Hit, NavItem, Update},
    backend::Service,
    commands::Command,
    icons::{self, icons},
    theme,
    views::View,
};

pub const HEIGHT: u16 = 4;
/// A square-ish button with its icon in the middle cell.
const BUTTON_WIDTH: u16 = 5;
/// A search field narrower than this is no longer useful; the right-hand controls give way first.
const MIN_SEARCH: u16 = 10;

#[derive(Clone, Copy)]
enum ServiceLabels {
    /// Active service with its name, the others with their icon or initials.
    Mixed,
    /// Icons or initials for every service.
    Icons,
}

pub fn draw(buf: &mut Buffer, app: &mut App, area: Rect) {
    let p = theme::palette();
    buf.set_style(area, Style::new().bg(p.bg));
    if area.height < HEIGHT {
        return compact(buf, app, area);
    }
    let wide = area.width >= 100;
    if wide {
        wordmark::draw(buf, area.x + 3, area.y + 1, app.accent.rgb());
    }

    // Back and home buttons and the search field, centered in the window like Spotify's.
    let search_width = (area.width / 2).clamp(28, 48);
    let cluster = 2 * (BUTTON_WIDTH + 1) + search_width;
    let left_limit = if wide { area.x + 3 + wordmark::WIDTH + 3 } else { area.x + 2 };
    let x = (area.x + area.width.saturating_sub(cluster) / 2).max(left_limit);
    let row = area.y + 2;

    let back = button(buf, app, x, row, 2, icons().chevron_left, Hit::Hint(HintAction::Back), app.can_go_back());
    let home = button(buf, app, back.right() + 1, row, 2, icons().home, Hit::Command(Command::Home), true);
    let search_x = home.right() + 1;
    // Reserve room for the right-hand controls before sizing the search field, otherwise the
    // service switch is drawn over the search prompt as the window shrinks.
    let switch_min = switch_width(app, 2, ServiceLabels::Icons);
    let mut update_min = update_width(app, 2);
    let mut width = search_width
        .min(area.right().saturating_sub(search_x + 2))
        .min(area.right().saturating_sub(3 + update_min + switch_min + 2).saturating_sub(search_x));
    if width < MIN_SEARCH {
        update_min = 0;
        width = search_width.min(area.right().saturating_sub(search_x + 2));
    }
    let search = Rect::new(search_x, row, width, 1);
    search_field(buf, app, search, 2);

    // Right of the search field: the update button, the service switch, then the user's name.
    let mut right = area.right().saturating_sub(3);
    if let Some(button) = update_button(buf, app, right, row, 2, update_min) {
        right = button.x.saturating_sub(2);
    }
    if let Some(switch) = service_switch(buf, app, right, row, 2, right.saturating_sub(search.right() + 2)) {
        right = switch.x.saturating_sub(2);
    }
    if let Some(me) = &app.me
        && wide
    {
        let mut name = format!("{}  {}", icons().artist, me.name);
        if app.shown_service() == Some(Service::Spotify)
            && me.product.as_deref() == Some("free")
        {
            name.push_str("  · Free");
        }
        let width = name.width() as u16;
        if right.saturating_sub(width) > search.right() + 2 {
            buf.set_string(right - width, row, name, Style::new().fg(p.muted));
            // Like the profile menu in Spotify's apps, the name leads to the settings.
            app.hits.push((Rect::new(right - width, row, width, 1), Hit::Command(Command::Settings)));
        }
    }
}

/// One row with slimmer buttons, for small windows.
fn compact(buf: &mut Buffer, app: &mut App, area: Rect) {
    let back =
        button(buf, app, area.x + 1, area.y, 1, icons().chevron_left, Hit::Hint(HintAction::Back), app.can_go_back());
    let home = button(buf, app, back.right() + 1, area.y, 1, icons().home, Hit::Command(Command::Home), true);
    let end = area.right().saturating_sub(1);
    let search_x = home.right() + 1;
    let switch_min = switch_width(app, 1, ServiceLabels::Icons);
    let mut update_min = update_width(app, 1);
    let mut search_end = end.saturating_sub(update_min + switch_min + 2);
    if search_end < search_x + MIN_SEARCH {
        update_min = 0;
        search_end = end.saturating_sub(2);
    }
    let right = update_button(buf, app, end, area.y, 1, update_min).map_or(end, |button| button.x.saturating_sub(1));
    service_switch(buf, app, right, area.y, 1, right.saturating_sub(search_end + 1));
    let search = Rect::new(search_x, area.y, search_end.saturating_sub(search_x), 1);
    search_field(buf, app, search, 1);
}

/// The smallest possible update button: just its icon and the padding around it.
fn update_width(app: &App, padding: u16) -> u16 {
    if app.update.is_some() { icons().update.width() as u16 + 2 * padding } else { 0 }
}

/// The update button, its right edge at `right`, with as much of its label as fits into `room`
/// columns and at least its icon. Draws nothing without an update.
fn update_button(buf: &mut Buffer, app: &mut App, right: u16, y: u16, padding: u16, room: u16) -> Option<Rect> {
    let p = theme::palette();
    let (icon, labels) = match app.update.as_ref()? {
        Update::Available(release) => (icons().update, [format!("Update to v{}", release.version()), "Update".into()]),
        // Nothing to install here: the button says what's out, and a click how to update.
        Update::Managed { release, .. } => {
            (icons().update, [format!("v{} is out", release.version()), format!("v{}", release.version())])
        }
        Update::Installing { percent, .. } => {
            let percent = percent.map(|percent| format!("{percent}%")).unwrap_or_default();
            (text::spinner(app.frame()), [format!("Updating {percent}").trim_end().to_owned(), percent])
        }
        Update::Installed { release, .. } => {
            (icons().restart, [format!("Restart for v{}", release.version()), "Restart".into()])
        }
    };
    let content = labels
        .into_iter()
        .filter(|label| !label.is_empty())
        .map(|label| format!("{icon}  {label}"))
        .chain([icon.to_owned()])
        .find(|content| content.width() as u16 + 2 * padding <= room)?;
    let width = content.width() as u16 + 2 * padding;
    let area = Rect::new(right.saturating_sub(width), y, width, 1).intersection(buf.area);
    buf.set_style(area, Style::new().bg(p.raised));
    buf.set_string(area.x + padding, y, content, Style::new().fg(app.accent.color()).bold());
    app.hits.push((area, Hit::Command(Command::Update)));
    Some(area)
}

/// The label a service gets in the switch. The service that is open keeps its name, the others
/// show only their icon, so the active one is the one you can read at a glance.
fn service_label(app: &App, service: Service, labels: ServiceLabels) -> String {
    let icon = icons::service(service);
    let initials = match service {
        Service::Spotify => "S",
        Service::YouTubeMusic => "YT",
        Service::SoundCloud => "SC",
    };
    if matches!(labels, ServiceLabels::Icons) || Some(service) != app.shown_service() {
        return if icon.is_empty() { initials.to_owned() } else { icon.to_owned() };
    }
    let short = match service {
        Service::Spotify => "Spotify",
        Service::YouTubeMusic => "YouTube",
        Service::SoundCloud => "SoundCloud",
    };
    if icon.is_empty() { short.to_owned() } else { format!("{icon}  {short}") }
}

/// The width of the service switch with either full or icon-only labels, including the gaps.
fn switch_width(app: &App, padding: u16, labels: ServiceLabels) -> u16 {
    app.services()
        .iter()
        .map(|&service| service_label(app, service, labels).width() as u16 + 2 * padding)
        .sum::<u16>()
        .saturating_sub(1)
}

/// The music services in use side by side, the one shown lit, each a button that shows its
/// pages; its right edge at `right`. Uses names when they fit, else icons; draws nothing if even
/// the icons would spill into whatever is left of `room`.
fn service_switch(buf: &mut Buffer, app: &mut App, right: u16, y: u16, padding: u16, room: u16) -> Option<Rect> {
    let p = theme::palette();
    if app.services().len() < 2 {
        return None;
    }
    let labels = if switch_width(app, padding, ServiceLabels::Mixed) <= room {
        ServiceLabels::Mixed
    } else {
        ServiceLabels::Icons
    };
    if switch_width(app, padding, labels) > room {
        return None;
    }
    let mut x = right;
    let mut first = right;
    for &service in app.services().iter().rev() {
        let label = service_label(app, service, labels);
        let width = label.width() as u16 + 2 * padding;
        let area = Rect::new(x.saturating_sub(width), y, width, 1).intersection(buf.area);
        let style = if Some(service) == app.shown_service() {
            Style::new().fg(app.accent.color()).bold().underlined()
        } else if app.failed_to_start(service) {
            Style::new().fg(p.faint)
        } else {
            Style::new().fg(p.muted)
        };
        buf.set_style(area, Style::new().bg(p.raised));
        buf.set_string(area.x + padding, y, label, style);
        if Some(service) != app.shown_service() {
            app.hits.push((area, Hit::Service(service)));
        }
        first = area.x;
        x = area.x.saturating_sub(1);
    }
    Some(Rect::new(first, y, right.saturating_sub(first), 1))
}

/// A filled button with `icon` centered, `padding` cells either side; dimmed and inert unless
/// `enabled`. The home icon lights up while Home is open.
#[allow(clippy::too_many_arguments)]
fn button(buf: &mut Buffer, app: &mut App, x: u16, y: u16, padding: u16, icon: &str, hit: Hit, enabled: bool) -> Rect {
    let p = theme::palette();
    let area = Rect::new(x, y, 2 * padding + 1, 1).intersection(buf.area);
    buf.set_style(area, Style::new().bg(p.raised));
    let color = match hit {
        _ if !enabled => p.disabled,
        Hit::Command(Command::Home) if app.is_open(NavItem::Home) => app.accent.color(),
        _ => p.text,
    };
    buf.set_string(x + padding, y, icon, Style::new().fg(color));
    if enabled {
        app.hits.push((area, hit));
    }
    area
}

/// The search field: icon, then the query or a prompt, and a spinner while results load.
fn search_field(buf: &mut Buffer, app: &mut App, area: Rect, padding: u16) {
    let p = theme::palette();
    let accent = app.accent.color();
    let typing = app.focus == Focus::SearchInput && !app.zen;
    buf.set_style(area, Style::new().bg(if typing { app.accent.selection() } else { p.raised }));
    let (query, loading) = match &app.view {
        View::Search(v) => (v.query.clone(), v.loading),
        _ => (String::new(), false),
    };
    let icon_style = Style::new().fg(if typing { accent } else { p.muted });
    let mut spans = vec![Span::styled(format!("{}  ", icons().search), icon_style)];
    let room = area.width.saturating_sub(2 * padding + 6) as usize;
    if query.is_empty() && !typing {
        spans.push(Span::styled(text::truncate("What do you want to play?", room), p.faint));
    } else {
        spans.push(Span::styled(tail(&query, room), Style::new().fg(p.text).bold()));
        if typing {
            spans.push(Span::styled("▏", accent));
        }
    }
    buf.set_line(area.x + padding, area.y, &Line::from(spans), area.width.saturating_sub(2 * padding));
    if loading {
        buf.set_string(
            area.right().saturating_sub(padding + 1),
            area.y,
            text::spinner(app.frame()),
            Style::new().fg(p.faint),
        );
    }
    app.hits.push((area, Hit::SearchInput));
}

/// The end of `text` that fits into `width` columns, so the cursor stays visible.
fn tail(text: &str, width: usize) -> String {
    let mut out: Vec<char> = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = c.to_string().width();
        if used + w > width {
            break;
        }
        used += w;
        out.push(c);
    }
    out.into_iter().rev().collect()
}

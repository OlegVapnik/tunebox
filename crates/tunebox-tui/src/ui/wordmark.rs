
use ratatui::{buffer::Buffer, style::Style};

use crate::theme::Rgb;

pub const HEIGHT: u16 = 3;
pub const WIDTH: u16 = 7;

pub fn draw(buf: &mut Buffer, x: u16, y: u16, accent: Rgb) {
    let highlight = accent.mix(crate::theme::palette().highlight, 0.35);
    buf.set_string(x, y + 1, "tunebox", Style::new().fg(highlight.into()).bold());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_matches_the_text() {
        assert_eq!(WIDTH, "tunebox".len() as u16);
    }
}

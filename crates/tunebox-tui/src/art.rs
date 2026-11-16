//! Artwork tunebox draws itself.

use image::{DynamicImage, Rgba, RgbaImage};

use crate::theme;

/// Pseudo-URL for the Liked Songs cover; the loader generates it instead of downloading.
pub const LIKED_COVER: &str = "tunebox:liked";
/// Pseudo-URL for items without a cover, such as new playlists.
pub const PLACEHOLDER_COVER: &str = "tunebox:placeholder";

const SIZE: u32 = 256;
/// Supersampling per axis, for smooth heart edges.
const SAMPLES: u32 = 3;

/// Spotify's Liked Songs look: a violet-to-mint gradient with a white heart.
pub fn liked_cover() -> DynamicImage {
    let from = [69.0, 10.0, 245.0];
    let to = [196.0, 239.0, 217.0];
    let image = RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let t = (x + y) as f32 / (2 * SIZE) as f32;
        let gradient = [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * t);
        let coverage = heart_coverage(x, y);
        let [r, g, b] = gradient.map(|c| (c + (255.0 - c) * coverage) as u8);
        Rgba([r, g, b, 255])
    });
    image.into()
}

/// Fraction of the pixel inside the heart curve (x² + y² − 1)³ − x²y³ ≤ 0.
fn heart_coverage(x: u32, y: u32) -> f32 {
    let mut inside = 0;
    for sx in 0..SAMPLES {
        for sy in 0..SAMPLES {
            let px = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / SIZE as f32;
            let py = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / SIZE as f32;
            // About a third of the cover, slightly above the center.
            let hx = (px - 0.5) * 6.0;
            let hy = (0.47 - py) * 6.0;
            if (hx * hx + hy * hy - 1.0).powi(3) - hx * hx * hy.powi(3) <= 0.0 {
                inside += 1;
            }
        }
    }
    inside as f32 / (SAMPLES * SAMPLES) as f32
}

/// A smooth gradient picked by the item's name, with transparent rounded corners and a large
/// initial drawn from a tiny built-in bitmap font. Names outside that font keep the text initial
/// the view draws on top.
pub fn name_cover(name: &str) -> DynamicImage {
    let (from, to) = theme::placeholder_colors(name);
    let letter = name.chars().find(|c| !c.is_whitespace()).and_then(|c| glyph(c.to_ascii_uppercase()));
    let base = from.mix(to, 0.5);
    let ink = base.mix(theme::Rgb(255, 255, 255), 0.85);
    let image = RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let t = (y as f32 / SIZE as f32 * 0.85 + x as f32 / SIZE as f32 * 0.15).clamp(0.0, 1.0);
        let channel = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        let (r, g, b) = (channel(from.0, to.0), channel(from.1, to.1), channel(from.2, to.2));
        let coverage = letter_coverage(letter, x, y);
        let blend = |c: u8, ink: u8| (c as f32 + (ink as f32 - c as f32) * coverage) as u8;
        Rgba([blend(r, ink.0), blend(g, ink.1), blend(b, ink.2), rounded_coverage(x, y)])
    });
    image.into()
}

/// 3×5 pixel glyphs, one bit per cell from the left. Enough for an initial on a cover, and no
/// font dependency in the terminal binary.
fn glyph(letter: char) -> Option<[u8; 5]> {
    Some(match letter {
        'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        'J' => [0b001, 0b001, 0b001, 0b101, 0b010],
        'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'M' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'N' => [0b101, 0b111, 0b111, 0b111, 0b101],
        'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        'P' => [0b110, 0b101, 0b110, 0b100, 0b100],
        'Q' => [0b010, 0b101, 0b101, 0b111, 0b011],
        'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        'S' => [0b011, 0b100, 0b010, 0b001, 0b110],
        'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        'W' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b110, 0b001, 0b010, 0b100, 0b111],
        '3' => [0b110, 0b001, 0b010, 0b001, 0b110],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b110, 0b001, 0b110],
        '6' => [0b011, 0b100, 0b110, 0b101, 0b010],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b010, 0b101, 0b010, 0b101, 0b010],
        '9' => [0b010, 0b101, 0b011, 0b001, 0b110],
        // Cyrillic initials are common in the user's libraries; keep them in the image too.
        'А' => [0b010, 0b101, 0b111, 0b101, 0b101],
        'Б' => [0b110, 0b100, 0b110, 0b101, 0b110],
        'В' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'Г' => [0b111, 0b100, 0b100, 0b100, 0b100],
        'Д' => [0b010, 0b101, 0b101, 0b101, 0b111],
        'Е' | 'Ё' => [0b111, 0b100, 0b110, 0b100, 0b111],
        'Ж' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'З' => [0b111, 0b001, 0b111, 0b001, 0b111],
        'И' | 'Й' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'К' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'Л' => [0b100, 0b100, 0b100, 0b100, 0b111],
        'М' => [0b101, 0b111, 0b111, 0b101, 0b101],
        'Н' => [0b101, 0b101, 0b111, 0b101, 0b101],
        'О' => [0b010, 0b101, 0b101, 0b101, 0b010],
        'П' => [0b111, 0b101, 0b101, 0b101, 0b101],
        'Р' => [0b110, 0b101, 0b110, 0b100, 0b100],
        'С' => [0b011, 0b100, 0b100, 0b100, 0b011],
        'Т' => [0b111, 0b010, 0b010, 0b010, 0b010],
        'У' => [0b101, 0b101, 0b101, 0b011, 0b001],
        'Ф' => [0b010, 0b101, 0b111, 0b101, 0b010],
        'Х' => [0b101, 0b101, 0b010, 0b101, 0b101],
        'Ц' => [0b101, 0b101, 0b101, 0b011, 0b001],
        'Ч' => [0b110, 0b010, 0b010, 0b010, 0b010],
        'Ш' => [0b101, 0b101, 0b111, 0b111, 0b101],
        'Щ' => [0b101, 0b101, 0b111, 0b111, 0b001],
        'Ъ' => [0b110, 0b101, 0b110, 0b101, 0b110],
        'Ы' => [0b101, 0b101, 0b110, 0b101, 0b101],
        'Ь' => [0b110, 0b100, 0b110, 0b101, 0b110],
        'Э' => [0b011, 0b100, 0b100, 0b100, 0b011],
        'Ю' => [0b101, 0b111, 0b110, 0b101, 0b101],
        'Я' => [0b010, 0b101, 0b011, 0b101, 0b101],
        _ => return None,
    })
}

/// Whether the generated cover can draw this initial itself; otherwise the view keeps its text one.
pub fn has_glyph(letter: char) -> bool {
    glyph(letter).is_some()
}

fn letter_coverage(letter: Option<[u8; 5]>, x: u32, y: u32) -> f32 {
    let Some(rows) = letter else { return 0.0 };
    let cell = SIZE as f32 / 7.0;
    let left = (SIZE as f32 - 3.0 * cell) / 2.0;
    let top = (SIZE as f32 - 5.0 * cell) / 2.0;
    let mut inside = 0;
    for sx in 0..SAMPLES {
        for sy in 0..SAMPLES {
            let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
            let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
            let column = ((px - left) / cell) as i32;
            let row = ((py - top) / cell) as i32;
            if (0..3).contains(&column) && (0..5).contains(&row) && rows[row as usize] & (1 << (2 - column)) != 0 {
                inside += 1;
            }
        }
    }
    inside as f32 / (SAMPLES * SAMPLES) as f32
}

/// Antialiased coverage of a rounded square: points within `radius` of the inner rectangle.
fn rounded_coverage(x: u32, y: u32) -> u8 {
    let radius = SIZE as f32 * 0.16;
    let mut inside = 0;
    for sx in 0..SAMPLES {
        for sy in 0..SAMPLES {
            let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
            let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
            let nearest_x = px.clamp(radius, SIZE as f32 - radius);
            let nearest_y = py.clamp(radius, SIZE as f32 - radius);
            if (px - nearest_x).powi(2) + (py - nearest_y).powi(2) <= radius * radius {
                inside += 1;
            }
        }
    }
    (inside * 255 / (SAMPLES * SAMPLES)) as u8
}

/// Spotify's look for playlists without a cover: a dark square with a muted music note.
pub fn placeholder_cover() -> DynamicImage {
    let image = RgbaImage::from_fn(SIZE, SIZE, |x, y| {
        let t = (x + y) as f32 / (2 * SIZE) as f32;
        let background = 40.0 - 12.0 * t;
        let coverage = supersample(x, y, note);
        let c = (background + (130.0 - background) * coverage) as u8;
        Rgba([c, c, c.saturating_add(6), 255])
    });
    image.into()
}

/// An eighth note: an oval head, a stem and a short flag, in unit coordinates.
fn note(px: f32, py: f32) -> bool {
    let (hx, hy) = (px - 0.44, py - 0.64);
    let (rx, ry) = (hx * 0.94 + hy * 0.34, -hx * 0.34 + hy * 0.94);
    let head = (rx / 0.105).powi(2) + (ry / 0.075).powi(2) <= 1.0;
    let stem = (0.515..=0.545).contains(&px) && (0.30..=0.64).contains(&py);
    let flag = (0.53..=0.66).contains(&px) && {
        let curve = 0.30 + (px - 0.53) * 1.1;
        py >= curve && py <= curve + 0.05
    };
    head || stem || flag
}

fn supersample(x: u32, y: u32, shape: fn(f32, f32) -> bool) -> f32 {
    let mut inside = 0;
    for sx in 0..SAMPLES {
        for sy in 0..SAMPLES {
            let px = (x as f32 + (sx as f32 + 0.5) / SAMPLES as f32) / SIZE as f32;
            let py = (y as f32 + (sy as f32 + 0.5) / SAMPLES as f32) / SIZE as f32;
            if shape(px, py) {
                inside += 1;
            }
        }
    }
    inside as f32 / (SAMPLES * SAMPLES) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_latin_initial_is_drawn_into_the_cover() {
        let with_letter = name_cover("popular songs");
        let without_letter = name_cover("1popular songs");
        let a = with_letter.to_rgba8();
        let b = without_letter.to_rgba8();
        let different = (0..SIZE)
            .any(|y| (0..SIZE).any(|x| a.get_pixel(x, y).0 != b.get_pixel(x, y).0));
        assert!(different, "the two names must produce visibly different covers");
    }

    #[test]
    fn a_cyrillic_initial_is_drawn_into_the_cover() {
        assert!(has_glyph('Э'));
        let with_letter = name_cover("Эндорфин");
        let without_letter = name_cover("1ндорфин");
        let a = with_letter.to_rgba8();
        let b = without_letter.to_rgba8();
        let different = (0..SIZE).any(|y| (0..SIZE).any(|x| a.get_pixel(x, y).0 != b.get_pixel(x, y).0));
        assert!(different, "a Cyrillic initial must be visible in the generated art");
    }

    #[test]
    fn a_name_without_a_known_glyph_still_gets_a_gradient() {
        assert!(!has_glyph('日'));
        let image = name_cover("日本語");
        assert_eq!(image.width(), SIZE);
        assert_eq!(image.height(), SIZE);
    }
}

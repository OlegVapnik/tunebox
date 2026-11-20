use std::{
    sync::{PoisonError, RwLock},
    time::Duration,
};

use image::DynamicImage;
use ratatui::style::Color;

use crate::clock::Instant;

const ACCENT_FADE: Duration = Duration::from_millis(600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub bg: Color,
    pub surface: Color,
    pub player: Color,
    pub raised: Color,
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub danger: Color,
    pub unfocused: Color,
    pub disabled: Color,
    pub placeholder: Color,
    pub thumb: Color,
    pub lyric_near: Color,
    pub lyric_far: Color,
    pub highlight: Rgb,
    pub accent: Rgb,
}

impl Palette {
    pub const DEFAULT: Palette = Palette {
        bg: Color::Rgb(17, 17, 27),
        surface: Color::Rgb(24, 24, 37),
        player: Color::Rgb(24, 24, 37),
        raised: Color::Rgb(49, 50, 68),
        text: Color::Rgb(205, 214, 244),
        muted: Color::Rgb(166, 173, 200),
        faint: Color::Rgb(108, 112, 134),
        danger: Color::Rgb(243, 139, 168),
        unfocused: Color::Rgb(30, 30, 46),
        disabled: Color::Rgb(88, 91, 112),
        placeholder: Color::Rgb(49, 50, 68),
        thumb: Color::Rgb(88, 91, 112),
        lyric_near: Color::Rgb(186, 194, 222),
        lyric_far: Color::Rgb(88, 91, 112),
        highlight: Rgb(245, 224, 220),
        accent: Rgb(203, 166, 247),
    };

    pub fn is_light(&self) -> bool {
        Rgb::of(self.bg).is_light()
    }
}

static PALETTE: RwLock<Palette> = RwLock::new(Palette::DEFAULT);

pub fn palette() -> Palette {
    *PALETTE.read().unwrap_or_else(PoisonError::into_inner)
}

pub fn set_palette(palette: Palette) {
    *PALETTE.write().unwrap_or_else(PoisonError::into_inner) = palette;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(lerp(self.0, other.0), lerp(self.1, other.1), lerp(self.2, other.2))
    }

    pub fn is_light(self) -> bool {
        u16::from(self.0) + u16::from(self.1) + u16::from(self.2) > 382
    }

    pub fn of(color: Color) -> Rgb {
        match color {
            Color::Rgb(r, g, b) => Rgb(r, g, b),
            _ => Rgb(0, 0, 0),
        }
    }
}

impl From<Rgb> for Color {
    fn from(c: Rgb) -> Self {
        Color::Rgb(c.0, c.1, c.2)
    }
}

pub struct Accent {
    from: Rgb,
    to: Rgb,
    since: Instant,
}

impl Accent {
    pub fn new() -> Self {
        let accent = palette().accent;
        Self { from: accent, to: accent, since: Instant::now() - ACCENT_FADE }
    }

    pub fn set(&mut self, target: Rgb) {
        self.from = self.rgb();
        self.to = target;
        self.since = Instant::now();
    }

    pub fn jump(&mut self, target: Rgb) {
        self.from = target;
        self.to = target;
        self.since = Instant::now() - ACCENT_FADE;
    }

    pub fn rgb(&self) -> Rgb {
        let t = (self.since.elapsed().as_secs_f32() / ACCENT_FADE.as_secs_f32()).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        self.from.mix(self.to, eased)
    }

    pub fn color(&self) -> Color {
        self.rgb().into()
    }

    pub fn selection(&self) -> Color {
        Rgb::of(palette().bg).mix(Rgb::of(palette().raised), 0.25).into()
    }

    pub fn is_fading(&self) -> bool {
        self.since.elapsed() < ACCENT_FADE
    }
}

pub fn placeholder_colors(name: &str) -> (Rgb, Rgb) {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let hue = ((hash ^ (hash >> 32)) % 360) as f32;
    let (r, g, b) = hsl_to_rgb(hue, 0.38, 0.34);
    let (r2, g2, b2) = hsl_to_rgb((hue + 24.0) % 360.0, 0.42, 0.20);
    (
        Rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8),
        Rgb((r2 * 255.0) as u8, (g2 * 255.0) as u8, (b2 * 255.0) as u8),
    )
}

pub fn dominant_color(image: &DynamicImage) -> Option<Rgb> {
    const BUCKETS: usize = 24;
    let mut buckets = [[0f32; 4]; BUCKETS];
    for pixel in image.thumbnail(40, 40).to_rgb8().pixels() {
        let [r, g, b] = pixel.0.map(|c| c as f32 / 255.0);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        if max < 0.15 {
            continue;
        }
        let saturation = (max - min) / max;
        let weight = saturation * saturation * max;
        let (hue, _, _) = rgb_to_hsl(r, g, b);
        let bucket = &mut buckets[((hue / 360.0) * BUCKETS as f32) as usize % BUCKETS];
        bucket[0] += r * weight;
        bucket[1] += g * weight;
        bucket[2] += b * weight;
        bucket[3] += weight;
    }
    let [r, g, b, weight] = buckets.into_iter().max_by(|a, b| a[3].total_cmp(&b[3])).unwrap_or_default();
    if weight < 1.0 {
        return None;
    }
    let channel = |c: f32| (c / weight * 255.0).round() as u8;
    Some(Rgb(channel(r), channel(g), channel(b)))
}

pub fn cover_accent(color: Rgb) -> Rgb {
    tune(color, palette().is_light())
}

fn tune(color: Rgb, light_background: bool) -> Rgb {
    let Rgb(r, g, b) = color;
    let (h, s, l) = rgb_to_hsl(f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let lightness = if light_background { LIGHT_LIGHTNESS } else { DARK_LIGHTNESS };
    let (r, g, b) = hsl_to_rgb(h, s.clamp(0.55, 0.85), l.clamp(lightness.0, lightness.1));
    Rgb((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

const DARK_LIGHTNESS: (f32, f32) = (0.58, 0.70);
const LIGHT_LIGHTNESS: (f32, f32) = (0.30, 0.42);

pub fn readable(color: Rgb) -> Rgb {
    if !palette().is_light() {
        return color;
    }
    let Rgb(r, g, b) = color;
    let (h, s, l) = rgb_to_hsl(f32::from(r) / 255.0, f32::from(g) / 255.0, f32::from(b) / 255.0);
    let (r, g, b) = hsl_to_rgb(h, s, l.min(LIGHT_LIGHTNESS.1));
    Rgb((r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8)
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0..60 => (c, x, 0.0),
        60..120 => (x, c, 0.0),
        120..180 => (0.0, c, x),
        180..240 => (0.0, x, c),
        240..300 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (r + m, g + m, b + m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jumping_skips_the_fade() {
        let mut accent = Accent::new();
        let target = Rgb(200, 40, 90);
        accent.set(target);
        assert!(accent.is_fading());
        accent.jump(target);
        assert!(!accent.is_fading());
        assert_eq!(accent.rgb(), target);
    }

    #[test]
    fn a_cover_s_color_is_tuned_to_the_background_it_is_shown_on() {
        let red = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([200, 20, 20])));
        let color = dominant_color(&red).expect("a vivid cover");
        let lightness = |Rgb(r, g, b): Rgb| (r.max(g).max(b) as f32 + r.min(g).min(b) as f32) / 2.0 / 255.0;
        let on_dark = tune(color, false);
        let on_light = tune(color, true);
        assert!((DARK_LIGHTNESS.0 - 0.01..=DARK_LIGHTNESS.1).contains(&lightness(on_dark)), "{on_dark:?}");
        assert!((LIGHT_LIGHTNESS.0 - 0.01..=LIGHT_LIGHTNESS.1).contains(&lightness(on_light)), "{on_light:?}");
        assert!(on_dark.0 > on_dark.1 && on_light.0 > on_light.1, "still red");

        let gray = DynamicImage::ImageRgb8(image::RgbImage::from_pixel(8, 8, image::Rgb([128, 128, 128])));
        assert_eq!(dominant_color(&gray), None, "the palette's accent then");
    }

    #[test]
    fn tells_light_themes_from_dark_ones() {
        assert!(!Palette::DEFAULT.is_light());
        assert!(Palette { bg: Color::Rgb(239, 241, 245), ..Palette::DEFAULT }.is_light());
    }
}

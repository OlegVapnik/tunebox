use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const EQUALIZER: [&str; 8] = ["⡄⡇⡆", "⡆⡄⡇", "⡇⡆⡄", "⡆⡇⡆", "⡄⡆⡇", "⡇⡄⡆", "⡆⡇⡄", "⡄⡆⡆"];

pub fn sanitize(text: &str) -> String {
    text.chars().filter(|c| c.width().unwrap_or(0) != 0 || c.is_whitespace()).collect()
}

pub fn fit(text: &str, width: usize) -> String {
    let mut out = truncate(text, width);
    let pad = width.saturating_sub(out.width());
    out.extend(std::iter::repeat_n(' ', pad));
    out
}

pub fn wrap_two(text: &str, width: usize) -> (String, String) {
    let text = sanitize(text);
    if text.width() <= width || width == 0 {
        return (text.to_owned(), String::new());
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut used, mut fit, mut space) = (0usize, 0usize, None);
    for (i, c) in chars.iter().enumerate() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            break;
        }
        used += w;
        fit = i + 1;
        if *c == ' ' {
            space = Some(i);
        }
    }
    let split = space.filter(|&s| s > 0).unwrap_or(fit);
    let first: String = chars[..split].iter().collect();
    let rest: String = chars[split..].iter().collect();
    (first.trim_end().to_owned(), truncate(rest.trim_start(), width))
}

pub fn truncate(text: &str, width: usize) -> String {
    let text = sanitize(text);
    if text.width() <= width {
        return text;
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    if width > 0 {
        out.push('…');
    }
    out
}

pub fn right(text: &str, width: usize) -> String {
    format!("{text:>width$}")
}

pub fn icon_label(icon: &str, label: &str) -> String {
    format!("{icon}  {label}")
}

pub fn duration(ms: u32) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub fn total_duration(ms: u64) -> String {
    let minutes = ms / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn spinner(frame: u64) -> &'static str {
    SPINNER[frame as usize % SPINNER.len()]
}

pub fn equalizer(frame: u64) -> &'static str {
    EQUALIZER[frame as usize % EQUALIZER.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_two_keeps_words_whole() {
        assert_eq!(wrap_two("popular English songs from tiktok", 14), ("popular".into(), "English songs…".into()));
        assert_eq!(wrap_two("Whit Girl Hardstyle (Hards…)", 14).0, "Whit Girl");
    }

    #[test]
    fn wrap_two_cuts_a_word_wider_than_the_line() {
        let (first, second) = wrap_two("Hardstyledubstep", 10);
        assert_eq!(first, "Hardstyled");
        assert_eq!(second, "ubstep");
    }

    #[test]
    fn wrap_two_leaves_a_short_title_on_one_line() {
        assert_eq!(wrap_two("Deep Focus", 14), ("Deep Focus".into(), String::new()));
    }
}

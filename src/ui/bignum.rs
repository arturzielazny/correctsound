//! Big numbers made of block characters, five rows tall, for a value meant to be read from
//! across the room (the placement score while carrying a bass trap). Plain text, so it
//! works in every terminal.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// Rows of each glyph, `#` lit; three pixels wide except the point.
fn glyph(c: char) -> Option<[&'static str; 5]> {
    Some(match c {
        '0' => ["###", "#.#", "#.#", "#.#", "###"],
        '1' => ["##.", ".#.", ".#.", ".#.", "###"],
        '2' => ["###", "..#", "###", "#..", "###"],
        '3' => ["###", "..#", "###", "..#", "###"],
        '4' => ["#.#", "#.#", "###", "..#", "..#"],
        '5' => ["###", "#..", "###", "..#", "###"],
        '6' => ["###", "#..", "###", "#.#", "###"],
        '7' => ["###", "..#", "..#", "..#", "..#"],
        '8' => ["###", "#.#", "###", "#.#", "###"],
        '9' => ["###", "#.#", "###", "..#", "###"],
        '+' => ["...", ".#.", "###", ".#.", "..."],
        '-' | '−' => ["...", "...", "###", "...", "..."],
        '±' => [".#.", "###", ".#.", "...", "###"],
        '.' => [".", ".", ".", ".", "#"],
        ' ' => ["..", "..", "..", "..", ".."],
        _ => return None,
    })
}

/// Terminal columns per pixel: cells are about twice as tall as wide, so two make it square.
const PIXEL_COLS: usize = 2;

/// `text` in big characters, as five lines; characters without a glyph are left out.
#[must_use]
pub fn lines(text: &str, style: Style) -> Vec<Line<'static>> {
    let glyphs: Vec<[&str; 5]> = text.chars().filter_map(glyph).collect();
    (0..5)
        .map(|row| {
            let mut s = String::new();
            for (i, g) in glyphs.iter().enumerate() {
                if i > 0 {
                    s.push_str(&" ".repeat(PIXEL_COLS));
                }
                for px in g[row].chars() {
                    let cell = if px == '#' { "█" } else { " " };
                    s.push_str(&cell.repeat(PIXEL_COLS));
                }
            }
            Line::from(Span::styled(s, style))
        })
        .collect()
}

/// Columns `text` takes in big characters.
#[must_use]
pub fn width(text: &str) -> usize {
    let glyphs: Vec<[&str; 5]> = text.chars().filter_map(glyph).collect();
    let pixels: usize = glyphs.iter().map(|g| g[0].len()).sum();
    (pixels + glyphs.len().saturating_sub(1)) * PIXEL_COLS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_a_signed_decimal() {
        let l = lines("+1.8", Style::default());
        assert_eq!(l.len(), 5);
        let row = |i: usize| l[i].spans[0].content.to_string();
        assert_eq!(row(2), "██████    ██        ██████");
        assert_eq!(row(4), "        ██████  ██  ██████");
        assert_eq!(width("+1.8"), row(2).chars().count());
    }
}

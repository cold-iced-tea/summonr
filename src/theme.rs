//! Visual identity: a service book for an old machine that still works.
//! Colour and ornament are optional; every glyph has an ASCII exit and the
//! literal state is always shown beside the ritual word.

use ratatui::style::Color;
use std::io::IsTerminal;

#[derive(Clone, Copy, PartialEq)]
pub enum Ink {
    Bone,
    Rule,
    Dim,
    Rubric,
    Ember,
    Gold,
    Verdigris,
    Phosphor,
}

impl Ink {
    fn rgb(self) -> (u8, u8, u8) {
        match self {
            Ink::Rule => (0x47, 0x50, 0x50),
            Ink::Bone => (0xE8, 0xDE, 0xCB),
            Ink::Dim => (0xAB, 0xAD, 0xA4),
            Ink::Rubric => (0xF0, 0x78, 0x78),
            Ink::Ember => (0xF2, 0xA6, 0x5A),
            Ink::Gold => (0xC5, 0xA4, 0x67),
            Ink::Verdigris => (0x78, 0xB6, 0xA3),
            Ink::Phosphor => (0xA9, 0xC9, 0x7D),
        }
    }

    pub fn color(self) -> Color {
        if plain() {
            return Color::Reset;
        }
        let (r, g, b) = self.rgb();
        Color::Rgb(r, g, b)
    }
}

/// `NO_COLOR` or `SUMMONR_PLAIN` removes colour and ornament everywhere.
pub fn plain() -> bool {
    std::env::var_os("NO_COLOR").is_some() || std::env::var_os("SUMMONR_PLAIN").is_some()
}

pub fn ascii() -> bool {
    plain() || std::env::var_os("SUMMONR_ASCII").is_some()
}

pub fn tty() -> bool {
    std::io::stdout().is_terminal()
}

/// Colour a string for terminal output, or return it untouched when stdout is
/// not a terminal.
pub fn paint(ink: Ink, s: &str) -> String {
    if plain() || !tty() {
        return s.to_string();
    }
    let (r, g, b) = ink.rgb();
    format!("\x1b[38;2;{r};{g};{b}m{s}\x1b[0m")
}

pub fn bold(ink: Ink, s: &str) -> String {
    if plain() || !tty() {
        return s.to_string();
    }
    let (r, g, b) = ink.rgb();
    format!("\x1b[1;38;2;{r};{g};{b}m{s}\x1b[0m")
}

/// The ritual word for a state, and the ink it is written in. The literal
/// status is shown next to it wherever there is room.
pub fn state_word(status: &str, outcome: Option<&str>) -> (&'static str, Ink) {
    use crate::outcome::Outcome;
    if let Some(o) = outcome.and_then(|o| o.parse::<Outcome>().ok()) {
        return match o {
            Outcome::Returned => ("AT REST", Ink::Verdigris),
            Outcome::Abstained => ("KEPT SILENCE", Ink::Gold),
            Outcome::Invalid => ("SPOKE AMISS", Ink::Rubric),
            Outcome::Failed => ("FELL", Ink::Rubric),
            Outcome::Exhausted => ("SPENT", Ink::Ember),
            Outcome::Cancelled => ("BANISHED", Ink::Dim),
            Outcome::Interrupted => ("LOST", Ink::Rubric),
            Outcome::Unverified => ("UNVERIFIED", Ink::Ember),
        };
    }
    if outcome.is_some() {
        ("UNKNOWN", Ink::Rubric)
    } else if status == "running" {
        ("LABOURS", Ink::Ember)
    } else {
        ("IS CALLED", Ink::Gold)
    }
}

const SIGILS: &[&str] = &[
    "☉", "☽", "☿", "♀", "♁", "♂", "♃", "♄", "△", "▽", "◇", "◈", "✶", "❖", "⌖", "⟁",
];
const SIGILS_ASCII: &[&str] = &[
    "o", "c", "v", "x", "+", "^", "z", "h", "A", "V", "<", ">", "*", "#", "=", "%",
];

/// A three-mark sigil derived from a summon's ID, so each summon is
/// recognizable at a glance. It carries no meaning beyond identity.
pub fn sigil(id: &str) -> String {
    let set = if ascii() { SIGILS_ASCII } else { SIGILS };
    let mut h: u32 = 2166136261;
    for b in id.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    (0..3)
        .map(|i| set[((h >> (i * 7)) & 15) as usize])
        .collect::<Vec<_>>()
        .join("")
}

/// An M9 seal: the first 27 bits of a hex hash drawn as three nine-bit
/// symbols, each a 3×3 grid read most-significant bit first, row by row (the
/// grid convention of the Marain experiment's M9 format). It is a recognition
/// aid shown beside the hex, never a substitute for comparing hashes.
/// `None` unless the text starts with seven hex digits.
pub fn seal(hex: &str) -> Option<[String; 3]> {
    seal_with(hex, ascii())
}

pub fn seal_with(hex: &str, ascii: bool) -> Option<[String; 3]> {
    let head = hex.get(..7)?;
    if !head.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let bits = u32::from_str_radix(head, 16).ok()? >> 1; // 28 bits -> 27
    let (on, off) = if ascii { ('#', '.') } else { ('█', '·') };
    let mut rows: [String; 3] = Default::default();
    for symbol in 0..3 {
        let value = (bits >> (18 - 9 * symbol)) & 0x1ff;
        for (r, row) in rows.iter_mut().enumerate() {
            if symbol > 0 {
                row.push(' ');
            }
            for c in 0..3 {
                let bit = 8 - (r * 3 + c);
                row.push(if value >> bit & 1 == 1 { on } else { off });
            }
        }
    }
    Some(rows)
}

/// A turning phase mark for work in progress.
pub fn phase(tick: u64) -> &'static str {
    if ascii() {
        ["-", "\\", "|", "/"][(tick % 4) as usize]
    } else {
        ["○", "◔", "◑", "◕", "●", "◕", "◑", "◔"][(tick % 8) as usize]
    }
}

pub fn ornament() -> &'static str {
    if ascii() {
        "*"
    } else {
        "❦"
    }
}

pub fn sun() -> &'static str {
    if ascii() {
        "(o)"
    } else {
        "☉"
    }
}

/// A ruled gauge: filled and empty marks for `used` of `max`.
pub fn gauge(used: u64, max: u64, width: usize) -> (String, String) {
    let filled = if max == 0 {
        0
    } else {
        ((used.min(max) as f64 / max as f64) * width as f64).round() as usize
    };
    let (on, off) = if ascii() { ("#", ".") } else { ("▰", "▱") };
    (on.repeat(filled), off.repeat(width - filled))
}

/// One mark per kind of ledger event.
pub fn event_mark(kind: &str) -> (&'static str, Ink) {
    let a = ascii();
    match kind {
        "summon.start" => (if a { ">" } else { "☉" }, Ink::Gold),
        "summon.end" => (if a { "." } else { "∎" }, Ink::Gold),
        "model" => (if a { "~" } else { "✶" }, Ink::Ember),
        "op" => (if a { "-" } else { "·" }, Ink::Dim),
        "op.refused" | "invalid" | "model.error" => (if a { "!" } else { "✕" }, Ink::Rubric),
        "item" => (if a { "+" } else { "❧" }, Ink::Verdigris),
        "abstain" => (if a { "?" } else { "◌" }, Ink::Gold),
        "child" => (if a { "\\" } else { "↳" }, Ink::Gold),
        "drift" => (if a { "!" } else { "≉" }, Ink::Ember),
        "release" => (if a { "<" } else { "◁" }, Ink::Dim),
        "banish" => (if a { "x" } else { "⊘" }, Ink::Rubric),
        _ => (if a { "-" } else { "·" }, Ink::Dim),
    }
}

/// The instrument supplies its own dark paper; plain mode inherits the terminal.
pub fn background() -> Color {
    if plain() {
        Color::Reset
    } else {
        Color::Rgb(20, 24, 27)
    }
}

pub fn reduced_motion() -> bool {
    plain() || std::env::var_os("SUMMONR_REDUCED_MOTION").is_some()
}

pub fn surface() -> Color {
    if plain() {
        Color::Reset
    } else {
        Color::Rgb(27, 33, 36)
    }
}

#[cfg(test)]
mod seal_tests {
    use super::*;

    #[test]
    fn seal_draws_nine_bit_grids() {
        // 0x8040201: the first 27 bits are 100000000 100000000 100000000.
        let rows = seal_with("8040201", false).unwrap();
        assert_eq!(rows[0], "█·· █·· █··");
        assert_eq!(rows[1], "··· ··· ···");
        assert_eq!(rows[2], "··· ··· ···");
    }

    #[test]
    fn seal_reads_bits_msb_first_row_by_row() {
        // 0xff8: the first nine bits are all set; the other two symbols are clear.
        let rows = seal_with("ff80000", false).unwrap();
        assert_eq!(rows, ["███ ··· ···", "███ ··· ···", "███ ··· ···"]);
    }

    #[test]
    fn seal_has_an_ascii_form() {
        assert_eq!(seal_with("8040201", true).unwrap()[0], "#.. #.. #..");
    }

    #[test]
    fn seal_rejects_short_or_non_hex() {
        assert!(seal("abc").is_none());
        assert!(seal("s-123456").is_none());
        assert!(seal("").is_none());
    }
}

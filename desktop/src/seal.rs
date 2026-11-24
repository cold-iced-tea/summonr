//! A run is drawn in ledger order on locations reserved by its pinned definition.
use gpui::{prelude::*, *};
use serde_json::Value;

#[derive(Clone, Default)]
pub struct Site {
    pub name: String,
    pub evidence: Option<String>,
}

#[derive(Clone, Default)]
pub struct Seal {
    pub sites: Vec<Site>,
    pub strokes: Vec<(usize, usize)>,
    pub last: Option<usize>,
}

impl Seal {
    fn reserve(&mut self, name: &str) -> usize {
        if let Some(i) = self.sites.iter().position(|s| s.name == name) {
            return i;
        }
        self.sites.push(Site {
            name: name.into(),
            evidence: None,
        });
        self.sites.len() - 1
    }
    pub fn layout(&mut self, steps: &Value) {
        for step in steps.as_array().into_iter().flatten() {
            if let Some(id) = step["id"].as_str() {
                self.reserve(id);
            }
            for body in ["then", "else", "steps"] {
                self.layout(&step[body]);
            }
        }
    }
    pub fn observe(&mut self, name: &str, text: &str) {
        let i = self.reserve(name);
        self.sites[i].evidence = Some(text.into());
        if let Some(prev) = self.last {
            if prev != i && !self.strokes.contains(&(prev, i)) {
                self.strokes.push((prev, i));
            }
        }
        self.last = Some(i);
    }
}

// Nine sites per cell. Continue long rituals into adjacent cells.
pub fn position(i: usize) -> (f32, f32) {
    let slot = [3, 1, 8, 0, 5, 7, 2, 6, 4][i % 9];
    let x = (slot % 3) as f32;
    let y = (slot / 3) as f32;
    (
        14. + (i / 9) as f32 * 91. + x * 27. - y * 4.,
        10. + y * 17. + x * 3.,
    )
}

// Live board states in Marain's MSP2 M9 phonetic registry (data/phonetic_registry.json).
const AT_REST: &[&[u16]] = &[&[133, 34], &[83, 127, 64, 34]];
const WORKING: &[&[u16]] = &[&[102, 129, 40, 117, 50]];
const WAITING: &[&[u16]] = &[&[102, 120, 117, 34, 117, 50]];
const OFFLINE: &[&[u16]] = &[&[132, 60, 87, 135, 117, 47]];

pub fn state_english(state: &str) -> &'static str {
    match state {
        "WORKING" => "working",
        "WAITING FOR YOU" => "waiting",
        "OFFLINE" | "CONNECTING" => "offline",
        _ => "at rest",
    }
}

fn state_words(state: &str) -> &'static [&'static [u16]] {
    match state {
        "WORKING" => WORKING,
        "WAITING FOR YOU" => WAITING,
        "OFFLINE" | "CONNECTING" => OFFLINE,
        _ => AT_REST,
    }
}

/// Encode 36 bits of a run/ritual ID (hex digits after any `prefix-`, or bytes)
/// into four deterministic 9-bit Marain M9 cells.
pub fn id_cells(id: &str) -> Vec<u16> {
    let payload = id.split_once('-').map(|(_, rest)| rest).unwrap_or(id);
    let mut bits: u64 = 0;
    let mut nbits: u32 = 0;
    for ch in payload.chars() {
        if let Some(d) = ch.to_digit(16) {
            bits = (bits << 4) | d as u64;
            nbits += 4;
            if nbits >= 36 {
                break;
            }
        }
    }
    if nbits < 36 {
        for &b in id.as_bytes() {
            bits = (bits << 8) | b as u64;
            nbits += 8;
            if nbits >= 36 {
                break;
            }
        }
    }
    (0..4)
        .map(|i| {
            let shift = (3 - i) * 9;
            let mask = ((bits >> shift) & 0x1ff) as u16;
            if mask == 0 { 0x010 } else { mask }
        })
        .collect()
}

fn render_m9(words: Vec<Vec<u16>>, stroke_color: u32) -> impl IntoElement {
    let cells: usize = words.iter().map(Vec::len).sum();
    let gaps = words.len().saturating_sub(1);
    let width = (8. + cells as f32 * 14. + gaps as f32 * 6.).max(24.);
    canvas(
        |_, _, _| (),
        move |bounds, _, win, _| {
            let mut x = 4.;
            let mut pen = PathBuilder::stroke(px(0.95));
            let mut dots = Vec::new();
            for word in &words {
                for &mask in word {
                    let mut points = Vec::new();
                    for i in 0..9 {
                        if mask & (1 << (8 - i)) == 0 {
                            continue;
                        }
                        let col = (i % 3) as f32;
                        let row = (i / 3) as f32;
                        let p = bounds.origin
                            + point(px(x + col * 4.4 - row * 1.3), px(7. + row * 4. + col));
                        for &(j, q) in &points {
                            if i32::abs(i / 3 - j / 3) <= 1 && i32::abs(i % 3 - j % 3) <= 1 {
                                pen.move_to(q);
                                pen.line_to(p);
                            }
                        }
                        points.push((i, p));
                        dots.push(p);
                    }
                    x += 14.;
                }
                x += 6.;
            }
            if let Ok(path) = pen.build() {
                win.paint_path(path, rgb(stroke_color));
            }
            for p in dots {
                win.paint_quad(fill(
                    Bounds::new(p - point(px(1.), px(1.)), size(px(2.), px(2.))),
                    rgb(super::INK),
                ));
            }
        },
    )
    .w(px(width))
    .h(px(24.))
}

pub fn state_glyphs(state: &str, color: u32) -> impl IntoElement {
    let words = state_words(state).iter().map(|w| w.to_vec()).collect();
    render_m9(words, color)
}

pub fn id_sigil(id: &str, color: u32) -> impl IntoElement {
    render_m9(vec![id_cells(id)], color)
}

#[cfg(test)]
mod tests {
    use super::{Seal, id_cells};
    use serde_json::json;

    #[test]
    fn branching_runs_keep_sites_and_follow_observed_order() {
        let mut left = Seal::default();
        left.layout(&json!([{"id":"gate","then":[{"id":"accept"}],"else":[{"id":"reject"}]},{"id":"finish"}]));
        let mut right = left.clone();
        for (seal, branch) in [(&mut left, "accept"), (&mut right, "reject")] {
            for site in ["gate", branch, branch, "finish"] {
                seal.observe(site, "observed");
            }
        }
        assert_eq!(left.strokes, [(0, 1), (1, 3)]);
        assert_eq!(right.strokes, [(0, 2), (2, 3)]);
        assert!(left.sites[2].evidence.is_none());
        assert!(right.sites[1].evidence.is_none());
    }

    #[test]
    fn run_id_sigils_are_deterministic_and_distinct() {
        let a = id_cells("s-1b1f570b4e7f");
        let b = id_cells("s-f01bdc8a9123");
        assert_eq!(a.len(), 4);
        assert_eq!(a, id_cells("s-1b1f570b4e7f"));
        assert_ne!(a, b);
        assert!(a.iter().all(|&m| m > 0 && m <= 0x1ff));
    }
}

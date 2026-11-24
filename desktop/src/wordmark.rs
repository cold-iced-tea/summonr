//! A drawn seven-letter stencil. Coordinates are in design cells; counters
//! are cut out before shading so glyph metrics never affect the wordmark.
use std::sync::OnceLock;

type Contour = &'static [(i16, i16)];
const S: Contour = &[
    (3, 0),
    (14, 0),
    (14, 3),
    (5, 3),
    (4, 4),
    (4, 6),
    (12, 6),
    (14, 8),
    (14, 13),
    (11, 16),
    (0, 16),
    (0, 13),
    (9, 13),
    (10, 12),
    (10, 10),
    (2, 10),
    (0, 8),
    (0, 3),
];
const U: Contour = &[
    (0, 0),
    (4, 0),
    (4, 11),
    (6, 13),
    (9, 13),
    (11, 11),
    (11, 0),
    (15, 0),
    (15, 13),
    (12, 16),
    (3, 16),
    (0, 13),
];
const M: Contour = &[
    (0, 0),
    (4, 0),
    (9, 7),
    (14, 0),
    (18, 0),
    (18, 16),
    (14, 16),
    (14, 6),
    (9, 13),
    (4, 6),
    (4, 16),
    (0, 16),
];
const O: Contour = &[
    (3, 0),
    (12, 0),
    (15, 3),
    (15, 13),
    (12, 16),
    (3, 16),
    (0, 13),
    (0, 3),
];
const O_HOLE: Contour = &[
    (5, 3),
    (10, 3),
    (11, 4),
    (11, 12),
    (10, 13),
    (5, 13),
    (4, 12),
    (4, 4),
];
const N: Contour = &[
    (0, 0),
    (4, 0),
    (11, 10),
    (11, 0),
    (15, 0),
    (15, 16),
    (11, 16),
    (4, 6),
    (4, 16),
    (0, 16),
];
const R: Contour = &[
    (0, 0),
    (12, 0),
    (15, 3),
    (15, 7),
    (12, 10),
    (10, 10),
    (16, 16),
    (10, 16),
    (5, 10),
    (4, 10),
    (4, 16),
    (0, 16),
];
const R_HOLE: Contour = &[(4, 3), (10, 3), (11, 4), (11, 6), (10, 7), (4, 7)];
const LETTERS: &[(i16, Contour, Contour)] = &[
    (0, S, &[]),
    (17, U, &[]),
    (35, M, &[]),
    (56, M, &[]),
    (77, O, O_HOLE),
    (95, N, &[]),
    (113, R, R_HOLE),
];
pub const W: usize = 278;
pub const H: usize = 39;
pub struct Strip {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub ink: u32,
}

fn inside(shape: Contour, x: f32, y: f32) -> bool {
    let mut crossed = false;
    for (&(ax, ay), &(bx, by)) in shape.iter().zip(shape.iter().cycle().skip(1)) {
        let (ax, ay, bx, by) = (ax as f32, ay as f32, bx as f32, by as f32);
        if (ay > y) != (by > y) && x < ax + (bx - ax) * (y - ay) / (by - ay) {
            crossed = !crossed;
        }
    }
    crossed
}
pub fn strips() -> &'static [Strip] {
    static CACHE: OnceLock<Vec<Strip>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut face = [[false; W]; H];
        for (row, line) in face.iter_mut().enumerate().take(32) {
            let y = (row as f32 + 0.5) / 2.;
            for (col, bit) in line.iter_mut().enumerate() {
                let x = (col as f32 + 0.5) / 2. - (16. - y) * 0.34;
                *bit = LETTERS.iter().any(|&(origin, outline, hole)| {
                    let local = x - origin as f32;
                    (0.0..=18.).contains(&local)
                        && inside(outline, local, y)
                        && !inside(hole, local, y)
                });
            }
        }
        let mut strips: Vec<Strip> = Vec::new();
        for y in 0..H {
            for x in 0..W {
                let color = if face[y][x] {
                    if y == 0 || !face[y - 1][x] {
                        0xe9ffd4
                    } else if y == 31 || !face[y + 1][x] {
                        0x759a58
                    } else {
                        match y {
                            0..=7 => 0xd3f69d,
                            8..=13 => 0xb7e37d,
                            14..=16 => 0x8bae63,
                            17..=19 => 0x597c50,
                            20..=25 => 0x94c56a,
                            _ => 0xbfe882,
                        }
                    }
                } else if (1..=5).any(|n| x >= n && y >= n && face[y - n][x - n]) {
                    if (x + y) % 5 == 0 { 0x716778 } else { 0x453f54 }
                } else {
                    continue;
                };
                if let Some(last) = strips.last_mut()
                    && last.y == y
                    && last.x + last.width == x
                    && last.ink == color
                {
                    last.width += 1;
                } else {
                    strips.push(Strip {
                        x,
                        y,
                        width: 1,
                        ink: color,
                    });
                }
            }
        }
        strips
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lettering_has_seven_separate_faces_before_extrusion() {
        let mut face = [[false; W]; H];
        for strip in strips() {
            if !matches!(strip.ink, 0x453f54 | 0x716778) {
                face[strip.y][strip.x..strip.x + strip.width].fill(true);
            }
        }
        let mut letters = 0;
        for y in 0..H {
            for x in 0..W {
                if !face[y][x] {
                    continue;
                }
                letters += 1;
                let mut stack = vec![(x, y)];
                while let Some((cx, cy)) = stack.pop() {
                    if !std::mem::take(&mut face[cy][cx]) {
                        continue;
                    }
                    for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
                        if let (Some(nx), Some(ny)) =
                            (cx.checked_add_signed(dx), cy.checked_add_signed(dy))
                            && nx < W
                            && ny < H
                            && face[ny][nx]
                        {
                            stack.push((nx, ny));
                        }
                    }
                }
            }
        }
        assert_eq!(letters, 7);
    }
}

//! Character-cell artwork for the board header.
use std::f32::consts::TAU;

/// Project a tilted torus into character cells. Depth picks the visible
/// surface and the surface normal picks the ink density at that cell.
pub fn core(frame: u32, mark: char) -> String {
    const WIDTH: usize = 31;
    const HEIGHT: usize = 11;
    let mut cells = [[' '; WIDTH]; HEIGHT];
    let mut depth = [[f32::NEG_INFINITY; WIDTH]; HEIGHT];
    let phase = (frame % 32) as f32 * TAU / 32.;
    let (tilt_sin, tilt_cos) = (0.68 + 0.32 * phase.sin()).sin_cos();
    let (roll_sin, roll_cos) = (0.35 + 0.55 * phase.cos()).sin_cos();
    let ink = ['·', ':', '░', '▒', '▓', '█'];
    for i in 0..96 {
        let (s, c) = (i as f32 * TAU / 96.).sin_cos();
        for j in 0..20 {
            let (v, u) = (j as f32 * TAU / 20.).sin_cos();
            let radius = 1.0 + 0.24 * u;
            let x = radius * c;
            let y = radius * s * tilt_cos - 0.24 * v * tilt_sin;
            let z = radius * s * tilt_sin + 0.24 * v * tilt_cos;
            let col = (15. + (x * roll_cos - y * roll_sin) * 11.).round() as usize;
            let row = (5. + (x * roll_sin + y * roll_cos) * 4.).round() as usize;
            if row < HEIGHT && col < WIDTH && z > depth[row][col] {
                depth[row][col] = z;
                let light = (0.48 - 0.32 * c * u - 0.19 * s * u + 0.3 * v).clamp(0., 1.);
                cells[row][col] = ink[(light * 5.).round() as usize];
            }
        }
    }
    cells[5][15] = mark;
    cells.map(|r| r.iter().collect::<String>()).join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn orbit_loops_on_a_fixed_grid() {
        assert_eq!(core(0, '?'), core(32, '?'));
        assert_ne!(core(0, '◇'), core(8, '◇'));
        assert!(core(0, '?').lines().all(|line| line.chars().count() == 31));
        assert_eq!(
            core(0, '?').lines().nth(5).unwrap().chars().nth(15),
            Some('?')
        );
    }
}

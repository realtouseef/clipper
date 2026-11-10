//! Gradient generator: colors from a base hue and a harmony rule, in OKLCh so the colors
//! have even lightness and chroma.

use super::color::{self, Rgb};
use super::{Gradient, GradientKind, Stop};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Harmony {
    Analogous,
    Complementary,
    Triad,
    SplitComplementary,
}

impl Harmony {
    pub const ALL: [Harmony; 4] = [Harmony::Analogous, Harmony::Complementary, Harmony::Triad, Harmony::SplitComplementary];
    pub fn label(self) -> &'static str {
        match self {
            Harmony::Analogous => "Analogous",
            Harmony::Complementary => "Complementary",
            Harmony::Triad => "Triad",
            Harmony::SplitComplementary => "Split",
        }
    }

    /// Hue offsets from the base hue, in degrees.
    pub fn offsets(self) -> &'static [f32] {
        match self {
            Harmony::Analogous => &[-30.0, 0.0, 30.0],
            Harmony::Complementary => &[0.0, 180.0],
            Harmony::Triad => &[0.0, 120.0, 240.0],
            Harmony::SplitComplementary => &[0.0, 150.0, 210.0],
        }
    }
}

/// Colors for `hue` (degrees) under `harmony`, light to dark-ish, chroma `chroma`.
pub fn palette(hue: f32, harmony: Harmony, lightness: f32, chroma: f32) -> Vec<Rgb> {
    let offs = harmony.offsets();
    let n = offs.len() as f32;
    offs.iter()
        .enumerate()
        .map(|(i, o)| {
            // A gentle lightness ramp keeps the result from looking flat.
            let l = (lightness + 0.08 - 0.16 * i as f32 / (n - 1.0).max(1.0)).clamp(0.2, 0.95);
            color::from_lch(l, chroma, (hue + o).rem_euclid(360.0))
        })
        .collect()
}

pub fn gradient(hue: f32, harmony: Harmony, angle: f32) -> Gradient {
    Gradient::linear(angle, &palette(hue, harmony, 0.72, 0.14))
}

/// A small deterministic PRNG (xorshift), seeded by the caller.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// A random gradient: random hue, harmony, lightness, kind and angle.
pub fn random(seed: u64) -> (Gradient, f32, Harmony) {
    let mut r = Rng::new(seed);
    let hue = r.unit() * 360.0;
    let harmony = Harmony::ALL[(r.next() % 4) as usize];
    let colors = palette(hue, harmony, 0.55 + r.unit() * 0.3, 0.08 + r.unit() * 0.12);
    let n = colors.len().max(2) - 1;
    let stops: Vec<Stop> = colors.iter().enumerate().map(|(i, &c)| Stop { pos: i as f32 / n as f32, color: c }).collect();
    let kind = match r.next() % 4 {
        0 | 1 => GradientKind::Linear { angle: (r.unit() * 360.0).round() },
        2 => GradientKind::Radial { cx: 0.2 + r.unit() * 0.6, cy: 0.2 + r.unit() * 0.6 },
        _ => {
            let mut grid = colors.clone();
            grid.push(colors[0]);
            grid.truncate(4);
            while grid.len() < 4 {
                grid.push(colors[grid.len() % colors.len()]);
            }
            GradientKind::Mesh { cols: 2, rows: 2, colors: grid }
        }
    };
    (Gradient { kind, stops, grain: 8.0 }, hue, harmony)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harmonies_place_hues() {
        for h in Harmony::ALL {
            let p = palette(40.0, h, 0.7, 0.12);
            assert_eq!(p.len(), h.offsets().len());
            // Each color's hue sits near base + offset.
            for (c, o) in p.iter().zip(h.offsets()) {
                let (_, _, hue) = color::lch(*c);
                let want = (40.0 + o).rem_euclid(360.0);
                let d = ((hue - want + 540.0).rem_euclid(360.0) - 180.0).abs();
                assert!(d < 12.0, "{h:?}: {hue} vs {want}");
            }
        }
        let a = random(7).0;
        assert_eq!(a, random(7).0, "same seed, same gradient");
        assert_ne!(a, random(8).0);
    }
}

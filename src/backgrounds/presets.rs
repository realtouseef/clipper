//! The built-in backgrounds: five gradient presets (one light, three colorful, one dark) and
//! the animated presets. Every other gradient the app used to ship is in a compact table
//! (`assets/gradients/legacy.tsv`) that is only read to migrate a favorite that pointed at
//! one (`legacy`); projects store a background's full stops, so they never needed it.

use super::color::{self, Rgb};
use super::{AnimStyle, Animated, Gradient, GradientKind, Stop};
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct Preset {
    /// Stable id ("app:N", the gradient's id since it first shipped).
    pub id: String,
    pub name: String,
    pub gradient: Gradient,
}

/// The five presets, in picker order: light, three colorful, dark. Picked from the old set
/// for smooth transitions (neighboring hues, no gray middle) and contrast with both light
/// and dark screen content.
pub fn gradients() -> &'static [Preset] {
    static ALL: OnceLock<Vec<Preset>> = OnceLock::new();
    ALL.get_or_init(|| {
        crate::style::GRADIENTS
            .iter()
            .enumerate()
            .map(|(i, g)| Preset { id: g.id.to_string(), name: g.name.to_string(), gradient: builtin_gradient(i) })
            .collect()
    })
}

/// Preset `i` (see `style::GRADIENTS`).
pub fn builtin_gradient(i: usize) -> Gradient {
    let g = &crate::style::GRADIENTS[i.min(crate::style::GRADIENTS.len() - 1)];
    let kind = match g.shape {
        crate::style::Shape::Linear(a) => GradientKind::Linear { angle: a },
        crate::style::Shape::Radial(x, y) => GradientKind::Radial { cx: x, cy: y },
    };
    Gradient { kind, stops: g.stops.iter().map(|&(pos, color)| Stop { pos, color }).collect(), grain: 0.0 }
}

/// A removed preset by its old id: (name, gradient).
pub fn legacy(id: &str) -> Option<(String, Gradient)> {
    include_str!("../../assets/gradients/legacy.tsv").lines().filter(|l| !l.starts_with('#')).find_map(|line| {
        let mut f = line.split('\t');
        if f.next()? != id {
            return None;
        }
        let name = f.next()?.to_string();
        let shape = f.next()?;
        let stops: Vec<Stop> = f
            .next()?
            .split(' ')
            .filter_map(|s| {
                let (p, c) = s.split_once(':')?;
                Some(Stop { pos: p.parse().ok()?, color: color::parse_hex(c)? })
            })
            .collect();
        let kind = match shape.split_at(1) {
            ("L", a) => GradientKind::Linear { angle: a.parse().ok()? },
            ("R", xy) => {
                let (x, y) = xy.split_once(',')?;
                GradientKind::Radial { cx: x.parse().ok()?, cy: y.parse().ok()? }
            }
            _ => return None,
        };
        (stops.len() >= 2).then(|| (name, Gradient { kind, stops, grain: 0.0 }))
    })
}

/// Any preset id, kept or removed.
pub fn resolve(id: &str) -> Option<(String, Gradient)> {
    gradients().iter().find(|p| p.id == id).map(|p| (p.name.clone(), p.gradient.clone())).or_else(|| legacy(id))
}

#[derive(Clone, Debug)]
pub struct AnimPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub anim: Animated,
}

const fn hex(v: u32) -> Rgb {
    [(v >> 16) as u8, (v >> 8) as u8, v as u8]
}

/// The animated presets (used for defaults and tests; the picker animates any gradient).
pub fn animated() -> &'static [AnimPreset] {
    static ALL: OnceLock<Vec<AnimPreset>> = OnceLock::new();
    ALL.get_or_init(|| {
        let a = |id, name, style, colors: &[u32], speed: f32, intensity: f32, blur: f32, period: f32| AnimPreset {
            id,
            name,
            anim: Animated { style, colors: colors.iter().map(|&c| hex(c)).collect(), speed, intensity, blur, grain: 12.0, period, frozen: false },
        };
        use AnimStyle::*;
        vec![
            a("northern", "Northern lights", Aurora, &[0x061423, 0x18c99b, 0x2a6df4, 0x9b5cf6], 1.0, 0.75, 0.6, 10.0),
            a("nebula", "Nebula", Aurora, &[0x0d0221, 0xff2a6d, 0x7b2cbf, 0x05d9e8], 1.0, 0.8, 0.55, 8.0),
            a("lagoon", "Lagoon", Aurora, &[0x02354a, 0x00b4d8, 0x90e0ef, 0x48cae4], 1.0, 0.6, 0.7, 12.0),
            a("peach", "Peach haze", Aurora, &[0xffe5d9, 0xffb4a2, 0xe5989b, 0xffcdb2], 1.0, 0.6, 0.75, 10.0),
            a("lime", "Lime fizz", Aurora, &[0x0a0a0b, 0xa4f47c, 0x3ddc97, 0x1b4332], 1.0, 0.7, 0.6, 8.0),
            a("ember", "Ember", Aurora, &[0x1a0505, 0xff5400, 0xff9e00, 0xc1121f], 2.0, 0.75, 0.5, 8.0),
            a("dawn-turn", "Dawn turn", Rotate, &[0x72a7e8, 0xfd8152, 0xf9ca56], 1.0, 0.5, 0.5, 12.0),
            a("ocean-turn", "Ocean turn", Rotate, &[0x4facfe, 0x00f2fe, 0x43e97b], 1.0, 0.5, 0.5, 16.0),
            a("berry-pulse", "Berry pulse", Pulse, &[0xf093fb, 0xf5576c, 0x4a1180], 1.0, 0.7, 0.5, 6.0),
            a("deep-pulse", "Deep pulse", Pulse, &[0x55f5dd, 0x04387e, 0x02254e], 1.0, 0.6, 0.5, 8.0),
            a("sunset-wave", "Sunset wave", Wave, &[0xfa709a, 0xfee140, 0xff8a5c], 1.0, 0.7, 0.5, 8.0),
            a("mint-wave", "Mint wave", Wave, &[0x84fab0, 0x8fd3f4, 0xa18cd1], 1.0, 0.6, 0.5, 10.0),
            a("mono-drift", "Graphite drift", Aurora, &[0x111114, 0x3a3a42, 0x5c5c66, 0x2a2a30], 1.0, 0.6, 0.7, 14.0),
            a("candy", "Candy", Rotate, &[0xff9a9e, 0xfad0c4, 0xa18cd1, 0xfbc2eb], 2.0, 0.5, 0.5, 10.0),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_five_presets_light_dark_and_colorful() {
        let all = gradients();
        assert_eq!(all.len(), 5);
        let light = |p: &Preset| p.gradient.colors().iter().map(|&c| color::lch(c).0).sum::<f32>() / p.gradient.stops.len() as f32;
        assert!(all.iter().any(|p| light(p) > 0.8), "one light");
        assert!(all.iter().any(|p| light(p) < 0.35), "one dark");
        let mut ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        ids.dedup();
        assert_eq!(ids.len(), 5);
    }

    #[test]
    fn removed_presets_still_resolve() {
        // The old default (Dawn), a uiGradients and a WebGradients preset.
        let (name, g) = resolve("app:0").expect("Dawn");
        assert_eq!(name, "Dawn");
        assert_eq!(g.kind, GradientKind::Linear { angle: 112.0 });
        assert_eq!(g.stops[0], Stop { pos: 0.09, color: [0x72, 0xa7, 0xe8] });
        assert!(matches!(resolve("app:2").unwrap().1.kind, GradientKind::Radial { .. }));
        assert!(legacy("zzz").is_none());
        let n = include_str!("../../assets/gradients/legacy.tsv").lines().filter(|l| !l.starts_with('#')).count();
        assert!(n > 500, "{n}");
        // Kept presets resolve to themselves.
        assert_eq!(resolve(&gradients()[0].id).unwrap().1, gradients()[0].gradient);
    }

    #[test]
    fn at_least_12_animated_presets() {
        assert!(animated().len() >= 12);
        for p in animated() {
            assert!((4.0..=20.0).contains(&p.anim.period) && p.anim.colors.len() >= 2, "{}", p.name);
        }
    }
}

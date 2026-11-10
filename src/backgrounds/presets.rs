//! The gradient library: the app's own gradients plus uiGradients and WebGradients (both MIT,
//! bundled with their licenses; see Credits), categorized by color, and the animated presets.
//! The JSON is compiled into the app and parsed once on first use.

use super::color::{self, Rgb};
use super::{AnimStyle, Animated, Gradient, GradientKind, Stop};
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Category {
    Dark,
    Pastel,
    Vibrant,
    Sunset,
    Ocean,
    Neon,
    Minimal,
}

impl Category {
    pub const ALL: [Category; 7] =
        [Category::Dark, Category::Pastel, Category::Vibrant, Category::Sunset, Category::Ocean, Category::Neon, Category::Minimal];
    pub fn label(self) -> &'static str {
        match self {
            Category::Dark => "Dark",
            Category::Pastel => "Pastel",
            Category::Vibrant => "Vibrant",
            Category::Sunset => "Sunset",
            Category::Ocean => "Ocean",
            Category::Neon => "Neon",
            Category::Minimal => "Minimal",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Preset {
    /// Stable id: "app:N", "ug:Name", "wg:NNN".
    pub id: String,
    pub name: String,
    pub source: &'static str,
    pub gradient: Gradient,
    pub categories: Vec<Category>,
}

/// Categories from the colors' OKLCh lightness, chroma and hue. A gradient can be in several.
pub fn categorize(colors: &[Rgb]) -> Vec<Category> {
    if colors.is_empty() {
        return Vec::new();
    }
    let lch: Vec<(f32, f32, f32)> = colors.iter().map(|&c| color::lch(c)).collect();
    let n = lch.len() as f32;
    let avg_l = lch.iter().map(|c| c.0).sum::<f32>() / n;
    let avg_c = lch.iter().map(|c| c.1).sum::<f32>() / n;
    let max_c = lch.iter().map(|c| c.1).fold(0.0, f32::max);
    let chromatic: Vec<f32> = lch.iter().filter(|c| c.1 > 0.05).map(|c| c.2).collect();
    let share = |f: &dyn Fn(f32) -> bool| if chromatic.is_empty() { 0.0 } else { chromatic.iter().filter(|&&h| f(h)).count() as f32 / chromatic.len() as f32 };
    let spread = {
        let first = colors[0];
        colors.iter().map(|&c| {
            let (a, b) = (color::to_lab(first), color::to_lab(c));
            ((a.l - b.l).powi(2) + (a.a - b.a).powi(2) + (a.b - b.b).powi(2)).sqrt()
        }).fold(0.0, f32::max)
    };
    let mut out = Vec::new();
    if avg_l < 0.45 {
        out.push(Category::Dark);
    }
    if avg_l > 0.78 && avg_c < 0.11 {
        out.push(Category::Pastel);
    }
    if avg_c > 0.14 {
        out.push(Category::Vibrant);
    }
    // Reds, oranges, yellows and pinks.
    if share(&|h| !(95.0..=320.0).contains(&h)) >= 0.6 && avg_c > 0.06 {
        out.push(Category::Sunset);
    }
    // Teals, cyans and blues.
    if share(&|h| (170.0..=275.0).contains(&h)) >= 0.6 {
        out.push(Category::Ocean);
    }
    if max_c > 0.22 && lch.iter().any(|c| c.1 > 0.2 && c.0 > 0.6) {
        out.push(Category::Neon);
    }
    if avg_c < 0.05 || spread < 0.12 {
        out.push(Category::Minimal);
    }
    out
}

/// The app's original gradient `i` (see `style::GRADIENTS`).
pub fn builtin_gradient(i: usize) -> Gradient {
    let g = &crate::style::GRADIENTS[i.min(crate::style::GRADIENTS.len() - 1)];
    let kind = match g.shape {
        crate::style::Shape::Linear(a) => GradientKind::Linear { angle: a },
        crate::style::Shape::Radial(x, y) => GradientKind::Radial { cx: x, cy: y },
    };
    Gradient { kind, stops: g.stops.iter().map(|&(pos, color)| Stop { pos, color }).collect(), grain: 0.0 }
}

#[derive(Deserialize)]
struct UiGradient {
    name: String,
    colors: Vec<String>,
}

#[derive(Deserialize)]
struct WebStop {
    color: String,
    pos: f32,
}

#[derive(Deserialize)]
struct WebGradient {
    name: String,
    index: String,
    deg: f32,
    gradient: Vec<WebStop>,
}

fn build() -> Vec<Preset> {
    let mut out = Vec::new();
    let mut push = |id: String, name: String, source: &'static str, gradient: Gradient| {
        let categories = categorize(&gradient.colors());
        out.push(Preset { id, name, source, gradient, categories });
    };
    for (i, g) in crate::style::GRADIENTS.iter().enumerate() {
        push(format!("app:{i}"), g.name.to_string(), "Clipper", builtin_gradient(i));
    }
    let ug: Vec<UiGradient> = serde_json::from_str(include_str!("../../assets/gradients/uigradients.json")).unwrap_or_default();
    for g in ug {
        let colors: Vec<Rgb> = g.colors.iter().filter_map(|c| color::parse_hex(c)).collect();
        if colors.len() >= 2 {
            push(format!("ug:{}", g.name), g.name, "uiGradients", Gradient::linear(90.0, &colors));
        }
    }
    let wg: Vec<WebGradient> = serde_json::from_str(include_str!("../../assets/gradients/webgradients.json")).unwrap_or_default();
    for g in wg {
        let stops: Vec<Stop> = g.gradient.iter().filter_map(|s| Some(Stop { pos: s.pos / 100.0, color: color::parse_hex(&s.color)? })).collect();
        if stops.len() >= 2 {
            push(format!("wg:{}", g.index), g.name, "WebGradients", Gradient { kind: GradientKind::Linear { angle: g.deg }, stops, grain: 0.0 });
        }
    }
    out
}

/// Every built-in gradient preset (the app's own first).
pub fn gradients() -> &'static [Preset] {
    static ALL: OnceLock<Vec<Preset>> = OnceLock::new();
    ALL.get_or_init(build)
}

/// Presets matching a name search and an optional category.
pub fn search(query: &str, category: Option<Category>) -> Vec<&'static Preset> {
    let q = query.trim().to_lowercase();
    gradients()
        .iter()
        .filter(|p| q.is_empty() || p.name.to_lowercase().contains(&q))
        .filter(|p| category.is_none_or(|c| p.categories.contains(&c)))
        .collect()
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

/// The animated presets.
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
    fn library_has_over_200_categorized_presets() {
        let all = gradients();
        assert!(all.len() > 200, "{} presets", all.len());
        assert!(all.iter().any(|p| p.source == "uiGradients") && all.iter().any(|p| p.source == "WebGradients"));
        // Ids are unique.
        let mut ids: Vec<&str> = all.iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        for c in Category::ALL {
            let n = search("", Some(c)).len();
            assert!(n >= 5, "{c:?} has only {n}");
        }
        assert!(!search("sunset", None).is_empty());
        assert!(search("zzz-no-such", None).is_empty());
    }

    #[test]
    fn categories_follow_color() {
        assert!(categorize(&[[10, 10, 20], [30, 20, 60]]).contains(&Category::Dark));
        assert!(categorize(&[[250, 220, 230], [220, 235, 250]]).contains(&Category::Pastel));
        assert!(categorize(&[[0, 90, 200], [0, 200, 220]]).contains(&Category::Ocean));
        assert!(categorize(&[[255, 94, 0], [255, 200, 0]]).contains(&Category::Sunset));
        assert!(categorize(&[[200, 200, 200], [210, 210, 210]]).contains(&Category::Minimal));
    }

    #[test]
    fn at_least_12_animated_presets() {
        assert!(animated().len() >= 12);
        for p in animated() {
            assert!((4.0..=20.0).contains(&p.anim.period) && p.anim.colors.len() >= 2, "{}", p.name);
        }
    }
}

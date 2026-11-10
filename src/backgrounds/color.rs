//! OKLab color math. Gradients interpolate in OKLab so mid-tones stay clean (sRGB mixing
//! goes muddy between complementary colors).

pub type Rgb = [u8; 3];

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Lab {
    pub l: f32,
    pub a: f32,
    pub b: f32,
}

fn to_linear(c: u8) -> f32 {
    let s = c as f32 / 255.0;
    if s <= 0.04045 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let s = if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

pub fn to_lab(c: Rgb) -> Lab {
    let (r, g, b) = (to_linear(c[0]), to_linear(c[1]), to_linear(c[2]));
    let l = (0.412_221_47 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    Lab {
        l: 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        a: 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        b: 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    }
}

pub fn to_rgb(c: Lab) -> Rgb {
    let l = c.l + 0.396_337_78 * c.a + 0.215_803_76 * c.b;
    let m = c.l - 0.105_561_346 * c.a - 0.063_854_17 * c.b;
    let s = c.l - 0.089_484_18 * c.a - 1.291_485_5 * c.b;
    let (l, m, s) = (l * l * l, m * m * m, s * s * s);
    [
        from_linear(4.076_741_7 * l - 3.307_711_6 * m + 0.230_969_94 * s),
        from_linear(-1.268_438 * l + 2.609_757_4 * m - 0.341_319_38 * s),
        from_linear(-0.004_196_086_3 * l - 0.703_418_6 * m + 1.707_614_7 * s),
    ]
}

pub fn mix(a: Lab, b: Lab, t: f32) -> Lab {
    Lab { l: a.l + (b.l - a.l) * t, a: a.a + (b.a - a.a) * t, b: a.b + (b.b - a.b) * t }
}

/// Lightness, chroma and hue (degrees) of a color.
pub fn lch(c: Rgb) -> (f32, f32, f32) {
    let lab = to_lab(c);
    let h = lab.b.atan2(lab.a).to_degrees().rem_euclid(360.0);
    (lab.l, (lab.a * lab.a + lab.b * lab.b).sqrt(), h)
}

/// A color from OKLCh (hue in degrees), clipped into sRGB.
pub fn from_lch(l: f32, c: f32, h: f32) -> Rgb {
    let (s, k) = h.to_radians().sin_cos();
    to_rgb(Lab { l, a: c * k, b: c * s })
}

pub fn parse_hex(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    let s = if s.len() == 3 { s.chars().flat_map(|c| [c, c]).collect::<String>() } else { s.to_string() };
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(&s, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// A 256-step lookup table for stops `(pos 0..=1, color)` interpolated in OKLab.
pub fn lut(stops: &[(f32, Rgb)]) -> Vec<Rgb> {
    let mut s: Vec<(f32, Lab)> = stops.iter().map(|&(p, c)| (p.clamp(0.0, 1.0), to_lab(c))).collect();
    s.sort_by(|a, b| a.0.total_cmp(&b.0));
    if s.is_empty() {
        return vec![[0, 0, 0]; 256];
    }
    (0..256)
        .map(|i| {
            let t = i as f32 / 255.0;
            let c = match s.iter().position(|(p, _)| *p >= t) {
                Some(0) => s[0].1,
                None => s[s.len() - 1].1,
                Some(j) => {
                    let (p0, c0) = s[j - 1];
                    let (p1, c1) = s[j];
                    let f = if p1 > p0 { (t - p0) / (p1 - p0) } else { 1.0 };
                    mix(c0, c1, f)
                }
            };
            to_rgb(c)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oklab_round_trip_and_endpoints() {
        for c in [[0, 0, 0], [255, 255, 255], [255, 0, 0], [12, 200, 99], [250, 128, 3]] {
            let back = to_rgb(to_lab(c));
            for k in 0..3 {
                assert!((back[k] as i32 - c[k] as i32).abs() <= 1, "{c:?} -> {back:?}");
            }
        }
        // Endpoints of an interpolation are the stop colors exactly.
        let l = lut(&[(0.0, [255, 0, 0]), (1.0, [0, 0, 255])]);
        assert_eq!(l[0], [255, 0, 0]);
        assert_eq!(l[255], [0, 0, 255]);
        // OKLab keeps the middle brighter and less muddy than sRGB's (127, 0, 127).
        let mid = l[128];
        let (lm, _, _) = lch(mid);
        let (ls, _, _) = lch([127, 0, 127]);
        assert!(lm > ls, "OKLab mid {mid:?} should be lighter than the sRGB average");
        // Outside the stops: held at the ends.
        let l = lut(&[(0.25, [10, 10, 10]), (0.75, [200, 200, 200])]);
        assert_eq!(l[0], [10, 10, 10]);
        assert_eq!(l[255], [200, 200, 200]);
    }

    #[test]
    fn hex_and_lch() {
        assert_eq!(parse_hex("#ff8000"), Some([255, 128, 0]));
        assert_eq!(parse_hex("0f0"), Some([0, 255, 0]));
        assert_eq!(parse_hex("nope"), None);
        let c = from_lch(0.7, 0.1, 200.0);
        let (l, ch, h) = lch(c);
        assert!((l - 0.7).abs() < 0.01 && (ch - 0.1).abs() < 0.01 && (h - 200.0).abs() < 2.0);
    }
}

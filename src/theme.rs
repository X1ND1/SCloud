//! Themes (like Discord's): built-in ones and a custom one with a gradient made of "keys",
//! window transparency and smooth transitions between themes.

use eframe::egui::Color32;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use std::time::Instant;

/// How many gradient points a palette stores (for drawing and smooth transitions).
pub const N: usize = 8;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// background gradient, evenly sampled at N points
    pub grad: [Color32; N],
    /// gradient angle in degrees, like CSS: 180 = top to bottom
    pub angle: f32,
    pub surface: Color32,
    pub hover: Color32,
    pub bar: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub wave: Color32,
    /// pop-up window background (opaque, for readability)
    pub window: Color32,
    pub field: Color32,
    /// 0.3..1, window opacity
    pub opacity: f32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

const fn flat(c: Color32) -> [Color32; N] {
    [c; N]
}

pub const DARK: Palette = Palette {
    grad: flat(rgb(18, 18, 18)),
    angle: 180.0,
    surface: rgb(28, 28, 31),
    hover: rgb(36, 36, 40),
    bar: rgb(12, 12, 12),
    text: rgb(245, 245, 245),
    muted: rgb(153, 153, 153),
    accent: rgb(255, 85, 0),
    wave: rgb(110, 76, 255),
    window: rgb(28, 28, 31),
    field: rgb(44, 44, 48),
    opacity: 1.0,
};

pub const MIDNIGHT: Palette = Palette {
    grad: flat(rgb(0, 0, 0)),
    angle: 180.0,
    surface: rgb(16, 16, 18),
    hover: rgb(26, 26, 30),
    bar: rgb(0, 0, 0),
    text: rgb(240, 240, 240),
    muted: rgb(130, 130, 136),
    accent: rgb(255, 85, 0),
    wave: rgb(110, 76, 255),
    window: rgb(14, 14, 16),
    field: rgb(30, 30, 34),
    opacity: 1.0,
};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stop {
    pub pos: f32,
    pub color: [u8; 3],
}

const fn stop(pos: f32, r: u8, g: u8, b: u8) -> Stop {
    Stop {
        pos,
        color: [r, g, b],
    }
}

/// GTA VI / Vice City sunset, like Discord's purple-orange theme: saturated purple on top,
/// through magenta to a warm orange-brown at the bottom.
pub const GTA6_STOPS: [Stop; 4] = [
    stop(0.0, 86, 34, 168),
    stop(0.42, 118, 32, 138),
    stop(0.78, 146, 50, 92),
    stop(1.0, 176, 92, 44),
];
pub const OCEAN_STOPS: [Stop; 3] = [
    stop(0.0, 8, 20, 52),
    stop(0.55, 10, 62, 104),
    stop(1.0, 12, 110, 120),
];

pub const PRESETS: [(&str, &str); 5] = [
    ("dark", "Тёмная"),
    ("midnight", "Полночь"),
    ("gta6", "GTA VI"),
    ("ocean", "Океан"),
    ("custom", "Своя"),
];

/// What gets saved in the settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
    pub preset: String,
    /// "Custom": gradient keys
    pub stops: Vec<Stop>,
    pub angle: f32,
    pub accent: [u8; 3],
    pub wave: [u8; 3],
    /// panel darkening over the gradient, 0..0.8
    pub shade: f32,
    pub opacity: f32,
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            preset: "dark".into(),
            stops: GTA6_STOPS.to_vec(),
            angle: 180.0,
            accent: [255, 79, 163],
            wave: [41, 211, 255],
            shade: 0.3,
            opacity: 1.0,
        }
    }
}

/// Gradient color at point t (keys can come in any order).
pub fn sample(stops: &[Stop], t: f32) -> Color32 {
    let mut s: Vec<Stop> = stops.to_vec();
    s.sort_by(|a, b| a.pos.total_cmp(&b.pos));
    let c = |a: [u8; 3]| rgb(a[0], a[1], a[2]);
    match s.len() {
        0 => rgb(18, 18, 18),
        1 => c(s[0].color),
        _ => {
            if t <= s[0].pos {
                return c(s[0].color);
            }
            for w in s.windows(2) {
                if t <= w[1].pos {
                    let span = (w[1].pos - w[0].pos).max(1e-4);
                    return lerp(c(w[0].color), c(w[1].color), (t - w[0].pos) / span);
                }
            }
            c(s[s.len() - 1].color)
        }
    }
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()))
}

/// Palette gradient color at point t (from the N stored points).
pub fn grad_at(g: &[Color32; N], t: f32) -> Color32 {
    let x = t.clamp(0.0, 1.0) * (N - 1) as f32;
    let i = (x.floor() as usize).min(N - 2);
    lerp(g[i], g[i + 1], x - i as f32)
}

fn gradient_palette(stops: &[Stop], angle: f32, accent: [u8; 3], wave: [u8; 3], shade: f32) -> Palette {
    let mut grad = [rgb(0, 0, 0); N];
    for (i, g) in grad.iter_mut().enumerate() {
        *g = sample(stops, i as f32 / (N - 1) as f32);
    }
    // windows and fields: the darkened average background color, always opaque
    let avg = |k: usize| (grad.iter().map(|c| [c.r(), c.g(), c.b()][k] as u32).sum::<u32>() / N as u32) as u8;
    let dark = |v: u8| (v as f32 * 0.45) as u8;
    let shade = shade.clamp(0.0, 0.8);
    Palette {
        grad,
        angle,
        surface: Color32::from_black_alpha((shade * 200.0) as u8),
        hover: Color32::from_white_alpha(16),
        bar: Color32::from_black_alpha((shade * 255.0 * 0.9 + 20.0).min(230.0) as u8),
        text: rgb(255, 248, 252),
        muted: rgb(224, 204, 226),
        accent: rgb(accent[0], accent[1], accent[2]),
        wave: rgb(wave[0], wave[1], wave[2]),
        window: rgb(dark(avg(0)), dark(avg(1)), dark(avg(2))),
        field: Color32::from_black_alpha(110),
        opacity: 1.0,
    }
}

impl ThemeSettings {
    pub fn palette(&self) -> Palette {
        let mut p = match self.preset.as_str() {
            "midnight" => MIDNIGHT,
            "gta6" => gradient_palette(&GTA6_STOPS, 180.0, [255, 79, 163], [41, 211, 255], 0.3),
            "ocean" => gradient_palette(&OCEAN_STOPS, 160.0, [61, 224, 255], [140, 120, 255], 0.3),
            "custom" => gradient_palette(&self.stops, self.angle, self.accent, self.wave, self.shade),
            _ => DARK,
        };
        p.opacity = self.opacity.clamp(0.3, 1.0);
        p
    }

    /// Keys of the current theme, so "Custom" starts from what is selected now.
    pub fn stops_of(preset: &str) -> Vec<Stop> {
        match preset {
            "gta6" => GTA6_STOPS.to_vec(),
            "ocean" => OCEAN_STOPS.to_vec(),
            "midnight" => vec![stop(0.0, 0, 0, 0), stop(1.0, 20, 20, 24)],
            _ => vec![stop(0.0, 18, 18, 18), stop(1.0, 40, 40, 46)],
        }
    }

    /// Preview for a theme card.
    pub fn preview(preset: &str, current: &ThemeSettings) -> Palette {
        ThemeSettings {
            preset: preset.into(),
            ..current.clone()
        }
        .palette()
    }
}

fn lerp_palette(a: &Palette, b: &Palette, t: f32) -> Palette {
    let mut grad = a.grad;
    for (i, g) in grad.iter_mut().enumerate() {
        *g = lerp(a.grad[i], b.grad[i], t);
    }
    let mut da = (b.angle - a.angle) % 360.0;
    if da > 180.0 {
        da -= 360.0;
    } else if da < -180.0 {
        da += 360.0;
    }
    Palette {
        grad,
        angle: a.angle + da * t,
        surface: lerp(a.surface, b.surface, t),
        hover: lerp(a.hover, b.hover, t),
        bar: lerp(a.bar, b.bar, t),
        text: lerp(a.text, b.text, t),
        muted: lerp(a.muted, b.muted, t),
        accent: lerp(a.accent, b.accent, t),
        wave: lerp(a.wave, b.wave, t),
        window: lerp(a.window, b.window, t),
        field: lerp(a.field, b.field, t),
        opacity: a.opacity + (b.opacity - a.opacity) * t,
    }
}

struct State {
    shown: Palette,
    from: Palette,
    to: Palette,
    start: Option<Instant>,
}

static STATE: RwLock<State> = RwLock::new(State {
    shown: DARK,
    from: DARK,
    to: DARK,
    start: None,
});

/// The current (possibly animating) palette.
pub fn pal() -> Palette {
    STATE.read().unwrap().shown
}

/// Immediately, without animation (at startup).
pub fn set(p: Palette) {
    let mut s = STATE.write().unwrap();
    *s = State {
        shown: p,
        from: p,
        to: p,
        start: None,
    };
}

/// Smoothly transition to a new palette.
pub fn transition_to(p: Palette, instant: bool) {
    let mut s = STATE.write().unwrap();
    if instant {
        *s = State {
            shown: p,
            from: p,
            to: p,
            start: None,
        };
    } else {
        s.from = s.shown;
        s.to = p;
        s.start = Some(Instant::now());
    }
}

/// Advance the animation. Returns true while the transition runs (needs repainting).
pub fn tick() -> bool {
    let mut s = STATE.write().unwrap();
    let Some(start) = s.start else { return false };
    let t = (start.elapsed().as_secs_f32() / 0.45).min(1.0);
    let e = 1.0 - (1.0 - t).powi(3);
    s.shown = lerp_palette(&s.from, &s.to, e);
    if t >= 1.0 {
        s.shown = s.to;
        s.start = None;
    }
    true
}

// =====================================================================
// Theme codes: a whole custom theme is packed into a short code for sharing,
// e.g. 1B4ZK-9SDKF-8123K-… No server needed: the code is the theme.
// Crockford Base32 alphabet: no I, L, O, U, so they can't be confused with 1 and 0.
// =====================================================================

const ALPH: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const CODE_VERSION: u8 = 1;

fn crc8(data: &[u8]) -> u8 {
    let mut crc: u8 = 0;
    for &b in data {
        crc ^= b;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 {
                (crc << 1) ^ 0x07
            } else {
                crc << 1
            };
        }
    }
    crc
}

fn b32_encode(bytes: &[u8]) -> String {
    let (mut out, mut acc, mut bits) = (String::new(), 0u32, 0u32);
    for &b in bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPH[((acc >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPH[((acc << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn b32_decode(s: &str) -> Option<Vec<u8>> {
    let (mut out, mut acc, mut bits) = (Vec::new(), 0u32, 0u32);
    for c in s.chars() {
        let c = match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        };
        let v = ALPH.iter().position(|&a| a as char == c)? as u32;
        acc = (acc << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xFF) as u8);
        }
    }
    Some(out)
}

impl ThemeSettings {
    /// Any theme (built-in too) as "Custom", so it can be encoded and tweaked.
    pub fn to_custom(&self) -> ThemeSettings {
        if self.preset == "custom" {
            return self.clone();
        }
        let p = self.palette();
        let rgb = |c: Color32| [c.r(), c.g(), c.b()];
        ThemeSettings {
            preset: "custom".into(),
            stops: ThemeSettings::stops_of(&self.preset),
            angle: p.angle,
            accent: rgb(p.accent),
            wave: rgb(p.wave),
            shade: if matches!(self.preset.as_str(), "dark" | "midnight") {
                0.6
            } else {
                0.3
            },
            opacity: self.opacity,
        }
    }

    /// Theme code for sharing: groups of 5 characters separated by dashes.
    pub fn to_code(&self) -> String {
        let t = self.to_custom();
        let mut stops = t.stops.clone();
        stops.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        stops.truncate(12);
        let mut b = vec![CODE_VERSION, stops.len() as u8];
        for s in &stops {
            b.push((s.pos.clamp(0.0, 1.0) * 255.0).round() as u8);
            b.extend_from_slice(&s.color);
        }
        let angle = (t.angle.rem_euclid(360.0)).round() as u16;
        b.extend_from_slice(&angle.to_be_bytes());
        b.extend_from_slice(&t.accent);
        b.extend_from_slice(&t.wave);
        b.push((t.shade.clamp(0.0, 1.0) * 255.0).round() as u8);
        b.push((t.opacity.clamp(0.0, 1.0) * 255.0).round() as u8);
        b.push(crc8(&b));
        let raw = b32_encode(&b);
        raw.as_bytes()
            .chunks(5)
            .map(|c| String::from_utf8_lossy(c).to_string())
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Parse a code. Error if the code is damaged or has a typo.
    pub fn from_code(code: &str) -> Result<ThemeSettings, ()> {
        let clean: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let b = b32_decode(&clean).ok_or(())?;
        if b.len() < 3 || b[0] != CODE_VERSION {
            return Err(());
        }
        let n = b[1] as usize;
        let need = 2 + n * 4 + 2 + 3 + 3 + 1 + 1;
        if n < 1 || b.len() < need + 1 || crc8(&b[..need]) != b[need] {
            return Err(());
        }
        let mut i = 2;
        let mut stops = Vec::new();
        for _ in 0..n {
            stops.push(Stop {
                pos: b[i] as f32 / 255.0,
                color: [b[i + 1], b[i + 2], b[i + 3]],
            });
            i += 4;
        }
        let angle = u16::from_be_bytes([b[i], b[i + 1]]) as f32;
        i += 2;
        let accent = [b[i], b[i + 1], b[i + 2]];
        let wave = [b[i + 3], b[i + 4], b[i + 5]];
        let shade = b[i + 6] as f32 / 255.0;
        let opacity = (b[i + 7] as f32 / 255.0).clamp(0.3, 1.0);
        Ok(ThemeSettings {
            preset: "custom".into(),
            stops,
            angle,
            accent,
            wave,
            shade,
            opacity,
        })
    }
}

#[cfg(test)]
mod code_tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let t = ThemeSettings {
            preset: "gta6".into(),
            ..Default::default()
        };
        let code = t.to_code();
        let back = ThemeSettings::from_code(&code).unwrap();
        assert_eq!(back.stops.len(), GTA6_STOPS.len());
        assert_eq!(back.to_code(), code);
        // a typo is caught by the checksum
        let mut bad: Vec<char> = code.chars().collect();
        bad[3] = if bad[3] == 'A' { 'B' } else { 'A' };
        assert!(ThemeSettings::from_code(&bad.into_iter().collect::<String>()).is_err());
    }
}

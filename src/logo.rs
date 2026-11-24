//! SCloud logo: a cloud with an equalizer. The same drawing is used in the header and as the window icon.

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

/// Geometry in a unit square: cloud circles, the "body", the cut-out bars.
struct Geo {
    circles: [(f32, f32, f32); 3],
    body: (f32, f32, f32, f32, f32), // x0, y0, x1, y1, corner radius
    bars: [(f32, f32); 4],           // x center, height (y center = BAR_Y)
}

const BAR_Y: f32 = 0.63;
const BAR_W: f32 = 0.065;

const GEO: Geo = Geo {
    circles: [(0.30, 0.55, 0.17), (0.53, 0.42, 0.25), (0.76, 0.56, 0.155)],
    body: (0.08, 0.50, 0.93, 0.84, 0.17),
    bars: [(0.31, 0.10), (0.43, 0.22), (0.55, 0.16), (0.67, 0.09)],
};

/// Draw the mark in a (square) rect. `fill` is the cloud, `cut` the bars.
/// `phase` is the time in seconds while music plays: the equalizer bars "jump".
pub fn paint(painter: &egui::Painter, rect: Rect, fill: Color32, cut: Color32, phase: Option<f32>) {
    let s = rect.width().min(rect.height());
    let o = rect.center() - Vec2::splat(s / 2.0);
    let at = |x: f32, y: f32| o + Vec2::new(x * s, y * s);
    for (x, y, r) in GEO.circles {
        painter.circle_filled(at(x, y), r * s, fill);
    }
    let (x0, y0, x1, y1, rr) = GEO.body;
    painter.rect_filled(Rect::from_min_max(at(x0, y0), at(x1, y1)), rr * s, fill);
    for (i, (x, h)) in GEO.bars.into_iter().enumerate() {
        let h = match phase {
            Some(t) => 0.08 + 0.16 * (t * 6.0 + [0.0, 1.1, 2.3, 3.4][i]).sin().abs(),
            None => h,
        };
        let r = Rect::from_center_size(at(x, BAR_Y), Vec2::new(BAR_W * s, h * s));
        painter.rect_filled(r, BAR_W * s / 2.0, cut);
    }
}

/// Mark + wordmark in the header. Returns the response (click = go home).
pub fn header(
    ui: &mut egui::Ui,
    accent: Color32,
    text: Color32,
    ink: Color32,
    font: egui::FontId,
    playing: bool,
) -> egui::Response {
    let galley = ui.painter().layout_no_wrap("SCloud".into(), font, text);
    let mark = 30.0;
    let size = Vec2::new(mark + 8.0 + galley.size().x, mark.max(galley.size().y));
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    let h = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.2);
    // the mark "hops" and tilts slightly on hover
    let mark_rect = Rect::from_center_size(
        Pos2::new(rect.left() + mark / 2.0, rect.center().y - 2.0 * h),
        Vec2::splat(mark * (1.0 + 0.1 * h)),
    );
    let phase = playing.then(|| ui.input(|i| i.time) as f32);
    if playing {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(40));
    }
    paint(ui.painter(), mark_rect, accent, ink, phase);
    ui.painter().galley(
        Pos2::new(rect.left() + mark + 8.0, rect.center().y - galley.size().y / 2.0),
        galley,
        text,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Number of icon animation frames (0 = static, 1..=FRAMES = the equalizer "jumps").
pub const FRAMES: usize = 8;
const ICON: usize = 64;

/// Theme-colored icon: a tile with the theme gradient, a white cloud, bars "cut out" to the background.
/// We render it ourselves (4×4 samples per pixel) so the icon always matches the chosen theme.
pub fn render(grad: &[Color32], angle_deg: f32, frame: usize) -> Vec<u8> {
    let n = ICON as f32;
    let (sa, ca) = (angle_deg.to_radians().sin(), -angle_deg.to_radians().cos());
    // CSS angle: 180° = top to bottom; projection onto the gradient direction
    let span = 0.5 * (sa.abs() + ca.abs());
    let grad_at = |x: f32, y: f32| {
        let t = ((x - 0.5) * sa + (y - 0.5) * ca) / (2.0 * span.max(1e-3)) + 0.5;
        let x = t.clamp(0.0, 1.0) * (grad.len() - 1) as f32;
        let i = (x.floor() as usize).min(grad.len().saturating_sub(2));
        let f = x - i as f32;
        let (a, b) = (grad[i], grad[(i + 1).min(grad.len() - 1)]);
        [
            a.r() as f32 + (b.r() as f32 - a.r() as f32) * f,
            a.g() as f32 + (b.g() as f32 - a.g() as f32) * f,
            a.b() as f32 + (b.b() as f32 - a.b() as f32) * f,
        ]
    };
    let bars: Vec<f32> = (0..4)
        .map(|i| {
            if frame == 0 {
                GEO.bars[i].1
            } else {
                let t = (frame - 1) as f32 / FRAMES as f32 * std::f32::consts::PI;
                0.08 + 0.16 * (t + [0.0, 1.1, 2.3, 3.4][i]).sin().abs()
            }
        })
        .collect();
    // cloud inside the tile: scale 0.78, centered
    let in_cloud = |px: f32, py: f32| -> bool {
        let x = 0.5 + (px - 0.5) / 0.78;
        let y = 0.505 + (py - 0.5) / 0.78;
        let (x0, y0, x1, y1, rr) = GEO.body;
        let body = {
            // max/min instead of clamp: for the "body" y0 + rr and y1 - rr are equal, and due to rounding
            // the lower bound can end up slightly above the upper one, and clamp panics on that
            let cx = x.max(x0 + rr).min((x1 - rr).max(x0 + rr));
            let cy = y.max(y0 + rr).min((y1 - rr).max(y0 + rr));
            (x - cx).powi(2) + (y - cy).powi(2) <= rr * rr
        };
        let cloud = body || GEO.circles.iter().any(|&(cx, cy, r)| (x - cx).powi(2) + (y - cy).powi(2) <= r * r);
        if !cloud {
            return false;
        }
        // cut-out bars
        !GEO.bars.iter().zip(&bars).any(|(&(bx, _), &h)| {
            let hw = BAR_W / 2.0;
            let hh = h / 2.0;
            let dx = (x - bx).abs();
            let dy = (y - BAR_Y).abs();
            if dy <= hh - hw {
                dx <= hw
            } else {
                // rounded ends
                let ey = dy - (hh - hw);
                dx * dx + ey * ey <= hw * hw && dy <= hh
            }
        })
    };
    let radius = 0.23;
    let in_tile = |x: f32, y: f32| {
        let cx = x.max(radius).min(1.0 - radius);
        let cy = y.max(radius).min(1.0 - radius);
        (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
    };
    let mut out = vec![0u8; ICON * ICON * 4];
    const SS: usize = 4;
    for py in 0..ICON {
        for px in 0..ICON {
            let (mut r, mut g, mut b, mut a) = (0.0, 0.0, 0.0, 0.0);
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / n;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / n;
                    if !in_tile(x, y) {
                        continue;
                    }
                    let c = if in_cloud(x, y) { [255.0, 255.0, 255.0] } else { grad_at(x, y) };
                    r += c[0];
                    g += c[1];
                    b += c[2];
                    a += 1.0;
                }
            }
            let i = (py * ICON + px) * 4;
            if a > 0.0 {
                // non-premultiplied RGBA
                out[i] = (r / a) as u8;
                out[i + 1] = (g / a) as u8;
                out[i + 2] = (b / a) as u8;
                out[i + 3] = (a / (SS * SS) as f32 * 255.0) as u8;
            }
        }
    }
    out
}

struct Icons {
    key: u64,
    data: Vec<std::sync::Arc<egui::IconData>>,
    #[cfg(windows)]
    hicons: Vec<usize>,
}

static ICONS: std::sync::Mutex<Option<Icons>> = std::sync::Mutex::new(None);
static PENDING: std::sync::Mutex<Option<(u64, std::time::Instant)>> = std::sync::Mutex::new(None);

/// The theme changed but the icon hasn't been re-rendered yet (waiting for a pause).
pub fn retheme_pending() -> bool {
    PENDING.lock().map(|p| p.is_some()).unwrap_or(false)
}

fn theme_key(grad: &[Color32], angle: f32) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for c in grad {
        c.to_array().hash(&mut h);
    }
    (angle as i32).hash(&mut h);
    h.finish()
}

/// Re-render the icons for the theme (if it changed). true = the icons were updated.
pub fn retheme(grad: &[Color32], angle: f32) -> bool {
    let key = theme_key(grad, angle);
    let mut g = ICONS.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_some_and(|i| i.key == key) {
        PENDING.lock().map(|mut p| *p = None).ok();
        return false;
    }
    // while a color is being dragged in the editor, wait instead of re-rendering the icon every frame
    if g.is_some() {
        let mut p = PENDING.lock().unwrap_or_else(|e| e.into_inner());
        match *p {
            Some((k, at)) if k == key && at.elapsed().as_millis() >= 400 => *p = None,
            Some((k, _)) if k == key => return false,
            _ => {
                *p = Some((key, std::time::Instant::now()));
                return false;
            }
        }
    }
    let data: Vec<_> = (0..=FRAMES)
        .map(|f| {
            std::sync::Arc::new(egui::IconData { rgba: render(grad, angle, f), width: ICON as u32, height: ICON as u32 })
        })
        .collect();
    #[cfg(windows)]
    let hicons = data.iter().map(|d| make_hicon(&d.rgba)).collect();
    // old HICONs aren't destroyed right away: the window may still be showing them
    let old = g.replace(Icons {
        key,
        data,
        #[cfg(windows)]
        hicons,
    });
    #[cfg(windows)]
    if let Some(old) = old {
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            for h in old.hicons {
                if h != 0 {
                    unsafe {
                        windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon(h as _);
                    }
                }
            }
        });
    }
    #[cfg(not(windows))]
    let _ = old;
    true
}

/// Window icon frames for egui. Until the first recolor, the stock one (from assets).
pub fn frames() -> Vec<std::sync::Arc<egui::IconData>> {
    if let Some(i) = ICONS.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return i.data.clone();
    }
    vec![std::sync::Arc::new(
        eframe::icon_data::from_png_bytes(include_bytes!("../assets/scloud-64.png")).unwrap_or_default(),
    )]
}

/// HICON from RGBA: the "icon resource" format (BITMAPINFOHEADER + bottom-up BGRA + AND mask).
#[cfg(windows)]
fn make_hicon(rgba: &[u8]) -> usize {
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, LR_DEFAULTCOLOR};
    let n = ICON;
    let mut buf = Vec::with_capacity(40 + n * n * 4 + n * 8);
    let push32 = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
    push32(&mut buf, 40); // biSize
    push32(&mut buf, n as u32); // width
    push32(&mut buf, (n * 2) as u32); // height ×2 (XOR + AND)
    buf.extend_from_slice(&1u16.to_le_bytes()); // planes
    buf.extend_from_slice(&32u16.to_le_bytes()); // bpp
    push32(&mut buf, 0); // BI_RGB
    push32(&mut buf, (n * n * 4) as u32);
    for _ in 0..4 {
        push32(&mut buf, 0);
    }
    for y in (0..n).rev() {
        for x in 0..n {
            let i = (y * n + x) * 4;
            buf.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    // AND mask: all zeros (transparency comes from alpha), rows padded to 4 bytes
    buf.extend(std::iter::repeat(0u8).take(n * ((n + 31) / 32) * 4));
    unsafe {
        CreateIconFromResourceEx(buf.as_ptr(), buf.len() as u32, 1, 0x0003_0000, n as i32, n as i32, LR_DEFAULTCOLOR)
            as usize
    }
}

/// Windows: set frame `idx` both on the taskbar (big icon) and in the title (small icon).
/// The stock egui command only changes the small one, which is why the taskbar didn't animate.
#[cfg(windows)]
pub fn set_taskbar_icon(frame: &eframe::Frame, idx: usize) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SendMessageW, ICON_BIG, ICON_SMALL, WM_SETICON};
    let icon = {
        let g = ICONS.lock().unwrap_or_else(|e| e.into_inner());
        let Some(i) = g.as_ref() else { return };
        let Some(&h) = i.hicons.get(idx) else { return };
        h
    };
    if icon == 0 {
        return;
    }
    let Ok(handle) = frame.window_handle() else { return };
    if let RawWindowHandle::Win32(w) = handle.as_raw() {
        let hwnd = w.hwnd.get() as *mut core::ffi::c_void;
        unsafe {
            SendMessageW(hwnd as _, WM_SETICON, ICON_BIG as usize, icon as isize);
            SendMessageW(hwnd as _, WM_SETICON, ICON_SMALL as usize, icon as isize);
        }
    }
}

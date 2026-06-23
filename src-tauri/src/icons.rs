use resvg::tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Transform};
use resvg::usvg;
use tauri::image::Image;

const LOGO_SVG: &str = include_str!("../assets/claude-logo.svg");
const CLAY: &str = "#D97757";

/// Rendered hi-res; Windows scales it down for the tray, keeping edges crisp.
pub const ICON_SIZE: u32 = 64;
/// Frames in the "working" claw-rotation animation.
pub const FRAME_COUNT: usize = 12;
/// Neutral fill for the initial icon before the first usage fetch.
pub const NEUTRAL: &str = CLAY;

/// Status-badge color by utilization: clay (normal) → amber (high) → red (critical).
pub fn color_for_percent(p: f64) -> &'static str {
    if p >= 90.0 {
        "#EF6248"
    } else if p >= 70.0 {
        "#E8A23D"
    } else {
        "#5BD17A" // healthy: a clear green dot reads well at tray size
    }
}

fn hex(c: &str) -> (u8, u8, u8) {
    let c = c.trim_start_matches('#');
    let n = u32::from_str_radix(c, 16).unwrap_or(0xD9_77_57);
    (((n >> 16) & 0xff) as u8, ((n >> 8) & 0xff) as u8, (n & 0xff) as u8)
}

fn fill_circle(pm: &mut Pixmap, cx: f32, cy: f32, r: f32, color: &str, alpha: u8) {
    let mut pb = PathBuilder::new();
    pb.push_circle(cx, cy, r);
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        let (rr, gg, bb) = hex(color);
        paint.set_color_rgba8(rr, gg, bb, alpha);
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

/// Render the tray icon: a large Claude claw (clay), plus a small status badge
/// dot in the corner (`badge_color`) so it reads as a *monitor* and shows
/// severity at a glance. `claw_angle` rotates the claw (working animation).
fn render(size: u32, badge_color: &str, claw_angle: f32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(size, size)?;
    let s = size as f32;
    let c = s / 2.0;

    // Big, recognizable claw.
    if let Ok(tree) = usvg::Tree::from_str(LOGO_SVG, &usvg::Options::default()) {
        let ts = tree.size();
        let scale = (s * 0.98) / ts.width().max(ts.height());
        let tx = c - ts.width() * scale / 2.0;
        let ty = c - ts.height() * scale / 2.0;
        let place = Transform::from_translate(tx, ty).pre_scale(scale, scale);
        let transform = if claw_angle.abs() > f32::EPSILON {
            Transform::from_rotate_at(claw_angle, c, c).pre_concat(place)
        } else {
            place
        };
        resvg::render(&tree, transform, &mut pm.as_mut());
    }

    // Status badge: dark separator ring (so it reads over the claw) + color dot.
    let bx = s * 0.74;
    let by = s * 0.74;
    fill_circle(&mut pm, bx, by, s * 0.26, "#1A1917", 255);
    fill_circle(&mut pm, bx, by, s * 0.18, badge_color, 255);

    Some(pm)
}

fn to_image(pm: &Pixmap) -> Image<'static> {
    let (w, h) = (pm.width(), pm.height());
    let mut rgba: Vec<u8> = Vec::with_capacity((w * h * 4) as usize);
    for px in pm.pixels() {
        let c = px.demultiply();
        rgba.push(c.red());
        rgba.push(c.green());
        rgba.push(c.blue());
        rgba.push(c.alpha());
    }
    Image::new_owned(rgba, w, h)
}

fn blank() -> Image<'static> {
    Image::new_owned(vec![0u8; (ICON_SIZE * ICON_SIZE * 4) as usize], ICON_SIZE, ICON_SIZE)
}

/// Static tray icon: large claw + status badge dot in `color`.
pub fn idle_icon(color: &str, _percent: f64) -> Image<'static> {
    render(ICON_SIZE, color, 0.0)
        .map(|p| to_image(&p))
        .unwrap_or_else(blank)
}

/// Rotation frames for the "working" animation (the claw spins).
pub fn working_frames(color: &str, _percent: f64) -> Vec<Image<'static>> {
    (0..FRAME_COUNT)
        .map(|i| {
            let angle = (i as f32) * (360.0 / FRAME_COUNT as f32);
            render(ICON_SIZE, color, angle)
                .map(|p| to_image(&p))
                .unwrap_or_else(|| idle_icon(color, 0.0))
        })
        .collect()
}

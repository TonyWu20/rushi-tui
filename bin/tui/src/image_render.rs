//! Image rendering for tool results (docs/NEW-refactor.md must-do 3).
//!
//! The kernel sends image data in tool-result values:
//! `value.details = {type: "image", mime_type, data(base64), path}`.
//! This module detects that shape, decodes the image, and converts it
//! to half-block Unicode lines that fit the transcript's `Vec<Line>`
//! model. No layout changes are needed: the image scrolls with the
//! transcript like any other body row.
//!
//! The half-block conversion uses the `image` crate for pixel access
//! and maps pixel pairs to Unicode half-block characters with
//! foreground/background colors.

use ratatui::style::{Color, Modifier, Style};

use crate::tool_display::BodyRow;

/// The decode + render entry point. Checks whether `value` carries
/// image data (`details.type == "image"` with a base64 `data` field,
/// or a top-level `type: "image"`). If so, decodes and renders to
/// half-block lines; otherwise returns `None`.
///
/// `width` is the usable body width in cells; the image is downscaled
/// to fit. The result is a `Vec<BodyRow>` that can be used as a tool
/// result body, with a header line describing the image followed by
/// the half-block pixel rows.
pub fn image_body_rows(
    value: &serde_json::Value,
    width: usize,
) -> Option<Vec<BodyRow>> {
    let details = value.get("details").unwrap_or(value);
    let is_image = details.get("type").and_then(|t| t.as_str()) == Some("image");
    if !is_image {
        return None;
    }
    // The base64 payload: `data` in the details (kernel shape) or at
    // the top level of the value.
    let data_b64 = details
        .get("data")
        .or_else(|| value.get("data"))
        .and_then(|d| d.as_str())
        .unwrap_or("");
    if data_b64.is_empty() {
        return None;
    }
    // Decode the base64 payload.
    use base64::Engine;
    let bytes = match base64::engine::general_purpose::STANDARD.decode(data_b64) {
        Ok(b) => b,
        Err(_) => return None,
    };
    let img = match image::load_from_memory(&bytes) {
        Ok(i) => i,
        Err(_) => return None,
    };
    let mime = details
        .get("mime_type")
        .and_then(|m| m.as_str())
        .unwrap_or("image")
        .to_string();
    let path = details
        .get("path")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string());
    let width_px = img.width();
    let height_px = img.height();

    // Compute the target cell size: fit the image into the body width,
    // cap the height so a huge image does not eat the viewport.
    let max_h = 12u32;
    let target_w = width.clamp(4, 80) as u32;
    let aspect = height_px as f64 / width_px.max(1) as f64;
    let target_h = ((target_w as f64 * aspect).min(max_h as f64)) as u32;
    let target_h = target_h.max(1);

    // Downscale the image to target dimensions.
    let scaled = img.resize(target_w, target_h, image::imageops::FilterType::Lanczos3);
    let rgba = scaled.to_rgba8();
    // Use actual dimensions (resize may clamp).
    let cell_w = rgba.width() as usize;
    let actual_h = rgba.height() as usize;
    let cell_h = actual_h.div_ceil(2);

    let mut lines: Vec<BodyRow> = Vec::with_capacity(cell_h);
    for row in 0..cell_h {
        let y_top = (row * 2).min(actual_h - 1);
        let y_bot = (row * 2 + 1).min(actual_h - 1);
        let mut row_spans: BodyRow = Vec::with_capacity(cell_w / 2 + 1);
        for x in 0..cell_w {
            let px_top = rgba.get_pixel(x as u32, y_top as u32);
            let px_bot = rgba.get_pixel(x as u32, y_bot as u32);
            let (ch, fg, bg) = halfblock_char(px_top, px_bot);
            let style = Style::default().fg(fg).bg(bg);
            if row_spans.last().is_some_and(|(s, _)| *s == style) {
                // Extend the last span.
                let last_span = row_spans.last_mut().unwrap();
                last_span.1.push(ch);
            } else {
                row_spans.push((style, ch.to_string()));
            }
        }
        lines.push(row_spans);
    }

    let label = if let Some(ref p) = path {
        format!(
            "image {}x{} {mime}  {p}",
            width_px, height_px
        )
    } else {
        format!("image {}x{} {mime}", width_px, height_px)
    };
    let header: BodyRow = vec![(
        Style::default()
            .add_modifier(Modifier::DIM)
            .add_modifier(Modifier::ITALIC),
        label,
    )];
    let mut rows = vec![header];
    rows.extend(lines);
    Some(rows)
}

/// Map a top/bottom pixel pair to a half-block character and colors.
///
/// - Both pixels dark: space with bg color.
/// - Both pixels bright: full block `█`.
/// - Top is brighter: upper half block `▀`, fg=top, bg=bottom.
/// - Bottom is brighter: lower half block `▄`, fg=bottom, bg=top.
fn halfblock_char(
    top: &image::Rgba<u8>,
    bot: &image::Rgba<u8>,
) -> (char, Color, Color) {
    let top_lum = luminance(top);
    let bot_lum = luminance(bot);
    let top_color = Color::Rgb(top[0], top[1], top[2]);
    let bot_color = Color::Rgb(bot[0], bot[1], bot[2]);

    if top_lum < 32 && bot_lum < 32 {
        (' ', bot_color, bot_color)
    } else if top_lum > 220 && bot_lum > 220 {
        ('\u{2588}', top_color, top_color)
    } else if top_lum >= bot_lum {
        ('\u{2580}', top_color, bot_color)
    } else {
        ('\u{2584}', bot_color, top_color)
    }
}

/// Compute the perceived luminance of an RGBA pixel (0-255).
fn luminance(px: &image::Rgba<u8>) -> u32 {
    // Rec. 709 luma coefficients, scaled to 0-255.
    (px[0] as u32 * 299 + px[1] as u32 * 587 + px[2] as u32 * 114) / 1000
}


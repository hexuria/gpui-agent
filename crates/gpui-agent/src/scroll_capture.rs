//! Semantic scroll-and-stitch helpers for `screenshot` `mode=scrolled`.
//!
//! The host (not OS HID, not virtual wheel) resolves a named scroll
//! target, reads metrics, sets offset, waits for paint, captures tiles,
//! stitches, and restores the original offset. GPUI `ScrollHandle` stays
//! in the app; this crate stays `gpui-kit`-free.

use crate::tree::Bounds;
use crate::{DispatchResult, screenshot_unavailable};

/// Safety cap when the client omits `max_height_px`. Also the maximum
/// allowed client cap — larger values fail closed (not a silent clamp).
pub const DEFAULT_MAX_HEIGHT_PX: u32 = 16_384;

/// Fail closed rather than stitching an unbounded number of window grabs.
pub const MAX_SCROLL_TILES: u32 = 32;

/// Refuse a stitched PNG bigger than this (decoded height is already capped).
pub const MAX_SCROLLED_PNG_BYTES: usize = 32 * 1024 * 1024;

/// Stable error prefix when the host cannot resolve or drive `target`.
pub const SCROLL_UNAVAILABLE: &str = "scroll_unavailable";

pub fn scroll_unavailable(detail: impl Into<String>) -> String {
    format!("{SCROLL_UNAVAILABLE}: {}", detail.into())
}

pub fn is_scroll_unavailable(error: &str) -> bool {
    error == SCROLL_UNAVAILABLE
        || error.starts_with(SCROLL_UNAVAILABLE)
            && error.as_bytes().get(SCROLL_UNAVAILABLE.len()) == Some(&b':')
}

/// Borrowed `screenshot` fields after JSON parse.
#[derive(Debug, Clone, Copy)]
pub struct ScreenshotSpec<'a> {
    pub path: Option<&'a str>,
    pub mode: crate::protocol::ScreenshotMode,
    pub target: Option<&'a str>,
    pub max_height_px: Option<u32>,
}

impl<'a> ScreenshotSpec<'a> {
    pub fn from_op(
        path: &'a Option<String>,
        mode: crate::protocol::ScreenshotMode,
        target: &'a Option<String>,
        max_height_px: Option<u32>,
    ) -> Self {
        Self {
            path: path.as_deref(),
            mode,
            target: target.as_deref(),
            max_height_px,
        }
    }

    /// Protocol checks that do not require a pixel surface.
    pub fn validate_request(&self) -> Result<(), String> {
        if self.mode.is_scrolled() {
            match self.target.map(str::trim).filter(|s| !s.is_empty()) {
                Some(_) => {}
                None => return Err("screenshot mode=scrolled requires target".into()),
            }
        }
        if let Some(h) = self.max_height_px {
            if h == 0 {
                return Err("max_height_px must be > 0".into());
            }
            if h > DEFAULT_MAX_HEIGHT_PX {
                return Err(format!(
                    "max_height_px exceeds {DEFAULT_MAX_HEIGHT_PX} (got {h})"
                ));
            }
        }
        Ok(())
    }

    pub fn height_cap(&self) -> u32 {
        self.max_height_px.unwrap_or(DEFAULT_MAX_HEIGHT_PX)
    }

    pub fn scrolled_target(&self) -> Result<&'a str, String> {
        match self.target.map(str::trim).filter(|s| !s.is_empty()) {
            Some(target) => Ok(target),
            None => Err("screenshot mode=scrolled requires target".into()),
        }
    }
}

/// Scroll container metrics in **logical pixels**, content-down.
///
/// `offset_y` is **positive down** (0 = top). Hosts that use GPUI's
/// `ScrollHandle` convert `offset().y` (negative when scrolled down) here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollMetrics {
    pub viewport: Bounds,
    pub content_height: f32,
    pub offset_y: f32,
}

impl ScrollMetrics {
    pub fn max_offset_y(self) -> f32 {
        (self.content_height - self.viewport.h).max(0.0)
    }
}

/// One capture step: scroll to `offset_y`, then crop `skip_top_px` off the
/// viewport and keep `take_height_px` of content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileSpec {
    pub offset_y: f32,
    pub skip_top_px: f32,
    pub take_height_px: f32,
}

/// Plan non-overlapping content slices. Fails closed if content is taller
/// than `max_height_px` or the tile cap would be exceeded.
pub fn plan_scroll_tiles(
    metrics: &ScrollMetrics,
    max_height_px: u32,
) -> Result<Vec<TileSpec>, String> {
    if max_height_px == 0 {
        return Err("max_height_px must be > 0".into());
    }
    if metrics.viewport.h < 1.0 {
        return Err(scroll_unavailable(
            "scroll target viewport height must be >= 1px",
        ));
    }
    if metrics.content_height < 0.0 {
        return Err(scroll_unavailable(
            "scroll target content_height must be >= 0",
        ));
    }
    let content = metrics.content_height.max(metrics.viewport.h);
    if content > max_height_px as f32 {
        return Err(format!(
            "screenshot scrolled exceeds max_height_px (content_height={content}, max_height_px={max_height_px})"
        ));
    }
    let max_offset = (content - metrics.viewport.h).max(0.0);
    let mut tiles = Vec::new();
    let mut covered = 0.0_f32;
    while covered < content - 0.5 {
        if tiles.len() as u32 >= MAX_SCROLL_TILES {
            return Err(format!(
                "screenshot scrolled exceeds {MAX_SCROLL_TILES} tiles"
            ));
        }
        let offset = covered.min(max_offset);
        let skip_top = (covered - offset).max(0.0);
        let remaining = content - covered;
        let take = remaining
            .min(metrics.viewport.h - skip_top)
            .min(metrics.viewport.h);
        if take < 0.5 {
            break;
        }
        tiles.push(TileSpec {
            offset_y: offset,
            skip_top_px: skip_top,
            take_height_px: take,
        });
        covered += take;
    }
    if tiles.is_empty() {
        tiles.push(TileSpec {
            offset_y: 0.0,
            skip_top_px: 0.0,
            take_height_px: metrics.viewport.h.min(content).max(1.0),
        });
    }
    Ok(tiles)
}

/// Decoded 8-bit RGBA bitmap. Not a substitute for a live window grab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl RgbaImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, String> {
        let need = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or_else(|| "rgba image size overflow".to_string())?;
        if pixels.len() != need {
            return Err(format!(
                "rgba buffer length {} != {need} ({width}x{height})",
                pixels.len()
            ));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    fn pixel_index(&self, x: u32, y: u32) -> usize {
        ((y as usize * self.width as usize) + x as usize) * 4
    }
}

/// Decode a PNG into 8-bit RGBA. Does not invent pixels.
pub fn decode_png_rgba(bytes: &[u8]) -> Result<RgbaImage, String> {
    if !bytes.starts_with(b"\x89PNG") {
        return Err(screenshot_unavailable("not a PNG"));
    }
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|err| format!("png decode: {err}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|err| format!("png frame: {err}"))?;
    let buf = &buf[..info.buffer_size()];
    to_rgba(
        info.width,
        info.height,
        info.color_type,
        info.bit_depth,
        buf,
    )
}

fn to_rgba(
    width: u32,
    height: u32,
    color: png::ColorType,
    depth: png::BitDepth,
    buf: &[u8],
) -> Result<RgbaImage, String> {
    if depth != png::BitDepth::Eight {
        return Err(format!("unsupported png bit depth {depth:?}"));
    }
    let n = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| "png size overflow".to_string())?;
    let mut pixels = Vec::with_capacity(n * 4);
    match color {
        png::ColorType::Rgba => {
            if buf.len() != n * 4 {
                return Err("png rgba size mismatch".into());
            }
            pixels.extend_from_slice(buf);
        }
        png::ColorType::Rgb => {
            if buf.len() != n * 3 {
                return Err("png rgb size mismatch".into());
            }
            for chunk in buf.chunks_exact(3) {
                pixels.extend_from_slice(&[chunk[0], chunk[1], chunk[2], 255]);
            }
        }
        png::ColorType::Grayscale => {
            if buf.len() != n {
                return Err("png gray size mismatch".into());
            }
            for &g in buf {
                pixels.extend_from_slice(&[g, g, g, 255]);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            if buf.len() != n * 2 {
                return Err("png gray-alpha size mismatch".into());
            }
            for chunk in buf.chunks_exact(2) {
                pixels.extend_from_slice(&[chunk[0], chunk[0], chunk[0], chunk[1]]);
            }
        }
        other => return Err(format!("unsupported png color type {other:?}")),
    }
    RgbaImage::new(width, height, pixels)
}

/// Encode 8-bit RGBA as PNG.
pub fn encode_png_rgba(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, img.width, img.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|err| format!("png encode header: {err}"))?;
    writer
        .write_image_data(&img.pixels)
        .map_err(|err| format!("png encode: {err}"))?;
    writer
        .finish()
        .map_err(|err| format!("png encode finish: {err}"))?;
    Ok(out)
}

/// Crop `src` to `[x, y, w, h)` in bitmap pixels.
pub fn crop_rgba(src: &RgbaImage, x: u32, y: u32, w: u32, h: u32) -> Result<RgbaImage, String> {
    if w == 0 || h == 0 {
        return Err("crop size must be > 0".into());
    }
    if x.checked_add(w).is_none_or(|r| r > src.width)
        || y.checked_add(h).is_none_or(|b| b > src.height)
    {
        return Err(format!(
            "crop ({x},{y},{w},{h}) outside {}x{}",
            src.width, src.height
        ));
    }
    let mut pixels = Vec::with_capacity(w as usize * h as usize * 4);
    for row in y..y + h {
        let start = src.pixel_index(x, row);
        let end = start + w as usize * 4;
        pixels.extend_from_slice(&src.pixels[start..end]);
    }
    RgbaImage::new(w, h, pixels)
}

/// Concatenate tiles top-to-bottom. Widths must match. No “AI fill”.
pub fn stitch_tiles_vertically(tiles: &[RgbaImage]) -> Result<RgbaImage, String> {
    if tiles.is_empty() {
        return Err("scrolled screenshot produced no tiles".into());
    }
    let width = tiles[0].width;
    if width == 0 {
        return Err("tile width must be > 0".into());
    }
    let mut height: u32 = 0;
    for (i, tile) in tiles.iter().enumerate() {
        if tile.width != width {
            return Err(format!(
                "tile {i} width {} != first tile {width}",
                tile.width
            ));
        }
        if tile.height == 0 {
            return Err(format!("tile {i} height must be > 0"));
        }
        height = height
            .checked_add(tile.height)
            .ok_or_else(|| "stitched height overflow".to_string())?;
    }
    let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
    for tile in tiles {
        pixels.extend_from_slice(&tile.pixels);
    }
    RgbaImage::new(width, height, pixels)
}

/// Map a logical clip rect onto a window PNG.
///
/// If the PNG is taller than `window_h * scale` (typical macOS titlebar),
/// that extra is added to `clip.y`.
pub fn crop_window_png(
    png: &[u8],
    window_w: f32,
    window_h: f32,
    clip: Bounds,
    skip_top_px: f32,
    take_height_px: f32,
) -> Result<RgbaImage, String> {
    let img = decode_png_rgba(png)?;
    if window_w < 1.0 || window_h < 1.0 {
        return Err(screenshot_unavailable(
            "window size is zero; cannot crop a scrolled tile",
        ));
    }
    let scale = img.width as f32 / window_w;
    let extra_top = (img.height as f32 / scale - window_h).max(0.0);
    let x = ((clip.x * scale).round() as i64).max(0) as u32;
    let y = (((clip.y + extra_top + skip_top_px) * scale).round() as i64).max(0) as u32;
    let w = ((clip.w * scale).round() as u32).max(1);
    let h = ((take_height_px * scale).round() as u32).max(1);
    let w = w.min(img.width.saturating_sub(x));
    let h = h.min(img.height.saturating_sub(y));
    if w == 0 || h == 0 {
        return Err(screenshot_unavailable(format!(
            "scroll clip ({},{}) {}x{} is outside {}x{} window png",
            clip.x, clip.y, clip.w, clip.h, img.width, img.height
        )));
    }
    crop_rgba(&img, x, y, w, h)
}

/// Result metadata for a successful scrolled capture (path is on disk).
pub fn scrolled_result_json(
    path: &str,
    target: &str,
    content_height: f32,
    viewport_height: f32,
    tiles: usize,
) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "backend": crate::SCREENSHOT_BACKEND_SCREENCAPTURE,
        "mode": "scrolled",
        "target": target,
        "content_height": content_height,
        "viewport_height": viewport_height,
        "tiles": tiles,
    })
}

pub fn scrolled_dispatch_result(
    path: &str,
    target: &str,
    content_height: f32,
    viewport_height: f32,
    tiles: usize,
) -> DispatchResult {
    DispatchResult::json(scrolled_result_json(
        path,
        target,
        content_height,
        viewport_height,
        tiles,
    ))
}

/// Drive scroll → wait → capture → stitch → **always restore** `original`
/// offset. `wait_paint` may be a no-op in unit tests.
pub fn run_scrolled_capture_sync(
    target: &str,
    metrics: ScrollMetrics,
    mut set_offset: impl FnMut(f32) -> Result<(), String>,
    mut wait_paint: impl FnMut() -> Result<(), String>,
    mut capture_window_png: impl FnMut() -> Result<Vec<u8>, String>,
    window_size: (f32, f32),
    max_height_px: u32,
) -> Result<(Vec<u8>, serde_json::Value), String> {
    let original = metrics.offset_y;
    let run = (|| {
        let tiles = plan_scroll_tiles(&metrics, max_height_px)?;
        let mut slices = Vec::with_capacity(tiles.len());
        for spec in &tiles {
            set_offset(spec.offset_y)?;
            wait_paint()?;
            let png = capture_window_png()?;
            let slice = crop_window_png(
                &png,
                window_size.0,
                window_size.1,
                metrics.viewport,
                spec.skip_top_px,
                spec.take_height_px,
            )?;
            slices.push(slice);
        }
        let stitched = stitch_tiles_vertically(&slices)?;
        let bytes = encode_png_rgba(&stitched)?;
        if bytes.len() > MAX_SCROLLED_PNG_BYTES {
            return Err(format!(
                "stitched png exceeds {MAX_SCROLLED_PNG_BYTES} bytes ({})",
                bytes.len()
            ));
        }
        let meta = scrolled_result_json(
            "",
            target,
            metrics.content_height.max(metrics.viewport.h),
            metrics.viewport.h,
            tiles.len(),
        );
        Ok((bytes, meta))
    })();
    let restore_err = set_offset(original).err();
    match (run, restore_err) {
        (Ok(v), None) => Ok(v),
        (Ok(_), Some(err)) => Err(format!("restore scroll failed: {err}")),
        (Err(err), _) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ScreenshotMode;

    fn metrics(vh: f32, content: f32, offset: f32) -> ScrollMetrics {
        ScrollMetrics {
            viewport: Bounds {
                x: 10.0,
                y: 20.0,
                w: 40.0,
                h: vh,
            },
            content_height: content,
            offset_y: offset,
        }
    }

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> RgbaImage {
        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..width * height {
            pixels.extend_from_slice(&rgba);
        }
        RgbaImage::new(width, height, pixels).unwrap()
    }

    #[test]
    fn scrolled_requires_target() {
        let spec = ScreenshotSpec {
            path: Some("a.png"),
            mode: ScreenshotMode::Scrolled,
            target: None,
            max_height_px: None,
        };
        let err = spec.validate_request().unwrap_err();
        assert!(err.contains("requires target"), "{err}");
        let spec = ScreenshotSpec {
            path: Some("a.png"),
            mode: ScreenshotMode::Viewport,
            target: None,
            max_height_px: None,
        };
        spec.validate_request().unwrap();
    }

    #[test]
    fn max_height_cap_is_hard() {
        let spec = ScreenshotSpec {
            path: Some("a.png"),
            mode: ScreenshotMode::Scrolled,
            target: Some("s"),
            max_height_px: Some(DEFAULT_MAX_HEIGHT_PX + 1),
        };
        let err = spec.validate_request().unwrap_err();
        assert!(err.contains("exceeds"), "{err}");
        let spec = ScreenshotSpec {
            path: Some("a.png"),
            mode: ScreenshotMode::Scrolled,
            target: Some("s"),
            max_height_px: Some(0),
        };
        assert!(spec.validate_request().unwrap_err().contains("> 0"));
    }

    #[test]
    fn plan_three_tiles_with_clamped_last() {
        let tiles = plan_scroll_tiles(&metrics(100.0, 250.0, 80.0), 16384).unwrap();
        assert_eq!(tiles.len(), 3);
        assert_eq!(tiles[0].offset_y, 0.0);
        assert_eq!(tiles[0].skip_top_px, 0.0);
        assert_eq!(tiles[0].take_height_px, 100.0);
        assert_eq!(tiles[1].offset_y, 100.0);
        assert_eq!(tiles[2].offset_y, 150.0);
        assert_eq!(tiles[2].skip_top_px, 50.0);
        assert_eq!(tiles[2].take_height_px, 50.0);
        let covered: f32 = tiles.iter().map(|t| t.take_height_px).sum();
        assert!((covered - 250.0).abs() < 0.01);
    }

    #[test]
    fn plan_one_tile_when_content_fits() {
        let tiles = plan_scroll_tiles(&metrics(100.0, 80.0, 0.0), 16384).unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].offset_y, 0.0);
        assert_eq!(tiles[0].take_height_px, 100.0);
    }

    #[test]
    fn plan_fails_closed_when_content_exceeds_cap() {
        let err = plan_scroll_tiles(&metrics(100.0, 20_000.0, 0.0), 4096).unwrap_err();
        assert!(err.contains("max_height_px"), "{err}");
        assert!(err.contains("20000") || err.contains("20000.0"), "{err}");
        assert!(!err.contains("truncated"));
    }

    #[test]
    fn plan_fails_closed_on_too_many_tiles() {
        let err =
            plan_scroll_tiles(&metrics(1.0, DEFAULT_MAX_HEIGHT_PX as f32, 0.0), 16384).unwrap_err();
        assert!(err.contains("tiles"), "{err}");
    }

    #[test]
    fn stitch_concatenates_without_inventing() {
        let top = solid(2, 1, [10, 20, 30, 255]);
        let bot = solid(2, 2, [40, 50, 60, 255]);
        let out = stitch_tiles_vertically(&[top, bot]).unwrap();
        assert_eq!(out.width, 2);
        assert_eq!(out.height, 3);
        assert_eq!(&out.pixels[0..4], [10, 20, 30, 255]);
        assert_eq!(&out.pixels[8..12], [40, 50, 60, 255]);
    }

    /// A macOS `screencapture -l` PNG is the whole window: title bar included.
    /// Hosts pass the **content** size, and the title bar is whatever is left
    /// over; a host that passed the frame size instead would zero that
    /// difference and crop every tile a title bar too high.
    #[test]
    fn crop_skips_the_title_bar_when_the_png_is_taller_than_the_window() {
        // 100×132 PNG for a 100×100 content area: 32 rows of title bar on top.
        // Row y is painted with red = y so a crop can be located exactly.
        let mut pixels = Vec::with_capacity(100 * 132 * 4);
        for y in 0..132u32 {
            for _x in 0..100u32 {
                pixels.extend_from_slice(&[y as u8, 0, 0, 255]);
            }
        }
        let img = RgbaImage::new(100, 132, pixels).unwrap();
        let png = encode_png_rgba(&img).unwrap();
        let clip = Bounds {
            x: 0.0,
            y: 10.0,
            w: 100.0,
            h: 50.0,
        };
        let tile = crop_window_png(&png, 100.0, 100.0, clip, 0.0, 50.0).unwrap();
        assert_eq!((tile.width, tile.height), (100, 50));
        assert_eq!(
            tile.pixels[0], 42,
            "content y=10 sits under a 32px title bar"
        );
        assert_eq!(tile.pixels[(49 * 100) * 4], 91, "last row is content y=59");

        // The same clip against the frame size (the bug): no title bar
        // compensation, tile starts 32 px too high.
        let wrong = crop_window_png(&png, 100.0, 132.0, clip, 0.0, 50.0).unwrap();
        assert_eq!(wrong.pixels[0], 10);
    }

    #[test]
    fn png_roundtrip_rgba() {
        let img = solid(3, 2, [1, 2, 3, 4]);
        let bytes = encode_png_rgba(&img).unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
        let back = decode_png_rgba(&bytes).unwrap();
        assert_eq!(back, img);
    }

    #[test]
    fn capture_sync_restores_offset_on_error() {
        let mut offset = 80.0_f32;
        let m = metrics(10.0, 25.0, 80.0);
        let img = solid(40, 50, [9, 9, 9, 255]);
        let png = encode_png_rgba(&img).unwrap();
        let mut captures = 0u32;
        let err = run_scrolled_capture_sync(
            "todo-list-scroll",
            m,
            |y| {
                offset = y;
                Ok(())
            },
            || Ok(()),
            || {
                captures += 1;
                if captures >= 2 {
                    Err("boom".into())
                } else {
                    Ok(png.clone())
                }
            },
            (40.0, 50.0),
            16384,
        )
        .unwrap_err();
        assert!(err.contains("boom"), "{err}");
        assert_eq!(offset, 80.0, "must restore original offset after error");
    }

    #[test]
    fn capture_sync_restores_offset_on_success() {
        let mut offset = 12.0_f32;
        let m = ScrollMetrics {
            viewport: Bounds {
                x: 0.0,
                y: 0.0,
                w: 4.0,
                h: 2.0,
            },
            content_height: 2.0,
            offset_y: 12.0,
        };
        let img = solid(4, 2, [7, 8, 9, 255]);
        let png = encode_png_rgba(&img).unwrap();
        let (bytes, meta) = run_scrolled_capture_sync(
            "todo-list-scroll",
            m,
            |y| {
                offset = y;
                Ok(())
            },
            || Ok(()),
            || Ok(png.clone()),
            (4.0, 2.0),
            16384,
        )
        .unwrap();
        assert!(bytes.starts_with(b"\x89PNG"));
        assert_eq!(meta["mode"], "scrolled");
        assert_eq!(meta["target"], "todo-list-scroll");
        assert_eq!(meta["tiles"], 1);
        assert_eq!(offset, 12.0);
    }

    #[test]
    fn scroll_unavailable_prefix() {
        let err = scroll_unavailable("no such id");
        assert!(is_scroll_unavailable(&err));
        assert!(!is_scroll_unavailable("screenshot_unavailable: x"));
    }
}

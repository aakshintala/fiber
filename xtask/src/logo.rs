//! The logo's alpha mask: a smooth wave and the name `fiber` set in
//! JetBrains Mono ExtraBold, 640 by 160 pixels of 8-bit alpha row by row.
//! `cargo xtask logo-mask` writes it to `crates/tui/assets/logo-mask.bin`;
//! the font is downloaded by whoever regenerates, never committed.

use std::path::Path;

use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, Size},
    outline::OutlinePen,
};
use tiny_skia::{FillRule, Mask, PathBuilder, Transform};

/// The mask's width in pixels: 32 cells at 20 pixels a cell.
pub(crate) const WIDTH: u32 = 640;
/// The mask's height in pixels: 4 rows at 40 pixels a row.
pub(crate) const HEIGHT: u32 = 160;
/// The wave owns the mask left of this column; the name starts past it.
pub(crate) const WAVE_PX: u32 = 60;

/// The default `--out`: the checked-in mask, from the workspace root.
pub(crate) const DEFAULT_OUT: &str = "crates/tui/assets/logo-mask.bin";

/// The name rasterised into the mask.
const WORD: &str = "fiber";

/// The probe scale [`mask`] measures the name's ink at before fitting it
/// to the box.
const PROBE: f32 = 100.0;

/// The wave's half stroke width in pixels.
const STROKE: f32 = 8.0;

/// 8-bit alpha from `cover` in 0.0..=1.0.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 0..=255 before the cast"
)]
fn alpha(cover: f32) -> u8 {
    (cover * 255.0).round().clamp(0.0, 255.0) as u8
}

/// A pixel coordinate from a glyph bound, floored at zero.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "floored at zero; glyph bounds are hundreds of pixels at most"
)]
fn at_zero(value: f32) -> u32 {
    value.floor().max(0.0) as u32
}

/// A glyph bound's far edge in pixels, ceiled at zero, so the coverage
/// keeps the edge's partial pixel.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "ceiled at zero; glyph bounds are hundreds of pixels at most"
)]
fn ceil_at_zero(value: f32) -> u32 {
    value.ceil().max(0.0) as u32
}

/// The `logo-mask` arguments.
pub(crate) struct Config {
    /// The `JetBrainsMono-ExtraBold.ttf` file to rasterise.
    pub(crate) font: String,
    /// Where the mask bytes go.
    pub(crate) out: String,
}

/// One rasterised glyph: 8-bit coverage at its origin in mask pixels.
pub(crate) struct Glyph {
    /// The glyph's left column in the mask.
    pub(crate) x: u32,
    /// The glyph's top row in the mask.
    pub(crate) y: u32,
    /// The coverage's width.
    pub(crate) width: u32,
    /// The coverage's height.
    pub(crate) height: u32,
    /// `width` by `height` bytes, row by row.
    pub(crate) coverage: Vec<u8>,
}

/// The analytic smooth wave: a vertical sine stroke down the mask's left,
/// shaped like the pixel logo's zigzag wave (`crates/tui/src/logo.rs`)
/// and spanning its full height. Confined to `x < WAVE_PX`.
pub(crate) fn wave(width: u32, height: u32) -> Vec<u8> {
    let row_len = width as usize;
    let mut out = vec![0u8; row_len.saturating_mul(height as usize)];
    if width == 0 || height == 0 {
        return out;
    }
    let edge = WAVE_PX.min(width);
    for (row, line) in out.chunks_exact_mut(row_len).enumerate() {
        let centre = 30.0 + 22.0 * (std::f32::consts::TAU * 2.0 * row as f32 / height as f32).sin();
        for x in 0..edge {
            let distance = (x as f32 - centre).abs();
            if distance < STROKE {
                let cover = 1.0 - distance / STROKE;
                let smooth = cover * cover * (3.0 - 2.0 * cover);
                if let Some(slot) = line.get_mut(x as usize) {
                    *slot = alpha(smooth);
                }
            }
        }
    }
    out
}

/// Stamps glyph coverage into a `width`-by-`height` mask, clipping glyphs
/// at the box edges. Where glyphs overlap the stronger coverage wins.
pub(crate) fn compose(width: u32, height: u32, glyphs: impl IntoIterator<Item = Glyph>) -> Vec<u8> {
    let row_len = width as usize;
    let mut out = vec![0u8; row_len.saturating_mul(height as usize)];
    for glyph in glyphs {
        for (row, line) in glyph
            .coverage
            .chunks_exact(glyph.width.max(1) as usize)
            .take(glyph.height as usize)
            .enumerate()
        {
            let Ok(down) = u32::try_from(row) else {
                continue;
            };
            let Some(y) = glyph.y.checked_add(down) else {
                continue;
            };
            if y >= height {
                continue;
            }
            for (col, ink) in line.iter().enumerate() {
                let Ok(across) = u32::try_from(col) else {
                    continue;
                };
                let Some(x) = glyph.x.checked_add(across) else {
                    continue;
                };
                if x >= width {
                    continue;
                }
                if let Some(slot) = out.get_mut(y as usize * row_len + x as usize) {
                    *slot = (*slot).max(*ink);
                }
            }
        }
    }
    out
}

/// Feeds one glyph's outline into a [`PathBuilder`]. Font outlines rise
/// from the baseline while mask rows grow down, so `y` is flipped around
/// `dy`; `dx` puts the glyph at its caret past `origin_x`. Flipping keeps
/// holes holes under the nonzero fill rule: outer and inner contours only
/// swap sign, never reach zero.
struct Pen<'a> {
    /// The path collecting the glyph's contours.
    builder: &'a mut PathBuilder,
    /// The caret: added to every outline `x`.
    dx: f32,
    /// The baseline row: every outline `y` is taken from below it.
    dy: f32,
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.builder.move_to(x + self.dx, self.dy - y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.builder.line_to(x + self.dx, self.dy - y);
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.builder
            .quad_to(cx0 + self.dx, self.dy - cy0, x + self.dx, self.dy - y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.builder.cubic_to(
            cx0 + self.dx,
            self.dy - cy0,
            cx1 + self.dx,
            self.dy - cy1,
            x + self.dx,
            self.dy - y,
        );
    }

    fn close(&mut self) {
        self.builder.close();
    }
}

/// Rasterises [`WORD`] in the font's ExtraBold face into `x >= WAVE_PX`,
/// sized so the name fills the [`HEIGHT`]-pixel box, and merges it with
/// [`wave`]. The output is [`WIDTH`] by [`HEIGHT`] bytes, row by row.
/// Deterministic: the same font bytes give the same bytes.
pub(crate) fn mask(font: &[u8]) -> Result<Vec<u8>, String> {
    let font = FontRef::new(font).map_err(|error| format!("font: {error}"))?;
    let location = LocationRef::default();
    let scale = fit_scale(ink_box(&rasterise(&font, PROBE, WAVE_PX + 8, 112.0)));
    let ascent = font.metrics(Size::new(scale), location).ascent;
    let baseline = ascent + (HEIGHT as f32 - ascent) / 2.0;
    let mut glyphs = rasterise(&font, scale, WAVE_PX + 8, baseline);
    if let Some((left, top, width, height)) = ink_box(&glyphs) {
        let spare_w = (WIDTH - WAVE_PX) as f32 - width as f32;
        let spare_h = HEIGHT as f32 - height as f32;
        for glyph in &mut glyphs {
            glyph.x = at_zero(glyph.x as f32 + spare_w / 2.0 + WAVE_PX as f32 - left as f32);
            glyph.y = at_zero(glyph.y as f32 + spare_h / 2.0 - top as f32);
        }
    }
    let name = compose(WIDTH, HEIGHT, glyphs);
    let mut out = wave(WIDTH, HEIGHT);
    for (slot, ink) in out.iter_mut().zip(name.iter()) {
        *slot = (*slot).max(*ink);
    }
    Ok(out)
}

/// Rasterises [`WORD`] in the font at `scale` pixels per em, each glyph
/// at its origin past `origin_x` on `baseline`. The face is monospace, so
/// the caret steps by each glyph's hmtx advance; no kerning lookup, every
/// advance is the same width. A glyph with no outline, a failed draw or a
/// zero-area path is skipped, keeping its advance so the rest of the word
/// stays where the font puts it.
fn rasterise(font: &FontRef<'_>, scale: f32, origin_x: u32, baseline: f32) -> Vec<Glyph> {
    let size = Size::new(scale);
    let location = LocationRef::default();
    let charmap = font.charmap();
    let advances = font.glyph_metrics(size, location);
    let outlines = font.outline_glyphs();
    let mut glyphs = Vec::new();
    let mut caret = origin_x as f32;
    for letter in WORD.chars() {
        let Some(id) = charmap.map(letter) else {
            continue;
        };
        let advance = advances.advance_width(id).unwrap_or(0.0);
        let origin = caret;
        caret += advance;
        let Some(outline) = outlines.get(id) else {
            continue;
        };
        let mut builder = PathBuilder::new();
        {
            let mut pen = Pen {
                builder: &mut builder,
                dx: origin,
                dy: baseline,
            };
            if outline.draw(size, &mut pen).is_err() {
                continue;
            }
        }
        let Some(path) = builder.finish() else {
            continue;
        };
        let bounds = path.bounds();
        if bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            continue;
        }
        let (x, y) = (at_zero(bounds.x()), at_zero(bounds.y()));
        let (right, bottom) = (ceil_at_zero(bounds.right()), ceil_at_zero(bounds.bottom()));
        let (width, height) = (right.saturating_sub(x), bottom.saturating_sub(y));
        let Some(mut raster) = Mask::new(width, height) else {
            continue;
        };
        raster.fill_path(
            &path,
            FillRule::Winding,
            true,
            Transform::from_translate(-(x as f32), -(y as f32)),
        );
        glyphs.push(Glyph {
            x,
            y,
            width,
            height,
            coverage: raster.data().to_vec(),
        });
    }
    glyphs
}

/// The raster scale fitting a probe ink box into the name region with an
/// 8-pixel margin on each side, or [`PROBE`] when the probe found no ink.
fn fit_scale(probe: Option<(u32, u32, u32, u32)>) -> f32 {
    let Some((_, _, width, height)) = probe else {
        return PROBE;
    };
    if width == 0 || height == 0 {
        return PROBE;
    }
    let across = (WIDTH - WAVE_PX - 16) as f32 / width as f32;
    let down = (HEIGHT - 24) as f32 / height as f32;
    PROBE * across.min(down)
}

/// The ink's bounding box in mask coordinates: left, top, width, height.
/// [`None`] when nothing is inked.
fn ink_box(glyphs: &[Glyph]) -> Option<(u32, u32, u32, u32)> {
    let mut left = u32::MAX;
    let mut top = u32::MAX;
    let mut right = 0u32;
    let mut bottom = 0u32;
    for glyph in glyphs {
        for (row, line) in glyph
            .coverage
            .chunks_exact(glyph.width.max(1) as usize)
            .enumerate()
        {
            let Ok(down) = u32::try_from(row) else {
                continue;
            };
            for (col, ink) in line.iter().enumerate() {
                if *ink == 0 {
                    continue;
                }
                let Ok(across) = u32::try_from(col) else {
                    continue;
                };
                if let (Some(x), Some(y)) = (glyph.x.checked_add(across), glyph.y.checked_add(down))
                {
                    left = left.min(x);
                    top = top.min(y);
                    right = right.max(x);
                    bottom = bottom.max(y);
                }
            }
        }
    }
    if left > right {
        return None;
    }
    Some((left, top, right - left + 1, bottom - top + 1))
}

/// Parses the `logo-mask` arguments. Missing `--font` is an error; `--out`
/// defaults to [`DEFAULT_OUT`].
pub(crate) fn parse(args: &[String]) -> Result<Config, String> {
    let mut font = None;
    let mut out = None;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--font" => {
                font = Some(value(&mut rest, "--font")?);
            }
            "--out" => {
                out = Some(value(&mut rest, "--out")?);
            }
            "--help" => return Err(usage()),
            other => return Err(format!("unknown flag {other}\n{}", usage())),
        }
    }
    Ok(Config {
        font: font.ok_or_else(|| format!("--font is required\n{}", usage()))?,
        out: out.unwrap_or_else(|| DEFAULT_OUT.to_owned()),
    })
}

/// The next argument after a flag, or an error naming it.
fn value<'a>(rest: &mut std::slice::Iter<'a, String>, flag: &str) -> Result<String, String> {
    rest.next()
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value\n{}", usage()))
}

/// How to regenerate the mask, naming the font release.
fn usage() -> String {
    format!(
        "usage: cargo xtask logo-mask --font <path> [--out <path>]\n\
        \n\
        Regenerates the logo's alpha mask (default {DEFAULT_OUT}) by\n\
        rasterising `fiber` in JetBrains Mono ExtraBold. Download JetBrains\n\
        Mono v2.304 (JetBrainsMono-2.304.zip) from\n\
        https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304\n\
        and pass fonts/ttf/JetBrainsMono-ExtraBold.ttf as --font."
    )
}

/// Runs the `logo-mask` subcommand: rasterises `--font` and writes `--out`.
pub(crate) fn run(args: &[String]) -> Result<bool, String> {
    let config = parse(args)?;
    let font = std::fs::read(&config.font).map_err(|error| format!("{}: {error}", config.font))?;
    let bytes = mask(&font)?;
    if let Some(parent) = Path::new(&config.out).parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(&config.out, &bytes).map_err(|error| format!("{}: {error}", config.out))?;
    println!("logo-mask: {} bytes to {}", bytes.len(), config.out);
    Ok(true)
}

#[cfg(test)]
#[path = "logo_tests.rs"]
mod tests;

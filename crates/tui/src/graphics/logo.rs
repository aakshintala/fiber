//! Home's logo image: a tinted wave and name where the terminal speaks
//! an image protocol, in exactly the pixel logo's cells (`docs/tui.md`,
//! "The logo"). The mask is checked in, built from the font with
//! `cargo xtask logo-mask`; the tint comes from the theme at run time,
//! so Fiber bundles no font and the logo follows any theme.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ratatui::layout::Rect;

use super::{png, sixel};
use crate::keys::{Event, Reply};

/// The logo's own image id: skipped by the image registry exactly like
/// the graphics probe's, so no ordinary image part ever collides with
/// the logo's transmission (`docs/tui.md`, "The logo").
pub(crate) const ID: u32 = 30;

/// The mask's width in pixels: 32 cells at 20 pixels a cell.
pub(crate) const WIDTH: u32 = 640;
/// The mask's height in pixels: 4 rows at 40 pixels a row.
pub(crate) const HEIGHT: u32 = 160;
/// The logo's width in cells: the pixel logo's width.
pub(crate) const COLS: u16 = 32;
/// The logo's height in rows: the pixel logo's height.
pub(crate) const ROWS: u16 = 4;
/// The wave owns the mask left of this column; the name starts past it.
pub(crate) const WAVE_PX: u32 = 60;

/// The checked-in wave-and-name alpha mask, 640 by 160, built from the
/// font with `cargo xtask logo-mask` (`docs/tui.md`, "The logo").
pub(crate) const MASK: &[u8] = include_bytes!("../../assets/logo-mask.bin");

/// Whether a reported cell of `cell` pixels per cell is believable: 1
/// by 1 up to 64 by 128 pixels, inclusive. Zero or larger keeps the
/// pixel logo for the session, silently, never an error (`docs/tui.md`,
/// "The logo").
pub(crate) fn cell_ok(cell: (u16, u16)) -> bool {
    let (wide, high) = cell;
    (1..=64).contains(&wide) && (1..=128).contains(&high)
}

/// Which image protocol draws the logo, if any: kitty when known, else
/// iTerm2, else Sixel, else none (`docs/tui.md`, "The logo").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Protocol {
    /// No image protocol: the pixel logo stays.
    None,
    /// Kitty's graphics protocol, placed by cell count.
    Kitty,
    /// iTerm2's protocol, over a fixed 32-by-4 image.
    Iterm,
    /// Sixel, scaled to the measured cell.
    Sixel {
        /// The measured cell in pixels.
        cell: (u16, u16),
    },
}

/// Which image protocol draws the logo: kitty when known, else iTerm2,
/// else Sixel on a measured cell inside the bound, else none. Kitty
/// places by cell count and needs no reported geometry; Sixel sizes
/// from the cell, so an unmeasured or out-of-bound cell keeps the
/// pixel logo (`docs/tui.md`, "The logo").
pub(crate) fn select(kitty: bool, iterm: bool, sixel_cell: Option<(u16, u16)>) -> Protocol {
    if kitty {
        return Protocol::Kitty;
    }
    if iterm {
        return Protocol::Iterm;
    }
    match sixel_cell {
        Some(cell) if cell_ok(cell) => Protocol::Sixel { cell },
        _ => Protocol::None,
    }
}

/// Which cells show the image instead of the pixel logo: kitty's by
/// placeholder, iTerm2's and Sixel's blank under the positioned image
/// (`docs/tui.md`, "The logo").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shown {
    /// The pixel logo draws: no protocol, no depth, flat, narrow, or
    /// a failed or cell-blocked session.
    Pixels,
    /// Kitty placeholders draw in the logo's cells.
    Kitty,
    /// Blank cells draw under the positioned image.
    Blank,
}

/// The logo's tint: the wave's colour and the name's five gradient
/// stops (`docs/tui.md`, "The logo").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Tint {
    /// The wave's colour.
    pub(crate) wave: crate::theme::Rgb,
    /// The name's gradient stops: heading, accent, string, type,
    /// keyword.
    pub(crate) stops: [crate::theme::Rgb; 5],
}

/// The tinted mask as 8-bit RGBA, row-major: the wave colour left of
/// the boundary, the name gradient across the stops right of it, the
/// mask's alpha on every pixel.
pub(crate) fn rgba(tint: &Tint) -> Vec<u8> {
    let (width, height) = (
        usize::try_from(WIDTH).unwrap_or(0),
        usize::try_from(HEIGHT).unwrap_or(0),
    );
    let [stop0, stop1, stop2, stop3, stop4] = tint.stops;
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let alpha = MASK.get(y * width + x).copied().unwrap_or(0);
            let xu = u32::try_from(x).unwrap_or(u32::MAX);
            let (red, green, blue) = if xu < WAVE_PX {
                tint.wave
            } else {
                let scaled = (xu - WAVE_PX) as f32 / (WIDTH - WAVE_PX - 1) as f32 * 4.0;
                let (from, to, f) = if scaled >= 3.0 {
                    (stop3, stop4, scaled - 3.0)
                } else if scaled >= 2.0 {
                    (stop2, stop3, scaled - 2.0)
                } else if scaled >= 1.0 {
                    (stop1, stop2, scaled - 1.0)
                } else {
                    (stop0, stop1, scaled)
                };
                let lerp = |a: u8, b: u8| {
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "a blend of two channels with weight in 0..=1 stays in 0..=255"
                    )]
                    let mixed = (f32::from(a) + (f32::from(b) - f32::from(a)) * f).round() as u8;
                    mixed
                };
                (lerp(from.0, to.0), lerp(from.1, to.1), lerp(from.2, to.2))
            };
            out.extend_from_slice(&[red, green, blue, alpha]);
        }
    }
    out
}

/// Scales 8-bit RGBA `src` (`src_w` by `src_h`) to `dst_w` by `dst_h`
/// by area-averaging each target pixel over its source box.
fn scale(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let (src_w, src_h, dst_w, dst_h) = (
        usize::try_from(src_w).unwrap_or(0),
        usize::try_from(src_h).unwrap_or(0),
        usize::try_from(dst_w).unwrap_or(0),
        usize::try_from(dst_h).unwrap_or(0),
    );
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return Vec::new();
    }
    let mut out = vec![0u8; dst_w * dst_h * 4];
    for y in 0..dst_h {
        for x in 0..dst_w {
            let x0 = x * src_w / dst_w;
            let x1 = ((x + 1) * src_w / dst_w).max(x0 + 1).min(src_w);
            let y0 = y * src_h / dst_h;
            let y1 = ((y + 1) * src_h / dst_h).max(y0 + 1).min(src_h);
            let mut sum = [0u32; 4];
            for sy in y0..y1 {
                for sx in x0..x1 {
                    if let Some(pixel) = src.get((sy * src_w + sx) * 4..(sy * src_w + sx) * 4 + 4) {
                        for (slot, channel) in sum.iter_mut().zip(pixel) {
                            *slot += u32::from(*channel);
                        }
                    }
                }
            }
            let count = u32::try_from((x1 - x0) * (y1 - y0))
                .unwrap_or(u32::MAX)
                .max(1);
            if let Some(pixel) = out.get_mut((y * dst_w + x) * 4..(y * dst_w + x) * 4 + 4) {
                for (slot, total) in pixel.iter_mut().zip(sum) {
                    *slot = u8::try_from(total / count).unwrap_or(u8::MAX);
                }
            }
        }
    }
    out
}

/// The logo's kitty transmit payload: the tinted RGBA as base64. The
/// event loop chunks it beside the place bytes through the writer, so
/// the payload itself carries no escapes (`docs/tui.md`, "The logo").
pub(crate) fn kitty(tint: &Tint) -> Vec<u8> {
    STANDARD.encode(rgba(tint)).into_bytes()
}

/// The logo's iTerm2 image: the File header naming a 32-by-4 image, the
/// mask's PNG as base64, BEL after it (`docs/tui.md`, "The logo").
pub(crate) fn iterm(tint: &Tint) -> Vec<u8> {
    let png = png::encode_rgba(WIDTH, HEIGHT, &rgba(tint));
    let mut out = b"\x1b]1337;File=inline=1;width=32;height=4;preserveAspectRatio=0:".to_vec();
    out.extend_from_slice(STANDARD.encode(png).as_bytes());
    out.push(0x07);
    out
}

/// The logo's Sixel data over `background`, scaled to the bounded
/// measured `cell`: 32 cells by the cell's pixels wide, 4 rows high
/// (`docs/tui.md`, "The logo").
pub(crate) fn sixel(tint: &Tint, background: [u8; 3], cell: (u16, u16)) -> Vec<u8> {
    let target_w = u32::from(COLS) * u32::from(cell.0);
    let target_h = u32::from(ROWS) * u32::from(cell.1);
    let scaled = scale(&rgba(tint), WIDTH, HEIGHT, target_w, target_h);
    sixel::encode(target_w, target_h, &scaled, background)
}

/// The logo's kitty place bytes: unicode placeholders over the logo's
/// 32-by-4 cells under its own id.
fn place() -> Vec<u8> {
    format!("\x1b_Ga=p,U=1,i={ID},c={COLS},r={ROWS}\x1b\\").into_bytes()
}

/// Wraps `data` in a positioned write: the cursor saved, moved to the
/// logo rect's first cell, restored after it.
fn positioned(rect: Rect, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::from(b"\x1b7".as_slice());
    out.extend_from_slice(format!("\x1b[{};{}H", rect.y + 1, rect.x + 1).as_bytes());
    out.extend_from_slice(data);
    out.extend_from_slice(b"\x1b8");
    out
}

/// What one frame's emission writes: any positioned image bytes, and
/// whether the frame must repaint first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct After {
    /// The positioned write, or the kitty place bytes when a transmit
    /// is due, else empty.
    pub(crate) bytes: Vec<u8>,
    /// The current screen needs a repaint and immediate redraw first:
    /// a shown image lost its rect.
    pub(crate) repaint: bool,
}

/// What the terminal currently shows for the logo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Emitted {
    /// The protocol the bytes went out under.
    protocol: Protocol,
    /// The rect they cover.
    rect: Rect,
    /// The tint and background they carry.
    tint: Tint,
    /// The page colour behind the image.
    background: [u8; 3],
}

/// Tracks the logo's emission: once per theme per home visit. A shown
/// image lost its rect repaints first (except kitty, whose
/// placeholders die with the frame); iTerm2's and Sixel's images
/// rewrite after any touched frame; a repainted frame forces the write
/// (`docs/tui.md`, "The logo").
#[derive(Debug, Default)]
pub(crate) struct Emit {
    /// What the terminal shows, while it shows an image.
    shown: Option<Emitted>,
}

impl Emit {
    /// Decides this frame's emission for `protocol` over `rect` with
    /// `tint`: the positioned write or, for kitty, the place bytes when
    /// a transmit is due, else nothing; a repaint when a shown image
    /// lost its rect. A missing tint names a screen that cannot show
    /// an image, exactly like a missing rect.
    pub(crate) fn after_frame(
        &mut self,
        protocol: Protocol,
        rect: Option<Rect>,
        touched: bool,
        repainted: bool,
        tint: Option<(Tint, [u8; 3])>,
    ) -> After {
        let want = match (protocol, rect, tint) {
            (Protocol::None, _, _) | (_, None, _) | (_, _, None) => None,
            (protocol, Some(rect), Some((tint, background))) => Some(Emitted {
                protocol,
                rect,
                tint,
                background,
            }),
        };
        match (want, self.shown.take()) {
            (None, None) => After {
                bytes: Vec::new(),
                repaint: false,
            },
            // A shown image lost its rect: clear the stale image, then
            // redraw. Kitty's placeholders die with the frame, so its
            // loss needs no wipe.
            (None, Some(was)) => {
                let repaint = was.protocol != Protocol::Kitty;
                After {
                    bytes: Vec::new(),
                    repaint,
                }
            }
            (Some(cur), None) => {
                let bytes = emit_bytes(&cur);
                self.shown = Some(cur);
                After {
                    bytes,
                    repaint: false,
                }
            }
            (Some(cur), Some(was))
                if cur == was && !repainted && (cur.protocol == Protocol::Kitty || !touched) =>
            {
                self.shown = Some(was);
                After {
                    bytes: Vec::new(),
                    repaint: false,
                }
            }
            (Some(cur), _) => {
                let bytes = emit_bytes(&cur);
                self.shown = Some(cur);
                After {
                    bytes,
                    repaint: false,
                }
            }
        }
    }
}

/// The bytes one emission writes: kitty's place (its transmit queues
/// beside it through the chunk writer), else the positioned image.
fn emit_bytes(next: &Emitted) -> Vec<u8> {
    match next.protocol {
        Protocol::None => Vec::new(),
        Protocol::Kitty => place(),
        Protocol::Iterm => positioned(next.rect, &iterm(&next.tint)),
        Protocol::Sixel { cell } => {
            positioned(next.rect, &sixel(&next.tint, next.background, cell))
        }
    }
}

/// One XTVERSION reply candidate: `buf` starts with `ESC P`.
#[derive(Debug)]
pub(crate) enum Dcs {
    /// Every byte so far fits the grammar; the rest may follow.
    Hold,
    /// A complete reply: its events and the reply's length.
    Done(Vec<Event>, usize),
    /// The first byte off the grammar: read `ESC P` as keys instead.
    Off,
}

/// The longest a held XTVERSION reply grows: a version line is short,
/// so anything longer is typed text, and resolving it keeps the parser
/// from holding a never-terminated candidate forever.
const DCS_CAP: usize = 256;

/// Reads an XTVERSION reply byte by byte: `ESC P > | <text>` with `ESC \`
/// or BEL after it. A buffer that ends while every byte so far fits is
/// held. The first byte that cannot continue the grammar ends the
/// candidate, so a typed Alt+Shift+P followed by text stays keys.
pub(crate) fn dcs(buf: &[u8]) -> Dcs {
    if buf.len() > DCS_CAP {
        return Dcs::Off;
    }
    const PREFIX: &[u8] = b"\x1bP>|";
    for (at, want) in PREFIX.iter().enumerate() {
        match buf.get(at) {
            None => return Dcs::Hold,
            Some(byte) if byte == want => {}
            Some(_) => return Dcs::Off,
        }
    }
    let mut at = PREFIX.len();
    loop {
        match buf.get(at) {
            None => return Dcs::Hold,
            Some(0x07) => {
                let text = String::from_utf8_lossy(buf.get(PREFIX.len()..at).unwrap_or_default())
                    .into_owned();
                return Dcs::Done(vec![Event::Reply(Reply::Version(text))], at + 1);
            }
            Some(0x1b) => match buf.get(at.saturating_add(1)) {
                None => return Dcs::Hold,
                Some(b'\\') => {
                    let text =
                        String::from_utf8_lossy(buf.get(PREFIX.len()..at).unwrap_or_default())
                            .into_owned();
                    return Dcs::Done(
                        vec![Event::Reply(Reply::Version(text))],
                        at.saturating_add(2),
                    );
                }
                Some(_) => return Dcs::Off,
            },
            // A control byte is typed text, not a version.
            Some(byte) if *byte < 0x20 => return Dcs::Off,
            Some(_) => at = at.saturating_add(1),
        }
    }
}

#[cfg(test)]
#[path = "logo_tests.rs"]
mod tests;

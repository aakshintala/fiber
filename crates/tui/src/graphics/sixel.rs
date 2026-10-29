//! A minimal Sixel encoder for the logo: the target-sized RGBA over
//! the page's background, quantized to 3-3-2, run-length packed
//! (`docs/tui.md`, "The logo"). Sixel carries no alpha, so nearly
//! transparent pixels read as the background.

/// One band holds six rows.
const BAND_ROWS: usize = 6;

/// A run longer than this packs into `!<count><char>`.
const RLE_OVER: usize = 3;

/// Encodes `rgba` (`width` by `height`, 8-bit RGBA row-major) as Sixel
/// data over `background`: 3-3-2 quantized, six-row bands, P2 set for
/// square pixels.
pub(crate) fn encode(width: u32, height: u32, rgba: &[u8], background: [u8; 3]) -> Vec<u8> {
    // The 3-3-2 register each pixel takes, in first-use order.
    let mut registers: Vec<u8> = Vec::new();
    let mut at = vec![0u8; width.saturating_mul(height) as usize];
    let [back_red, back_green, back_blue] = background;
    let (pixels, _) = rgba.as_chunks::<4>();
    for (pixel, slot) in pixels.iter().zip(at.iter_mut()) {
        let [red, green, blue, alpha] = *pixel;
        let (red, green, blue) = if alpha < 32 {
            (back_red, back_green, back_blue)
        } else {
            let blend = |ink: u8, base: u8| {
                u8::try_from(
                    (u16::from(ink) * u16::from(alpha) + u16::from(base) * u16::from(255 - alpha))
                        / 255,
                )
                .unwrap_or(u8::MAX)
            };
            (
                blend(red, back_red),
                blend(green, back_green),
                blend(blue, back_blue),
            )
        };
        let register = (red & 0xe0) | ((green & 0xe0) >> 3) | (blue >> 6);
        if !registers.contains(&register) {
            registers.push(register);
        }
        *slot = register;
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"\x1bP0;1q");
    out.extend_from_slice(format!("\"1;1;{width};{height}").as_bytes());
    for (seq, register) in registers.iter().enumerate() {
        let red = f64::from(register >> 5) * 100.0 / 7.0;
        let green = f64::from((register >> 2) & 0x07) * 100.0 / 7.0;
        let blue = f64::from(register & 0x03) * 100.0 / 3.0;
        out.extend_from_slice(
            format!(
                "#{};2;{};{};{}",
                seq,
                red.round(),
                green.round(),
                blue.round()
            )
            .as_bytes(),
        );
    }
    let rows = height as usize;
    let cols = width as usize;
    let mut band = 0;
    while band * BAND_ROWS < rows {
        for (seq, register) in registers.iter().enumerate() {
            out.extend_from_slice(format!("#{seq}").as_bytes());
            let mut run = (b'?', 0usize);
            let flush = |out: &mut Vec<u8>, run: (u8, usize)| {
                if run.1 > RLE_OVER {
                    out.extend_from_slice(format!("!{}{}", run.1, run.0 as char).as_bytes());
                } else {
                    out.extend(std::iter::repeat_n(run.0, run.1));
                }
            };
            for col in 0..cols {
                let mut cell = b'?';
                for row in 0..BAND_ROWS {
                    let y = band * BAND_ROWS + row;
                    if y < rows && at.get(y * cols + col) == Some(register) {
                        cell += 1 << row;
                    }
                }
                if cell == run.0 {
                    run.1 += 1;
                } else {
                    flush(&mut out, run);
                    run = (cell, 1);
                }
            }
            flush(&mut out, run);
            out.push(b'$');
        }
        out.push(b'-');
        band += 1;
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

#[cfg(test)]
#[path = "sixel_tests.rs"]
mod tests;

//! Tests for the logo's XTVERSION reply grammar: what holds, what
//! completes, and what reads as keys (`docs/tui.md`, "The logo").

use super::{
    COLS, Dcs, Emit, HEIGHT, MASK, Protocol, ROWS, Tint, WAVE_PX, WIDTH, cell_ok, dcs, iterm,
    kitty, rgba, scale, select, sixel,
};
use crate::keys::{Event, Reply};

/// A complete reply carries the version text and its length.
#[test]
fn dcs_done_names_the_version_and_its_length() {
    for (bytes, want) in [
        (b"\x1bP>|iTerm2 3.5.0\x1b\\".as_slice(), "iTerm2 3.5.0"),
        (b"\x1bP>|WezTerm 1\x07".as_slice(), "WezTerm 1"),
        // Empty text is a version too: it simply names no protocol.
        (b"\x1bP>|\x1b\\".as_slice(), ""),
    ] {
        let (events, used) = match dcs(bytes) {
            Dcs::Done(events, used) => (events, used),
            Dcs::Hold | Dcs::Off => panic!("{bytes:?} completes"),
        };
        assert_eq!(events, vec![Event::Reply(Reply::Version(want.to_owned()))]);
        assert_eq!(used, bytes.len());
    }
}

/// A prefix that fits so far holds for the next read.
#[test]
fn dcs_hold_keeps_a_fitting_prefix() {
    for prefix in [
        b"\x1b".as_slice(),
        b"\x1bP".as_slice(),
        b"\x1bP>".as_slice(),
        b"\x1bP>|".as_slice(),
        b"\x1bP>|iTerm2 ".as_slice(),
        b"\x1bP>|iTerm2 3.5.0\x1b".as_slice(),
    ] {
        assert!(matches!(dcs(prefix), Dcs::Hold), "{prefix:?} holds");
    }
}

/// The first byte off the grammar ends the candidate: it reads as
/// keys instead, never holding.
#[test]
fn dcs_off_rejects_off_grammar_bytes() {
    for bytes in [
        b"\x1bX>|iTerm2 3.5.0\x1b\\".as_slice(),
        b"\x1bP>|iTerm\x012\x1b\\".as_slice(),
        b"\x1bP>|text\x1bX".as_slice(),
    ] {
        assert!(matches!(dcs(bytes), Dcs::Off), "{bytes:?} is keys");
    }
}

/// A candidate past the cap resolves instead of holding forever.
#[test]
fn dcs_off_past_the_cap() {
    let mut candidate = b"\x1bP>|".to_vec();
    candidate.extend(std::iter::repeat_n(b'x', 300));
    assert!(matches!(dcs(&candidate), Dcs::Off));
}

/// The cap is exact: a 256-byte fitting candidate still holds or
/// completes, while a 257-byte candidate reads as keys.
#[test]
fn dcs_boundary_holds_at_256_and_off_at_257() {
    // 256 bytes, unterminated: still a fitting prefix.
    let mut holding = b"\x1bP>|".to_vec();
    holding.extend(std::iter::repeat_n(b'x', 252));
    assert_eq!(holding.len(), 256);
    assert!(matches!(dcs(&holding), Dcs::Hold));
    // 256 bytes, BEL-terminated: a complete reply.
    let mut done = b"\x1bP>|".to_vec();
    done.extend(std::iter::repeat_n(b'x', 251));
    done.push(0x07);
    assert_eq!(done.len(), 256);
    match dcs(&done) {
        Dcs::Done(_, used) => assert_eq!(used, 256),
        Dcs::Hold | Dcs::Off => panic!("a 256-byte reply completes"),
    }
    // 257 bytes: past the cap, keys.
    let mut off = b"\x1bP>|".to_vec();
    off.extend(std::iter::repeat_n(b'x', 253));
    assert_eq!(off.len(), 257);
    assert!(matches!(dcs(&off), Dcs::Off));
}

// The mask geometry the encoders share: the pixel logo's 32-by-4
// cells, 640 by 160 pixels, the wave left of column 60.
#[test]
fn mask_geometry_matches_the_pixel_logo_cells() {
    assert_eq!(WIDTH, 640);
    assert_eq!(HEIGHT, 160);
    assert_eq!(COLS, 32);
    assert_eq!(ROWS, 4);
    assert_eq!(WAVE_PX, 60);
    assert_eq!(MASK.len(), 640 * 160);
}

// The mask holds ink in the wave and in the name: neither region is
// empty, or the image would miss half the logo.
#[test]
fn mask_holds_ink_in_the_wave_and_in_the_name() {
    assert!(
        MASK.iter()
            .enumerate()
            .any(|(at, alpha)| *alpha > 0 && at % 640 < 60),
        "the wave draws"
    );
    assert!(
        MASK.iter()
            .enumerate()
            .any(|(at, alpha)| *alpha > 0 && at % 640 >= 60),
        "the name draws"
    );
}

// The cell gate: a reported cell of 1 by 1 up to 64 by 128 pixels is
// believable; zero or larger keeps the pixel logo (`docs/tui.md`,
// "The logo").
#[test]
fn cell_ok_takes_one_by_one_to_sixty_four_by_128() {
    for cell in [(1, 1), (32, 64), (64, 128), (10, 20)] {
        assert!(cell_ok(cell), "{cell:?} is believable");
    }
}

#[test]
fn cell_ok_rejects_zero_and_past_the_bound() {
    // Each comparison flips at its edge: 0 against 1, 64 against 65,
    // 128 against 129.
    for cell in [
        (0, 0),
        (0, 1),
        (1, 0),
        (65, 128),
        (64, 129),
        (65, 129),
        (65535, 65535),
    ] {
        assert!(!cell_ok(cell), "{cell:?} keeps the pixels");
    }
}

// The protocol order: kitty when known, else iTerm2, else Sixel, else
// none (`docs/tui.md`, "The logo").
#[test]
fn select_prefers_kitty_then_iterm_then_sixel() {
    assert_eq!(select(true, false, None), Protocol::Kitty);
    assert_eq!(
        select(true, true, Some((10, 20))),
        Protocol::Kitty,
        "kitty wins over every reply"
    );
    assert_eq!(
        select(false, true, Some((10, 20))),
        Protocol::Iterm,
        "iTerm2 wins over Sixel"
    );
    assert_eq!(
        select(false, false, Some((10, 20))),
        Protocol::Sixel { cell: (10, 20) }
    );
    assert_eq!(select(false, false, None), Protocol::None);
}

#[test]
fn select_needs_a_measured_cell_inside_the_bound_for_sixel() {
    // Kitty draws without reported geometry; Sixel never does.
    assert_eq!(select(true, false, None), Protocol::Kitty);
    for cell in [(0, 0), (65, 128), (64, 129), (65535, 65535)] {
        assert_eq!(
            select(false, false, Some(cell)),
            Protocol::None,
            "{cell:?} keeps the pixels"
        );
    }
    let protocol = select(false, false, Some((64, 128)));
    assert_eq!(protocol, Protocol::Sixel { cell: (64, 128) });
    if let Protocol::Sixel { cell } = protocol {
        // The largest raster: 32 cells by 64 pixels wide, 4 rows by
        // 128 high.
        assert_eq!(
            (
                u32::from(COLS).saturating_mul(u32::from(cell.0)),
                u32::from(ROWS).saturating_mul(u32::from(cell.1))
            ),
            (2048, 512)
        );
    } else {
        panic!("64 by 128 measures Sixel");
    }
}

/// One fixed tint for the encoder tests.
fn tint() -> Tint {
    Tint {
        wave: (1, 2, 3),
        stops: [(10, 0, 0), (0, 20, 0), (0, 0, 30), (40, 40, 0), (0, 50, 60)],
    }
}

// The tinted RGBA: the wave color left of the boundary, the name
// gradient from the first stop to the last right of it, and the
// mask's alpha on every pixel.
#[test]
fn rgba_tints_the_wave_and_the_name_over_the_mask_alpha() {
    let tint = tint();
    let rgba = rgba(&tint);
    assert_eq!(rgba.len(), 640 * 160 * 4);
    let [stop0, stop1, stop2, stop3, stop4] = tint.stops;
    let (rows, _) = rgba.as_chunks::<{ 640 * 4 }>();
    for (y, row) in rows.iter().enumerate() {
        let (pixels, _) = row.as_chunks::<4>();
        for (x, pixel) in pixels.iter().enumerate() {
            let want = if x < 60 {
                tint.wave
            } else {
                let scaled = (x - 60) as f32 / 579.0 * 4.0;
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
            assert_eq!(
                (pixel[0], pixel[1], pixel[2]),
                want,
                "pixel {x},{y} takes its region's color"
            );
            assert_eq!(
                pixel[3],
                MASK[y * 640 + x],
                "pixel {x},{y} keeps the mask's alpha"
            );
        }
    }
}

// The wave/name boundary: column 59 is wave, column 60 is the first
// stop, exactly.
#[test]
fn rgba_changes_regions_at_column_60() {
    let tint = tint();
    let rgba = rgba(&tint);
    for y in 0..160 {
        let left = &rgba[(y * 640 + 59) * 4..][..3];
        let right = &rgba[(y * 640 + 60) * 4..][..3];
        assert_eq!(
            (left[0], left[1], left[2]),
            tint.wave,
            "column 59 is wave on row {y}"
        );
        assert_eq!(
            (right[0], right[1], right[2]),
            tint.stops[0],
            "column 60 is the name on row {y}"
        );
    }
}

// The kitty payload is the tinted RGBA as base64: the loop chunks it
// beside the place bytes.
#[test]
fn kitty_is_the_rgba_as_base64() {
    use base64::Engine as _;
    let tint = tint();
    let payload = kitty(&tint);
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&payload)
        .expect("the payload is base64");
    assert_eq!(decoded, rgba(&tint));
}

// The iTerm2 image is a positioned PNG: the File header names a
// 32-by-4 image, the base64 between decodes to a PNG of the mask's
// size, and BEL ends it.
#[test]
fn iterm_is_a_32_by_4_png_with_bel_after_it() {
    use base64::Engine as _;
    let bytes = iterm(&tint());
    let header = b"\x1b]1337;File=inline=1;width=32;height=4;preserveAspectRatio=0:";
    assert!(bytes.starts_with(header));
    assert_eq!(bytes.last(), Some(&0x07));
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&bytes[header.len()..bytes.len() - 1])
        .expect("the file is base64");
    assert_eq!(
        &decoded[..8],
        &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
    );
    assert_eq!(&decoded[16..24], &[0, 0, 2, 128, 0, 0, 0, 160]);
    assert_eq!(
        &decoded[decoded.len() - 12..],
        b"\x00\x00\x00\x00IEND\xaeB`\x82"
    );
}

// Sixel names its raster after the measured cell: 32 cells by the
// cell's pixels wide, 4 rows high.
#[test]
fn sixel_names_its_raster_after_the_measured_cell() {
    let tint = tint();
    let small = sixel(&tint, [0, 0, 0], (1, 1));
    let small = String::from_utf8(small).expect("sixel is ASCII");
    assert!(small.contains("\"1;1;32;4"), "{small:?} scales to 32 by 4");
    let large = sixel(&tint, [0, 0, 0], (64, 128));
    let large = String::from_utf8(large).expect("sixel is ASCII");
    assert!(
        large.contains("\"1;1;2048;512"),
        "64 by 128 scales to 2048 by 512"
    );
}

// The emission decision: what shows writes once per theme per visit;
// what stops showing repaints, except kitty, which needs no wipe.
#[test]
fn after_frame_emits_once_then_holds() {
    use ratatui::layout::Rect;
    let rect = Rect::new(10, 5, 32, 4);
    let tint = (tint(), [4, 5, 6]);
    let mut emit = Emit::default();
    // Nothing showing, nothing wanted: nothing happens.
    let after = emit.after_frame(Protocol::None, None, false, false, None);
    assert_eq!(after.bytes, Vec::<u8>::new());
    assert!(!after.repaint);
    // A kitty rect writes its place bytes once, then holds.
    let after = emit.after_frame(Protocol::Kitty, Some(rect), false, false, Some(tint));
    assert!(!after.repaint);
    assert!(!after.bytes.is_empty());
    let again = emit.after_frame(Protocol::Kitty, Some(rect), true, false, Some(tint));
    assert!(!again.repaint, "a touched kitty frame rewrites nothing");
    assert!(again.bytes.is_empty());
    // A re-tint writes again.
    let retint = (
        Tint {
            wave: (7, 8, 9),
            stops: tint.0.stops,
        },
        tint.1,
    );
    let again = emit.after_frame(Protocol::Kitty, Some(rect), false, false, Some(retint));
    assert!(!again.bytes.is_empty());
}

#[test]
fn after_frame_repaints_a_gone_sixel_rect_but_not_a_kitty_one() {
    use ratatui::layout::Rect;
    let rect = Rect::new(10, 5, 32, 4);
    let tint = (tint(), [4, 5, 6]);
    let mut emit = Emit::default();
    let first = emit.after_frame(
        Protocol::Sixel { cell: (10, 20) },
        Some(rect),
        false,
        false,
        Some(tint),
    );
    assert!(!first.bytes.is_empty());
    // The rect gone after a Sixel show: repaint, then redraw.
    let gone = emit.after_frame(
        Protocol::Sixel { cell: (10, 20) },
        None,
        false,
        false,
        Some(tint),
    );
    assert!(gone.repaint);
    assert!(gone.bytes.is_empty());
    // …and nothing further: the show was cleared.
    let still = emit.after_frame(
        Protocol::Sixel { cell: (10, 20) },
        None,
        false,
        false,
        Some(tint),
    );
    assert!(!still.repaint);
    // The same for kitty, without the wipe: placeholders die with
    // the frame.
    let mut emit = Emit::default();
    let first = emit.after_frame(Protocol::Kitty, Some(rect), false, false, Some(tint));
    assert!(!first.bytes.is_empty());
    let gone = emit.after_frame(Protocol::Kitty, None, false, false, Some(tint));
    assert!(!gone.repaint);
    assert!(gone.bytes.is_empty());
}

#[test]
fn after_frame_rewrites_iterm_and_sixel_on_touch() {
    use ratatui::layout::Rect;
    let rect = Rect::new(10, 5, 32, 4);
    let tint = (tint(), [4, 5, 6]);
    let mut emit = Emit::default();
    let first = emit.after_frame(Protocol::Iterm, Some(rect), false, false, Some(tint));
    assert!(first.bytes.starts_with(b"\x1b7"));
    assert!(first.bytes.ends_with(b"\x1b8"));
    assert!(first.bytes.windows(7).any(|w| w == b"\x1b[6;11H"));
    let held = emit.after_frame(Protocol::Iterm, Some(rect), false, false, Some(tint));
    assert!(held.bytes.is_empty());
    // A scrolled frame rewrites the positioned image.
    let touched = emit.after_frame(Protocol::Iterm, Some(rect), true, false, Some(tint));
    assert_eq!(touched.bytes, first.bytes);
    // A repainted frame forces the write too.
    let mut emit = Emit::default();
    let forced = emit.after_frame(Protocol::Iterm, Some(rect), false, true, Some(tint));
    assert_eq!(forced.bytes, first.bytes);
}

// A gone rect resets the emission: the return re-emits, even on the
// same theme with no touch.
#[test]
fn after_frame_reemits_on_return_after_a_gone_rect() {
    use ratatui::layout::Rect;
    let rect = Rect::new(10, 5, 32, 4);
    let tint = (tint(), [4, 5, 6]);
    let mut emit = Emit::default();
    let first = emit.after_frame(Protocol::Iterm, Some(rect), false, false, Some(tint));
    assert!(!first.bytes.is_empty());
    assert!(!first.repaint);
    let gone = emit.after_frame(Protocol::Iterm, None, false, false, Some(tint));
    assert!(gone.bytes.is_empty());
    assert!(gone.repaint);
    let back = emit.after_frame(Protocol::Iterm, Some(rect), false, false, Some(tint));
    assert_eq!(back.bytes, first.bytes);
    assert!(!back.repaint);
}

// An empty side scales to nothing instead of dividing by zero: each
// comparison flips at its edge, so a single zero anywhere empties the
// target, even with the other three sides nonzero.
#[test]
fn scale_with_an_empty_side_is_empty() {
    assert!(scale(&[255, 0, 0, 255], 1, 1, 0, 1).is_empty());
    assert!(scale(&[255, 0, 0, 255], 1, 1, 1, 0).is_empty());
    assert!(scale(&[], 0, 0, 8, 8).is_empty());
    assert!(scale(&[], 0, 1, 8, 8).is_empty());
    assert!(scale(&[0u8; 16], 1, 0, 8, 8).is_empty());
}

// One test image: non-linear channels, every pixel distinct, so any
// shifted box or mistallied sum reads a different average.
fn figured(width: u32, height: u32, seeds: &[u8]) -> Vec<u8> {
    let mut src = Vec::with_capacity(seeds.len() * 4);
    for seed in seeds {
        src.extend_from_slice(&[
            *seed,
            seed.wrapping_mul(3).wrapping_add(11),
            seed.wrapping_mul(7).wrapping_add(5),
            255,
        ]);
    }
    assert_eq!(src.len(), (width * height * 4) as usize);
    src
}

// A 4-by-4 downscale to 2-by-2: every target pixel averages its own
// 2-by-2 source box.
#[test]
fn scale_down_averages_each_target_pixel_over_its_box() {
    let src = figured(
        4,
        4,
        &[
            3, 17, 42, 99, 8, 200, 5, 61, 150, 11, 88, 23, 7, 176, 64, 129,
        ],
    );
    assert_eq!(
        scale(&src, 4, 4, 2, 2),
        vec![
            57, 54, 84, 255, 51, 102, 111, 255, 86, 77, 95, 255, 76, 111, 153, 255,
        ]
    );
}

// A 6-by-2 downscale to 2-by-1: non-square 3-by-2 boxes, where the
// count's `*` and `+` disagree.
#[test]
fn scale_down_counts_a_non_square_box() {
    let seeds: Vec<u8> = (0..12u8)
        .map(|at| at.wrapping_mul(37).wrapping_add(13))
        .collect();
    let src = figured(6, 2, &seeds);
    assert_eq!(
        scale(&src, 6, 2, 2, 1),
        vec![75, 110, 108, 255, 144, 144, 117, 255,]
    );
}

// A 2-by-2 upscale to 4-by-4: each source pixel covers its 2-by-2
// block, pinning the one-pixel boxes and the clamped empty ones.
#[test]
fn scale_up_replicates_each_source_pixel() {
    let src = figured(2, 2, &[7, 19, 149, 203]);
    let (near, far) = (
        vec![
            7u8, 32, 54, 255, 7, 32, 54, 255, 19, 68, 138, 255, 19, 68, 138, 255,
        ],
        vec![
            149u8, 202, 24, 255, 149, 202, 24, 255, 203, 108, 146, 255, 203, 108, 146, 255,
        ],
    );
    assert_eq!(
        scale(&src, 2, 2, 4, 4),
        [near.clone(), near, far.clone(), far].concat()
    );
}

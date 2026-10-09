use super::*;
use tiny_skia::PathSegment;

fn flag(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

#[test]
fn alpha_rounds_and_clamps_coverage_to_a_byte() {
    for (cover, expected) in [(0.0, 0), (0.5, 128), (1.0, 255), (2.0, 255), (-1.0, 0)] {
        assert_eq!(alpha(cover), expected, "cover {cover}");
    }
}

#[test]
fn wave_stays_inside_its_region_and_is_not_empty() {
    let bytes = wave(WIDTH, HEIGHT);
    assert_eq!(bytes.len(), WIDTH as usize * HEIGHT as usize);
    assert!(bytes.iter().any(|ink| *ink != 0));
    for (at, ink) in bytes.iter().enumerate() {
        if at % WIDTH as usize >= WAVE_PX as usize {
            assert_eq!(*ink, 0, "ink at column {}", at % WIDTH as usize);
        }
    }
}

#[test]
fn wave_has_exact_sine_centres_and_strict_stroke_edges() {
    let width = 64usize;
    let bytes = wave(u32::try_from(width).unwrap(), 8);
    let pixel = |x: usize, y: usize| bytes[y * width + x];

    assert_eq!(pixel(30, 0), 255);
    assert_eq!(pixel(23, 0), 11);
    assert_eq!(pixel(22, 0), 0, "distance exactly STROKE has no coverage");
    assert_eq!(pixel(21, 0), 0, "pixels beyond STROKE have no coverage");
    assert_eq!(pixel(52, 1), 255, "the sine reaches its rightmost centre");
    assert_eq!(pixel(45, 1), 11);
    assert_eq!(pixel(44, 1), 0);
    assert_eq!(pixel(8, 3), 255, "the sine reaches its leftmost centre");
    assert_eq!(pixel(1, 3), 11);
    assert_eq!(pixel(0, 3), 0);
}

#[test]
fn wave_is_empty_when_either_dimension_is_zero() {
    assert!(wave(0, 3).is_empty(), "zero width");
    assert!(wave(3, 0).is_empty(), "zero height");
}

#[test]
fn wave_boundary_columns() {
    let bytes = wave(WIDTH, HEIGHT);
    let stride = WIDTH as usize;
    let inked = |column: usize| {
        bytes
            .iter()
            .enumerate()
            .any(|(at, ink)| at % stride == column && *ink != 0)
    };
    assert!(!inked(0), "the stroke never reaches the left edge");
    assert!(inked(59), "the last inside column carries the wave");
    assert!(!inked(60), "the first outside column stays empty");
}

#[test]
fn compose_places_glyph_coverage_at_its_origin() {
    let bytes = compose(
        5,
        4,
        [Glyph {
            x: 2,
            y: 1,
            width: 2,
            height: 2,
            coverage: vec![10, 20, 30, 40],
        }],
    );
    assert_eq!(bytes.len(), 20);
    for (at, ink) in bytes.iter().enumerate() {
        let (x, y) = (at % 5, at / 5);
        let want = match (x, y) {
            (2, 1) => 10,
            (3, 1) => 20,
            (2, 2) => 30,
            (3, 2) => 40,
            _ => 0,
        };
        assert_eq!(*ink, want, "pixel ({x}, {y})");
    }
}

#[test]
fn compose_clips_at_the_box_edges() {
    let glyph = |x: u32, y: u32| Glyph {
        x,
        y,
        width: 2,
        height: 2,
        coverage: vec![7, 7, 7, 7],
    };
    let right = compose(4, 4, [glyph(3, 0)]);
    assert_eq!(right.iter().filter(|ink| **ink != 0).count(), 2);
    let bottom = compose(4, 4, [glyph(0, 3)]);
    assert_eq!(bottom.iter().filter(|ink| **ink != 0).count(), 2);
    let past = compose(4, 4, [glyph(4, 4)]);
    assert!(past.iter().all(|ink| *ink == 0));
    let exact = compose(4, 4, [glyph(2, 2)]);
    assert_eq!(exact.iter().filter(|ink| **ink != 0).count(), 4);
}

#[test]
fn compose_keeps_the_stronger_coverage_where_glyphs_overlap() {
    let bytes = compose(
        3,
        2,
        [
            Glyph {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
                coverage: vec![9, 200],
            },
            Glyph {
                x: 1,
                y: 0,
                width: 2,
                height: 1,
                coverage: vec![100, 50],
            },
        ],
    );
    assert_eq!(bytes, vec![9, 200, 50, 0, 0, 0]);
}

#[test]
fn ink_box_bounds_the_ink_in_mask_coordinates() {
    assert_eq!(ink_box(&[]), None);
    let blank = [Glyph {
        x: 3,
        y: 4,
        width: 2,
        height: 1,
        coverage: vec![0, 0],
    }];
    assert_eq!(ink_box(&blank), None);
    let inked = [Glyph {
        x: 3,
        y: 4,
        width: 3,
        height: 2,
        coverage: vec![0, 0, 0, 0, 9, 0],
    }];
    assert_eq!(ink_box(&inked), Some((4, 5, 1, 1)));
}

#[test]
fn fit_scale_fits_the_probe_into_the_name_region() {
    assert_eq!(fit_scale(None), PROBE);
    assert_eq!(fit_scale(Some((0, 0, 0, 0))), PROBE);
    assert_eq!(fit_scale(Some((0, 0, 0, 5))), PROBE);
    assert_eq!(fit_scale(Some((0, 0, 5, 0))), PROBE);
    let full = (0, 0, WIDTH - WAVE_PX - 16, HEIGHT - 24);
    assert_eq!(fit_scale(Some(full)), PROBE);
    let half = (0, 0, (WIDTH - WAVE_PX - 16) / 2, (HEIGHT - 24) / 2);
    assert_eq!(fit_scale(Some(half)), 2.0 * PROBE);
    assert_eq!(fit_scale(Some((0, 0, 282, 34))), 200.0);
    assert_eq!(fit_scale(Some((0, 0, 141, 68))), 200.0);
}

/// Ink in a mask at `x >= WAVE_PX`: the name's left, top, width and
/// height. [`None`] when the name region is blank.
fn name_ink_box(bytes: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let stride = WIDTH as usize;
    let wave_px = WAVE_PX as usize;
    let mut left = usize::MAX;
    let mut top = usize::MAX;
    let mut right = 0usize;
    let mut bottom = 0usize;
    for (at, ink) in bytes.iter().enumerate() {
        if *ink == 0 {
            continue;
        }
        let (x, y) = (at % stride, at / stride);
        if x < wave_px {
            continue;
        }
        left = left.min(x);
        top = top.min(y);
        right = right.max(x);
        bottom = bottom.max(y);
    }
    if left > right {
        return None;
    }
    Some((left, top, right - left + 1, bottom - top + 1))
}

#[test]
fn mask_centres_the_test_font_at_exact_pixels() {
    let bytes = mask(&minimal_font((0, 0))).unwrap();
    assert_eq!(name_ink_box(&bytes), Some((68, 11, 564, 137)));
}

#[test]
fn mask_centres_an_off_centre_test_font_at_exact_pixels() {
    // Glyph ids are b, e, f, i, r; the flat f leaves one advance before ink.
    let font_bytes = test_font(
        &[(500, 700), (500, 700), (500, 0), (500, 700), (500, 700)],
        (0, 10),
    );
    let font = FontRef::new(&font_bytes).unwrap();
    let probe = ink_box(&rasterise(&font, PROBE, 0, 112.0)).unwrap();
    let (left, top, _, _) = probe;
    assert_eq!((left, top), (60, 41));

    let scale = fit_scale(Some(probe));
    let ascent = font
        .metrics(Size::new(scale), LocationRef::default())
        .ascent;
    let baseline = ascent + (HEIGHT as f32 - ascent) / 2.0;
    let (_, _, width, height) = ink_box(&rasterise(&font, scale, 0, baseline)).unwrap();
    // The name region has 132 even horizontal and 23 odd vertical pixels spare.
    assert_eq!((WIDTH - WAVE_PX - width, HEIGHT - height), (132, 23));

    let bytes = mask(&font_bytes).unwrap();
    assert_eq!(name_ink_box(&bytes), Some((126, 11, 448, 137)));
}

#[test]
fn rasterise_skips_a_flat_glyph_and_keeps_its_advance() {
    let bytes = test_font(
        &[(500, 0), (500, 700), (500, 700), (500, 700), (500, 700)],
        (0, 0),
    );
    let font = FontRef::new(&bytes).unwrap();
    let glyphs = rasterise(&font, PROBE, WAVE_PX + 8, 112.0);
    assert_eq!(glyphs.len(), 4);
    let before = glyphs.get(1).unwrap();
    let after = glyphs.get(2).unwrap();
    let gap = after.x - (before.x + before.width);
    assert_eq!(gap, 69, "the skipped glyph keeps its advance");
}

#[test]
fn rasterise_skips_a_zero_width_glyph() {
    let bytes = test_font(
        &[(0, 700), (500, 700), (500, 700), (500, 700), (500, 700)],
        (0, 0),
    );
    let font = FontRef::new(&bytes).unwrap();
    let glyphs = rasterise(&font, PROBE, WAVE_PX + 8, 112.0);
    assert_eq!(glyphs.len(), 4);
}

#[test]
fn rasterise_keeps_a_hairline_glyph() {
    let bytes = test_font(
        &[(1, 1), (500, 700), (500, 700), (500, 700), (500, 700)],
        (0, 0),
    );
    let font = FontRef::new(&bytes).unwrap();
    let glyphs = rasterise(&font, PROBE, WAVE_PX + 8, 112.0);
    assert_eq!(glyphs.len(), 5);
    let hairline = glyphs.get(2).unwrap();
    assert_eq!(hairline.width, 2);
    assert_eq!(hairline.height, 1);
}

#[test]
fn pen_shifts_and_flips_a_quadratic_segment() {
    let mut builder = PathBuilder::new();
    {
        let mut pen = Pen {
            builder: &mut builder,
            dx: 10.0,
            dy: 100.0,
        };
        pen.move_to(1.0, 2.0);
        pen.quad_to(3.0, 5.0, 7.0, 11.0);
    }
    let path = builder.finish().unwrap();
    let mut segments = path.segments();
    assert!(
        matches!(segments.next(), Some(PathSegment::MoveTo(end))
            if end.x == 11.0 && end.y == 98.0),
        "the start moves past the caret and above the baseline"
    );
    assert!(
        matches!(segments.next(), Some(PathSegment::QuadTo(control, end))
            if control.x == 13.0
                && control.y == 95.0
                && end.x == 17.0
                && end.y == 89.0),
        "the control point and the end move with the start"
    );
    assert!(segments.next().is_none());
}

#[test]
fn pen_shifts_and_flips_a_line_segment() {
    let mut builder = PathBuilder::new();
    {
        let mut pen = Pen {
            builder: &mut builder,
            dx: 10.0,
            dy: 100.0,
        };
        pen.move_to(1.0, 2.0);
        pen.line_to(7.0, 11.0);
    }
    let path = builder.finish().unwrap();
    let mut segments = path.segments();
    assert!(
        matches!(segments.next(), Some(PathSegment::MoveTo(end)) if end.x == 11.0 && end.y == 98.0)
    );
    assert!(
        matches!(segments.next(), Some(PathSegment::LineTo(end)) if end.x == 17.0 && end.y == 89.0)
    );
    assert!(segments.next().is_none());
}

#[test]
fn pen_closes_a_contour() {
    let mut builder = PathBuilder::new();
    {
        let mut pen = Pen {
            builder: &mut builder,
            dx: 10.0,
            dy: 100.0,
        };
        pen.move_to(1.0, 2.0);
        pen.line_to(7.0, 11.0);
        pen.close();
    }
    let path = builder.finish().unwrap();
    let segments: Vec<_> = path.segments().collect();
    assert!(matches!(segments.last(), Some(PathSegment::Close)));
}

#[test]
fn pen_shifts_and_flips_a_cubic_segment() {
    let mut builder = PathBuilder::new();
    {
        let mut pen = Pen {
            builder: &mut builder,
            dx: 10.0,
            dy: 100.0,
        };
        pen.move_to(1.0, 2.0);
        pen.curve_to(3.0, 5.0, 7.0, 11.0, 13.0, 17.0);
    }
    let path = builder.finish().unwrap();
    let mut segments = path.segments();
    assert!(
        matches!(segments.next(), Some(PathSegment::MoveTo(end))
            if end.x == 11.0 && end.y == 98.0),
        "the start moves past the caret and above the baseline"
    );
    assert!(
        matches!(
            segments.next(),
            Some(PathSegment::CubicTo(first, second, end))
                if first.x == 13.0
                    && first.y == 95.0
                    && second.x == 17.0
                    && second.y == 89.0
                    && end.x == 23.0
                    && end.y == 83.0
        ),
        "both control points and the end move with the start"
    );
    assert!(segments.next().is_none());
}

#[test]
fn mask_rejects_garbage_font_bytes() {
    assert!(mask(b"not a font").is_err());
    assert!(mask(b"").is_err());
}

#[test]
fn checked_in_mask_is_the_right_size_with_ink_in_both_regions() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/tui/assets/logo-mask.bin");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len(), WIDTH as usize * HEIGHT as usize);
    let stride = WIDTH as usize;
    let wave_px = WAVE_PX as usize;
    let inked = |from: usize, to: usize| {
        bytes
            .iter()
            .enumerate()
            .any(|(at, ink)| at % stride >= from && at % stride < to && *ink != 0)
    };
    assert!(inked(0, wave_px), "no ink in the wave region");
    assert!(inked(wave_px, stride), "no ink in the name region");
}

#[test]
fn run_writes_a_sized_mask_with_ink_in_both_regions() {
    let dir = fakes::TempDir::new("fiber-logo-mask");
    let font = dir.path().join("test-font.ttf");
    std::fs::write(&font, minimal_font((0, 0))).unwrap();
    let first = dir.path().join("first/logo-mask.bin");
    let second = dir.path().join("second/logo-mask.bin");
    for out in [&first, &second] {
        let args = [
            "--font".to_owned(),
            font.to_str().unwrap().to_owned(),
            "--out".to_owned(),
            out.to_str().unwrap().to_owned(),
        ];
        assert!(run(&args).unwrap());
    }
    let bytes = std::fs::read(&first).unwrap();
    assert_eq!(bytes.len(), WIDTH as usize * HEIGHT as usize);
    let stride = WIDTH as usize;
    let wave_px = WAVE_PX as usize;
    let inked = |from: usize, to: usize| {
        bytes
            .iter()
            .enumerate()
            .any(|(at, ink)| at % stride >= from && at % stride < to && *ink != 0)
    };
    assert!(inked(0, wave_px), "no ink in the wave region");
    assert!(inked(wave_px, stride), "no ink in the name region");
    let again = std::fs::read(&second).unwrap();
    assert_eq!(bytes.len(), again.len());
    let differs = bytes.iter().zip(&again).position(|(a, b)| a != b);
    assert_eq!(differs, None, "first differing offset");
}

/// A minimal valid TrueType font with five filled-rectangle glyphs, one
/// each for `b`, `e`, `f`, `i` and `r`: the `--font` the `run` test feeds
/// the command entry point. The repo bundles no font, so the test builds
/// one from its tables: head, hhea, maxp, hmtx, cmap format 12, short
/// loca and glyf. Checksums are zero, which the parser does not verify.
fn minimal_font(origin: (i16, i16)) -> Vec<u8> {
    test_font(
        &[(500, 700), (500, 700), (500, 700), (500, 700), (500, 700)],
        origin,
    )
}

/// A font like [`minimal_font`], but each of `b`, `e`, `f`, `i` and `r` in
/// glyph-id order gets a `width`-by-`height` filled rectangle in font
/// units at `origin`. A zero side is a degenerate contour the rasteriser
/// skips; a one-unit side is a hairline it keeps.
fn test_font(sizes: &[(i16, i16)], origin: (i16, i16)) -> Vec<u8> {
    /// One `width`-by-`height` filled rectangle at `origin`: a single
    /// contour through four on-curve corners, stored as 16-bit deltas.
    fn rectangle(width_units: i16, height_units: i16, origin: (i16, i16)) -> Vec<u8> {
        let mut glyph = Vec::new();
        glyph.extend_from_slice(&1u16.to_be_bytes());
        for edge in [
            origin.0,
            origin.1,
            origin.0 + width_units,
            origin.1 + height_units,
        ] {
            glyph.extend_from_slice(&edge.to_be_bytes());
        }
        for word in [3u16, 0] {
            glyph.extend_from_slice(&word.to_be_bytes());
        }
        glyph.extend_from_slice(&[1u8, 1, 1, 1]);
        for delta in [
            origin.0,
            width_units,
            0,
            -width_units,
            origin.1,
            0,
            height_units,
            0,
        ] {
            glyph.extend_from_slice(&delta.to_be_bytes());
        }
        glyph
    }

    let mut head = Vec::new();
    for word in [1u16, 0] {
        head.extend_from_slice(&word.to_be_bytes());
    }
    head.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    head.extend_from_slice(&0u32.to_be_bytes());
    head.extend_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    for word in [0u16, 1000] {
        head.extend_from_slice(&word.to_be_bytes());
    }
    head.extend_from_slice(&0i64.to_be_bytes());
    head.extend_from_slice(&0i64.to_be_bytes());
    for word in [0i16, 0, 500, 700] {
        head.extend_from_slice(&word.to_be_bytes());
    }
    for word in [0u16, 3] {
        head.extend_from_slice(&word.to_be_bytes());
    }
    for word in [2i16, 0, 0] {
        head.extend_from_slice(&word.to_be_bytes());
    }

    let mut hhea = Vec::new();
    for word in [1u16, 0] {
        hhea.extend_from_slice(&word.to_be_bytes());
    }
    for word in [800i16, -200, 0] {
        hhea.extend_from_slice(&word.to_be_bytes());
    }
    hhea.extend_from_slice(&600u16.to_be_bytes());
    for word in [0i16, 0, 500] {
        hhea.extend_from_slice(&word.to_be_bytes());
    }
    for word in [1i16, 0, 0, 0, 0, 0, 0] {
        hhea.extend_from_slice(&word.to_be_bytes());
    }
    hhea.extend_from_slice(&0i16.to_be_bytes());
    hhea.extend_from_slice(&6u16.to_be_bytes());

    let mut maxp = Vec::new();
    maxp.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    maxp.extend_from_slice(&6u16.to_be_bytes());
    for word in [8u16, 2, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0] {
        maxp.extend_from_slice(&word.to_be_bytes());
    }

    let mut hmtx = Vec::new();
    for _ in 0..6 {
        hmtx.extend_from_slice(&600u16.to_be_bytes());
        hmtx.extend_from_slice(&0i16.to_be_bytes());
    }

    let mut cmap = Vec::new();
    for word in [0u16, 1, 3, 1] {
        cmap.extend_from_slice(&word.to_be_bytes());
    }
    cmap.extend_from_slice(&12u32.to_be_bytes());
    cmap.extend_from_slice(&12u16.to_be_bytes());
    cmap.extend_from_slice(&0u16.to_be_bytes());
    cmap.extend_from_slice(&76u32.to_be_bytes());
    cmap.extend_from_slice(&0u32.to_be_bytes());
    cmap.extend_from_slice(&5u32.to_be_bytes());
    for (start, glyph) in [(0x62u32, 1u32), (0x65, 2), (0x66, 3), (0x69, 4), (0x72, 5)] {
        cmap.extend_from_slice(&start.to_be_bytes());
        cmap.extend_from_slice(&start.to_be_bytes());
        cmap.extend_from_slice(&glyph.to_be_bytes());
    }

    let empty = vec![0u8; 10];
    let rects: Vec<Vec<u8>> = sizes
        .iter()
        .map(|(width, height)| rectangle(*width, *height, origin))
        .collect();
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    let mut at = 0u16;
    let mut pieces: Vec<&[u8]> = vec![&empty];
    pieces.extend(rects.iter().map(Vec::as_slice));
    for piece in pieces {
        // Short loca offsets are byte offsets divided by two.
        loca.extend_from_slice(&(at / 2).to_be_bytes());
        glyf.extend_from_slice(piece);
        at += u16::try_from(piece.len()).unwrap();
    }
    loca.extend_from_slice(&(at / 2).to_be_bytes());

    let tables: [(&[u8; 4], Vec<u8>); 7] = [
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    let mut offset = 124u32;
    let mut starts = Vec::new();
    for table in &tables {
        starts.push(offset);
        offset += u32::try_from(table.1.len()).unwrap();
        offset = offset.next_multiple_of(4);
    }
    let mut font = Vec::new();
    font.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    font.extend_from_slice(&7u16.to_be_bytes());
    font.extend_from_slice(&64u16.to_be_bytes());
    font.extend_from_slice(&2u16.to_be_bytes());
    font.extend_from_slice(&48u16.to_be_bytes());
    for (table, start) in tables.iter().zip(starts.iter()) {
        font.extend_from_slice(table.0);
        font.extend_from_slice(&0u32.to_be_bytes());
        font.extend_from_slice(&start.to_be_bytes());
        font.extend_from_slice(&u32::try_from(table.1.len()).unwrap().to_be_bytes());
    }
    for (table, start) in tables.iter().zip(starts.iter()) {
        while u32::try_from(font.len()).unwrap() < *start {
            font.push(0);
        }
        font.extend_from_slice(&table.1);
    }
    font
}

#[test]
fn usage_names_the_flags_and_font_download() {
    let text = usage();
    assert!(text.starts_with("usage:"));
    assert!(text.contains("--font"));
    assert!(text.contains("--out"));
    assert!(text.contains("https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304"));
}

#[test]
fn parse_takes_font_with_the_default_out() {
    let config = parse(&flag(&["--font", "JetBrainsMono-ExtraBold.ttf"])).unwrap();
    assert_eq!(config.font, "JetBrainsMono-ExtraBold.ttf");
    assert_eq!(config.out, DEFAULT_OUT);
}

#[test]
fn parse_takes_an_explicit_out() {
    let config = parse(&flag(&["--font", "f.ttf", "--out", "out/logo.bin"])).unwrap();
    assert_eq!(config.font, "f.ttf");
    assert_eq!(config.out, "out/logo.bin");
}

#[test]
fn parse_missing_font_is_an_error() {
    assert!(parse(&flag(&["--out", "out/logo.bin"])).is_err());
    assert!(parse(&flag(&[])).is_err());
}

#[test]
fn parse_rejects_a_flag_without_its_value_and_an_unknown_flag() {
    assert!(parse(&flag(&["--font"])).is_err());
    assert!(parse(&flag(&["--font", "f.ttf", "--bogus", "x"])).is_err());
}

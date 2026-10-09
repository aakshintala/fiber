use super::*;

fn flag(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
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
    std::fs::write(&font, minimal_font()).unwrap();
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
    assert_eq!(bytes, std::fs::read(&second).unwrap());
}

/// A minimal valid TrueType font with five filled-rectangle glyphs, one
/// each for `b`, `e`, `f`, `i` and `r`: the `--font` the `run` test feeds
/// the command entry point. The repo bundles no font, so the test builds
/// one from its tables: head, hhea, maxp, hmtx, cmap format 12, short
/// loca and glyf. Checksums are zero, which the parser does not verify.
fn minimal_font() -> Vec<u8> {
    /// One 500-by-700 filled rectangle: a single contour through four
    /// on-curve corners, stored as 16-bit deltas.
    fn rectangle() -> Vec<u8> {
        let mut glyph = Vec::new();
        for word in [1u16, 0, 0, 500, 700, 3, 0] {
            glyph.extend_from_slice(&word.to_be_bytes());
        }
        glyph.extend_from_slice(&[1u8, 1, 1, 1]);
        for delta in [0i16, 500, 0, -500, 0, 0, 700, 0] {
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
    let rect = rectangle();
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    let mut at = 0u16;
    for piece in [&empty, &rect, &rect, &rect, &rect, &rect] {
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

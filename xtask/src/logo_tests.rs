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

//! The child through its public entry point, on the process's own streams.

use std::ffi::OsString;

#[test]
fn a_usage_error_returns_2() {
    assert_eq!(picture::main(vec![OsString::from("only-one")]), 2);
}

#[test]
fn a_pdf_is_cut_and_the_entry_returns_0() {
    let dir = fakes::TempDir::new("fiber-picture-entry-pdf");
    let input = dir.path().join("in.pdf");
    std::fs::copy(
        format!("{}/../../research/pdf-tool-results/text.pdf", env!("CARGO_MANIFEST_DIR")),
        &input,
    )
    .unwrap();
    let code = picture::main(vec![
        OsString::from("pdf"),
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("p_entry"),
        OsString::from("whole=10"),
    ]);
    assert_eq!(code, 0);
    assert!(dir.path().join("p_entry.pdf").exists());
}

#[test]
fn an_image_is_stored_and_the_entry_returns_0() {
    let dir = fakes::TempDir::new("fiber-picture-entry");
    let input = dir.path().join("in.png");
    let image = image::RgbImage::from_pixel(30, 20, image::Rgb([9, 9, 9]));
    image.save(&input).unwrap();
    let code = picture::main(vec![
        input.into_os_string(),
        dir.path().as_os_str().to_owned(),
        OsString::from("i_entry"),
    ]);
    assert_eq!(code, 0);
    assert!(dir.path().join("i_entry.png").exists());
}

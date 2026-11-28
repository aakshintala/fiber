use super::LayoutRect;

#[test]
fn layout_rect_keeps_signed_coordinates_and_dimensions() {
    let rect = LayoutRect {
        x: -1,
        y: 2,
        width: 3,
        height: 4,
    };

    assert_eq!(rect.x, -1);
    assert_eq!(rect.y, 2);
    assert_eq!(rect.width, 3);
    assert_eq!(rect.height, 4);
}

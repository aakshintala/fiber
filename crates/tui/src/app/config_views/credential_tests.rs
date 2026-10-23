use super::*;
use ratatui::{buffer::Buffer, layout::Rect};

fn labels(current: Option<&str>) -> CredentialLabels {
    CredentialLabels {
        provider: "fake".to_owned(),
        credentials: ["default", "home", "work"].map(str::to_owned).to_vec(),
        credential: current.map(str::to_owned),
    }
}

fn ready(current: Option<&str>) -> Picker {
    let mut picker = Picker::open("q".to_owned());
    picker.answered("q", labels(current), 24);
    picker
}

#[test]
fn reading_stale_answers_and_rejections() {
    let mut picker = Picker::open("q".to_owned());
    assert!(picker.frame().rows.is_empty());
    assert_eq!(picker.frame().below, ["Reading the labels…"]);
    picker.answered("old", labels(Some("work")), 24);
    assert!(picker.frame().rows.is_empty());
    picker.rejected("old", "wrong");
    assert_eq!(picker.frame().below, ["Reading the labels…"]);
    picker.rejected("q", "not available");
    assert_eq!(picker.frame().below, ["not available"]);
    picker.answered("q", labels(None), 24);
    assert!(picker.frame().rows.is_empty());
}

#[test]
fn answer_order_current_selection_and_marks() {
    for (index, current) in ["default", "home", "work"].iter().enumerate() {
        let picker = ready(Some(current));
        let frame = picker.frame();
        assert_eq!(frame.list.selected(), index);
        assert_eq!(frame.below, ["fake"]);
        for (at, row) in frame.rows.iter().enumerate() {
            assert_eq!(row[0].0, ["default", "home", "work"][at]);
            assert_eq!(row.len(), if at == index { 2 } else { 1 });
            if at == index {
                assert_eq!(row[1].0, "  ● current");
                assert_eq!(row[1].2, Ink::Role(crate::theme::Role::Accent));
            }
        }
    }
    let picker = ready(None);
    assert_eq!(picker.frame().list.selected(), 0);
    assert!(picker.frame().rows.iter().all(|row| row.len() == 1));
}

#[test]
fn empty_answer_and_selection_edges() {
    let mut picker = ready(Some("home"));
    assert_eq!(picker.key(&Key::Down, 24), Pick::Stay);
    assert_eq!(picker.frame().list.selected(), 2);
    assert_eq!(picker.key(&Key::Down, 24), Pick::Stay);
    assert_eq!(picker.frame().list.selected(), 2);
    for selected in [1, 0, 0] {
        picker.key(&Key::Up, 24);
        assert_eq!(picker.frame().list.selected(), selected);
    }
    let mut empty = Picker::open("q".to_owned());
    empty.answered(
        "q",
        CredentialLabels {
            provider: "scripted".to_owned(),
            credentials: vec![],
            credential: None,
        },
        24,
    );
    assert!(empty.frame().rows.is_empty());
    assert_eq!(empty.frame().below, ["scripted has no credential labels"]);
    assert_eq!(empty.key(&Key::Enter, 24), Pick::Stay);
    assert_eq!(empty.session_only(), Pick::Stay);
    assert_eq!(empty.click(Spot::Row(0), 24), Pick::Stay);
}

#[test]
fn keys_clicks_and_session_only() {
    let mut picker = ready(Some("home"));
    assert_eq!(picker.key(&Key::Char('w'), 24), Pick::Stay);
    assert_eq!(picker.frame().list.selected(), 1);
    assert_eq!(
        picker.key(&Key::Enter, 24),
        Pick::Choose {
            label: "home".to_owned(),
            session_only: false
        }
    );
    assert_eq!(
        picker.session_only(),
        Pick::Choose {
            label: "home".to_owned(),
            session_only: true
        }
    );
    assert_eq!(
        picker.click(Spot::Row(2), 24),
        Pick::Choose {
            label: "work".to_owned(),
            session_only: false
        }
    );
    assert_eq!(picker.click(Spot::Row(3), 24), Pick::Stay);
    assert_eq!(picker.click(Spot::Row(4), 24), Pick::Stay);
    assert_eq!(picker.click(Spot::Close, 24), Pick::Close);
    assert_eq!(picker.key(&Key::Esc, 24), Pick::Close);
    assert_eq!(picker.click(Spot::Cell(0, 0), 24), Pick::Stay);
}

#[test]
fn only_waiting_pickers_accept_an_answer_or_rejection() {
    let mut waiting = Picker::open("q".to_owned());
    let mut rejected = Picker::open("q".to_owned());
    rejected.rejected("q", "refused");
    for picker in [&mut waiting, &mut rejected] {
        assert_eq!(picker.key(&Key::Down, 24), Pick::Stay);
        assert_eq!(picker.key(&Key::Enter, 24), Pick::Stay);
        assert_eq!(picker.session_only(), Pick::Stay);
        assert_eq!(picker.click(Spot::Row(0), 24), Pick::Stay);
        assert_eq!(picker.frame().list.selected(), 0);
    }
    rejected.answered("q", labels(Some("work")), 24);
    rejected.rejected("q", "replacement");
    assert_eq!(rejected.frame().below, ["refused"]);
    assert!(rejected.frame().rows.is_empty());

    let mut answered = ready(Some("home"));
    answered.answered("q", labels(Some("work")), 24);
    answered.rejected("q", "refused");
    assert_eq!(answered.frame().below, ["fake"]);
    assert_eq!(answered.frame().list.selected(), 1);
    assert_eq!(
        answered.session_only(),
        Pick::Choose {
            label: "home".to_owned(),
            session_only: true,
        }
    );
}

#[test]
fn a_wait_slot_is_taken_only_by_its_matching_id() {
    let mut waiting = Some(Save {
        id: "new".to_owned(),
        label: "work".to_owned(),
    });
    assert!(Waits::take(&mut waiting, "old", |save| &save.id).is_none());
    assert_eq!(waiting.as_ref().unwrap().label, "work");
    let taken = Waits::take(&mut waiting, "new", |save| &save.id).unwrap();
    assert_eq!(taken.label, "work");
    assert!(waiting.is_none());
    assert!(Waits::take(&mut waiting, "new", |save| &save.id).is_none());
}

#[test]
fn credentials_80x24() {
    let picker = ready(Some("home"));
    let area = Rect::new(0, 0, 80, 24);
    let mut buffer = Buffer::empty(area);
    crate::swapped::render(&picker.frame(), area, &mut buffer, &mut vec![]);
    let lines: Vec<String> = (0..24)
        .map(|y| {
            (0..80)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    assert_eq!(lines[1], "default");
    assert_eq!(lines[2], "home  ● current");
    assert_eq!(lines[3], "work");
    assert_eq!(lines[22], "fake");
    assert_eq!(
        lines[23],
        "↑↓ move · Enter choose · Ctrl+S this session · Esc close"
    );
    insta::assert_snapshot!(lines.join("\n"));
}

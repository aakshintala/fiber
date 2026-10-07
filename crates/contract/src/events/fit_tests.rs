//! `Interaction::fit`'s table (`docs/invocation.md`, "Replying"): each
//! answer fits only its kind, `declined` fits every kind, and an approval
//! or an offer's answer fits nothing.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use crate::commands::{ReplyAnswer, SentFormAnswer};
use crate::events::{Answer, Interaction};
use crate::shapes::{Choice, Question, True};

fn choice(label: &str) -> Choice {
    Choice {
        label: label.into(),
        description: None,
    }
}

fn field(multi: bool) -> Question {
    Question {
        header: "Model".into(),
        question: "Which?".into(),
        options: vec![choice("a"), choice("b")],
        multi_select: multi.then_some(true),
    }
}

fn asked(kind: &str) -> Interaction {
    match kind {
        "confirm" => Interaction::Confirm {
            prompt: "go?".into(),
        },
        "select" => Interaction::Select {
            prompt: "go?".into(),
            options: vec![
                choice("a"),
                Choice {
                    label: "b".into(),
                    description: Some("d".into()),
                },
            ],
        },
        "multi_select" => Interaction::MultiSelect {
            prompt: "go?".into(),
            options: vec![choice("a"), choice("b")],
        },
        "text_input" => Interaction::TextInput {
            prompt: "go?".into(),
        },
        "form" => Interaction::Form {
            fields: vec![field(false)],
        },
        _ => unreachable!(),
    }
}

fn declined() -> ReplyAnswer {
    ReplyAnswer::Declined { declined: True }
}

fn approval() -> ReplyAnswer {
    ReplyAnswer::Approval {
        decision: crate::events::Decision::Allow,
        feedback: None,
        remember: None,
    }
}

#[test]
fn declined_fits_every_kind() {
    for kind in ["confirm", "select", "multi_select", "text_input", "form"] {
        assert_eq!(
            asked(kind).fit(&declined()),
            Some(Answer::Declined { declined: True })
        );
    }
}

#[test]
fn every_kind_against_every_answer_variant() {
    let confirmed = ReplyAnswer::Confirmed { confirmed: true };
    let one = ReplyAnswer::Labels {
        labels: vec!["a".into()],
    };
    let text = ReplyAnswer::Text { text: "x".into() };
    let form = ReplyAnswer::Form {
        answers: vec![SentFormAnswer::Answered {
            labels: vec!["a".into()],
            text: None,
        }],
        note: None,
    };
    let cases: &[(&str, &ReplyAnswer, bool)] = &[
        ("confirm", &confirmed, true),
        ("confirm", &one, false),
        ("confirm", &text, false),
        ("confirm", &form, false),
        ("select", &confirmed, false),
        ("select", &one, true),
        ("select", &text, false),
        ("select", &form, false),
        ("multi_select", &confirmed, false),
        ("multi_select", &one, true),
        ("multi_select", &text, false),
        ("multi_select", &form, false),
        ("text_input", &confirmed, false),
        ("text_input", &one, false),
        ("text_input", &text, true),
        ("text_input", &form, false),
        ("form", &confirmed, false),
        ("form", &one, false),
        ("form", &text, false),
        ("form", &form, true),
    ];
    for (kind, answer, fits) in cases {
        assert_eq!(
            asked(kind).fit(answer).is_some(),
            *fits,
            "{kind} against {answer:?}"
        );
    }
}

#[test]
fn select_takes_exactly_one_label() {
    let asked = asked("select");
    assert!(asked.fit(&ReplyAnswer::Labels { labels: vec![] }).is_none());
    assert!(
        asked
            .fit(&ReplyAnswer::Labels {
                labels: vec!["a".into(), "b".into()]
            })
            .is_none()
    );
    assert_eq!(
        asked.fit(&ReplyAnswer::Labels {
            labels: vec!["a".into()]
        }),
        Some(Answer::Labels {
            labels: vec!["a".into()]
        })
    );
}

#[test]
fn a_label_not_offered_fits_nothing() {
    let asked = asked("select");
    assert!(
        asked
            .fit(&ReplyAnswer::Labels {
                labels: vec!["z".into()]
            })
            .is_none()
    );
}

#[test]
fn multi_select_takes_distinct_offered_labels() {
    let asked = asked("multi_select");
    assert_eq!(
        asked.fit(&ReplyAnswer::Labels { labels: vec![] }),
        Some(Answer::Labels { labels: vec![] })
    );
    assert!(
        asked
            .fit(&ReplyAnswer::Labels {
                labels: vec!["a".into(), "b".into()]
            })
            .is_some()
    );
    assert!(
        asked
            .fit(&ReplyAnswer::Labels {
                labels: vec!["a".into(), "a".into()]
            })
            .is_none()
    );
    assert!(
        asked
            .fit(&ReplyAnswer::Labels {
                labels: vec!["z".into()]
            })
            .is_none()
    );
}

#[test]
fn form_answers_match_the_fields_in_order() {
    let asked = asked("form");
    let one = || SentFormAnswer::Answered {
        labels: vec!["a".into()],
        text: None,
    };
    assert!(
        asked
            .fit(&ReplyAnswer::Form {
                answers: vec![],
                note: None
            })
            .is_none()
    );
    assert!(
        asked
            .fit(&ReplyAnswer::Form {
                answers: vec![one(), one()],
                note: None
            })
            .is_none()
    );
    let fitted = asked.fit(&ReplyAnswer::Form {
        answers: vec![one()],
        note: Some("note".into()),
    });
    assert_eq!(
        fitted,
        Some(Answer::Form {
            answers: vec![crate::events::FormAnswer::Answered {
                labels: vec!["a".into()],
                text: None
            }],
            note: Some("note".into())
        })
    );
}

#[test]
fn a_skipped_field_fits() {
    let fitted = asked("form").fit(&ReplyAnswer::Form {
        answers: vec![SentFormAnswer::Skipped { skipped: True }],
        note: None,
    });
    assert_eq!(
        fitted,
        Some(Answer::Form {
            answers: vec![crate::events::FormAnswer::Skipped { skipped: True }],
            note: None
        })
    );
}

#[test]
fn at_most_one_label_unless_multi_select() {
    let single = Interaction::Form {
        fields: vec![field(false)],
    };
    let multi = Interaction::Form {
        fields: vec![field(true)],
    };
    let two = || SentFormAnswer::Answered {
        labels: vec!["a".into(), "b".into()],
        text: None,
    };
    assert!(
        single
            .fit(&ReplyAnswer::Form {
                answers: vec![two()],
                note: None
            })
            .is_none()
    );
    assert!(
        multi
            .fit(&ReplyAnswer::Form {
                answers: vec![two()],
                note: None
            })
            .is_some()
    );
}

#[test]
fn a_label_from_another_field_fits_nothing() {
    let asked = Interaction::Form {
        fields: vec![field(false), field(false)],
    };
    // Only the labels are per-field here; a label the field never offered
    // is refused on any field.
    assert!(
        asked
            .fit(&ReplyAnswer::Form {
                answers: vec![
                    SentFormAnswer::Answered {
                        labels: vec!["z".into()],
                        text: None
                    },
                    SentFormAnswer::Skipped { skipped: True },
                ],
                note: None
            })
            .is_none()
    );
}

#[test]
fn text_with_labels_on_a_form_field_round_trips() {
    let fitted = asked("form").fit(&ReplyAnswer::Form {
        answers: vec![SentFormAnswer::Answered {
            labels: vec!["a".into()],
            text: Some("extra".into()),
        }],
        note: None,
    });
    assert_eq!(
        fitted,
        Some(Answer::Form {
            answers: vec![crate::events::FormAnswer::Answered {
                labels: vec!["a".into()],
                text: Some("extra".into())
            }],
            note: None
        })
    );
}

#[test]
fn an_approval_answer_fits_nothing() {
    for kind in ["confirm", "select", "multi_select", "text_input", "form"] {
        assert_eq!(asked(kind).fit(&approval()), None);
    }
}

#[test]
fn an_offers_answer_fits_nothing() {
    let decisions = ReplyAnswer::Decisions {
        decisions: vec![crate::events::OfferDecision::Approve],
    };
    for kind in ["confirm", "select", "multi_select", "text_input", "form"] {
        assert_eq!(asked(kind).fit(&decisions), None);
    }
}

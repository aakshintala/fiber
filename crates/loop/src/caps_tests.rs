//! `capped` applies each configured `tools."<name>".max_result_bytes` to the
//! tool registered under that name.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use contract::emit::Emit;
use contract::provider::ToolDefinition;
use contract::shapes::{ContentPart, DeclaredEffects};
use contract::tool::{Answered, Ask, Asking, Bound, Cancel, Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use super::{ResultCaps, capped};

/// A tool with a settable name, bound, guidelines, output and `with_cap`
/// answer, recording what `with_cap` was asked.
struct Fixed {
    name: String,
    bound: Bound,
    output: Output,
    with_cap_answer: Option<Arc<dyn Tool>>,
    with_cap_calls: Mutex<Vec<usize>>,
}

impl Fixed {
    fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            bound: Bound::DEFAULT,
            output: Output {
                content: vec![ContentPart::Text {
                    text: "done".to_owned(),
                }],
                ..Output::default()
            },
            with_cap_answer: None,
            with_cap_calls: Mutex::new(Vec::new()),
        }
    }

    fn with_cap_calls(&self) -> Vec<usize> {
        self.with_cap_calls.lock().unwrap().clone()
    }
}

impl Tool for Fixed {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: format!("The {} tool.", self.name),
            input_schema: json!({}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(&self, _arguments: &Map<String, Value>) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: Vec::new(),
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
        })
    }

    fn run(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
    ) -> Output {
        self.output.clone()
    }

    /// Answers with the asker's call id, so a test sees which asker came.
    fn run_asking(
        &self,
        _arguments: &Map<String, Value>,
        _cancel: &dyn Cancel,
        _emit: &dyn Emit,
        ask: &dyn Ask,
    ) -> Output {
        Output {
            details: Some(json!(ask.action().0)),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        self.bound
    }

    fn guidelines(&self) -> Option<String> {
        Some("g".to_owned())
    }

    fn with_cap(&self, cap: usize) -> Option<Arc<dyn Tool>> {
        self.with_cap_calls.lock().unwrap().push(cap);
        self.with_cap_answer.clone()
    }
}

fn tool(name: &str, bound: Bound) -> Arc<Fixed> {
    let mut fixed = Fixed::named(name);
    fixed.bound = bound;
    Arc::new(fixed)
}

fn caps(entries: &[(&str, u64)]) -> ResultCaps {
    entries
        .iter()
        .map(|(name, cap)| ((*name).to_owned(), *cap))
        .collect::<BTreeMap<_, _>>()
}

fn wrap(name: &str, inner: Arc<Fixed>) -> (String, Arc<dyn Tool>) {
    (name.to_owned(), inner as Arc<dyn Tool>)
}

#[test]
fn a_cap_on_a_start_only_tool_keeps_that_many_bytes_of_the_start() {
    let tools: Vec<(String, Arc<dyn Tool>)> = vec![wrap(
        "cat",
        tool(
            "cat",
            Bound {
                start: 32_768,
                end: 0,
            },
        ),
    )];
    let out = capped(tools, &caps(&[("cat", 100)]));
    assert_eq!(out[0].1.bound(), Bound { start: 100, end: 0 });
}

#[test]
fn a_cap_on_a_both_ends_tool_keeps_its_proportions_and_gives_the_odd_byte_to_the_head() {
    let tools: Vec<(String, Arc<dyn Tool>)> = vec![wrap(
        "shell",
        tool(
            "shell",
            Bound {
                start: 8192,
                end: 8192,
            },
        ),
    )];
    let out = capped(tools, &caps(&[("shell", 1001)]));
    assert_eq!(
        out[0].1.bound(),
        Bound {
            start: 501,
            end: 500
        }
    );
}

#[test]
fn an_uneven_both_ends_tool_keeps_its_proportions() {
    let tools: Vec<(String, Arc<dyn Tool>)> =
        vec![wrap("cat", tool("cat", Bound { start: 3, end: 1 }))];
    let out = capped(tools, &caps(&[("cat", 10)]));
    assert_eq!(out[0].1.bound(), Bound { start: 8, end: 2 });
}

#[test]
fn a_tool_that_declares_no_bytes_keeps_the_start() {
    let tools: Vec<(String, Arc<dyn Tool>)> =
        vec![wrap("cat", tool("cat", Bound { start: 0, end: 0 }))];
    let out = capped(tools, &caps(&[("cat", 10)]));
    assert_eq!(out[0].1.bound(), Bound { start: 10, end: 0 });
}

#[test]
fn a_zero_cap_keeps_nothing() {
    let tools: Vec<(String, Arc<dyn Tool>)> = vec![wrap(
        "shell",
        tool(
            "shell",
            Bound {
                start: 8192,
                end: 8192,
            },
        ),
    )];
    let out = capped(tools, &caps(&[("shell", 0)]));
    assert_eq!(out[0].1.bound(), Bound { start: 0, end: 0 });
}

#[test]
fn a_cap_past_usize_is_clamped_and_never_wraps() {
    let tools: Vec<(String, Arc<dyn Tool>)> = vec![wrap(
        "shell",
        tool(
            "shell",
            Bound {
                start: 8192,
                end: 8192,
            },
        ),
    )];
    let out = capped(tools, &caps(&[("shell", u64::MAX)]));
    let bound = out[0].1.bound();
    assert_eq!(bound.start.checked_add(bound.end), Some(usize::MAX));
    assert!(bound.start >= bound.end);
}

#[test]
fn a_tool_with_no_cap_keeps_its_own_bound() {
    let tools: Vec<(String, Arc<dyn Tool>)> =
        vec![wrap("cat", tool("cat", Bound { start: 10, end: 20 }))];
    let out = capped(tools, &caps(&[("other", 5)]));
    assert_eq!(out[0].1.bound(), Bound { start: 10, end: 20 });
}

#[test]
fn the_cap_is_found_by_the_registered_name_not_the_registrant() {
    let bound = Bound { start: 10, end: 0 };
    let by_registrant = capped(vec![wrap("cat", tool("dog", bound))], &caps(&[("cat", 5)]));
    assert_eq!(by_registrant[0].1.bound(), bound);
    let by_name = capped(vec![wrap("cat", tool("dog", bound))], &caps(&[("dog", 5)]));
    assert_eq!(by_name[0].1.bound(), Bound { start: 5, end: 0 });
}

#[test]
fn a_tool_that_cuts_its_own_output_is_used_as_it_returns_itself() {
    let cutting = Arc::new({
        let mut cutting = Fixed::named("cat");
        cutting.bound = Bound { start: 10, end: 20 };
        cutting.with_cap_answer = Some(tool("cat", Bound { start: 77, end: 0 }) as Arc<dyn Tool>);
        cutting
    });
    let tools: Vec<(String, Arc<dyn Tool>)> =
        vec![("cat".to_owned(), cutting.clone() as Arc<dyn Tool>)];
    let out = capped(tools, &caps(&[("cat", 5)]));
    assert_eq!(out[0].1.bound(), Bound { start: 77, end: 0 });
    assert_eq!(cutting.with_cap_calls(), vec![5]);
}

#[test]
fn a_tool_that_cuts_its_own_output_keeps_its_registered_name() {
    let cutting = Arc::new({
        let mut cutting = Fixed::named("cat");
        cutting.with_cap_answer = Some(tool("cat", Bound { start: 77, end: 0 }) as Arc<dyn Tool>);
        cutting
    });
    let tools: Vec<(String, Arc<dyn Tool>)> =
        vec![("ext".to_owned(), cutting.clone() as Arc<dyn Tool>)];
    let out = capped(tools, &caps(&[("cat", 5)]));
    assert_eq!(out[0].0, "ext");
    assert_eq!(out[0].1.definition().name, "cat");
    let cutting = Arc::new({
        let mut cutting = Fixed::named("cat");
        cutting.with_cap_answer = Some(tool("cat", Bound { start: 77, end: 0 }) as Arc<dyn Tool>);
        cutting
    });
    let tools: Vec<(String, Arc<dyn Tool>)> =
        vec![("ext".to_owned(), cutting.clone() as Arc<dyn Tool>)];
    let out = capped(tools, &caps(&[("ext", 5)]));
    assert_eq!(out[0].0, "ext");
    assert_eq!(out[0].1.definition().name, "cat");
    assert!(cutting.with_cap_calls().is_empty());
}

#[test]
fn a_capped_tool_answers_as_the_tool_it_wraps() {
    let mut inner = Fixed::named("cat");
    inner.bound = Bound { start: 10, end: 20 };
    inner.output = Output {
        content: vec![ContentPart::Text {
            text: "inner".to_owned(),
        }],
        ..Output::default()
    };
    let inner = Arc::new(inner);
    let tools: Vec<(String, Arc<dyn Tool>)> =
        vec![("builtin".to_owned(), inner.clone() as Arc<dyn Tool>)];
    let out = capped(tools, &caps(&[("cat", 50)]));
    let wrapped = &out[0].1;
    assert_eq!(wrapped.definition(), inner.definition());
    assert_eq!(wrapped.guidelines(), Some("g".to_owned()));
    let arguments: Map<String, Value> = Map::new();
    assert_eq!(
        wrapped.effects(&arguments).unwrap(),
        inner.effects(&arguments).unwrap()
    );
    let cancel = fakes::CancelToken::new();
    let emit = fakes::Recorder::default();
    assert_eq!(
        wrapped.run(&arguments, &cancel, &emit),
        inner.run(&arguments, &cancel, &emit)
    );
}

/// An asker that names the call `a_9` and is never asked.
struct Named;

impl Ask for Named {
    fn action(&self) -> contract::ActionId {
        contract::ActionId("a_9".into())
    }

    fn answerable(&self) -> bool {
        true
    }

    fn ask(&self, _asking: Asking) -> Answered {
        Answered::NoAnswer
    }
}

#[test]
fn a_capped_tool_runs_the_wrapped_tool_with_the_same_asker() {
    let inner = Arc::new(Fixed::named("cat"));
    let tools = vec![("builtin".to_owned(), inner as Arc<dyn Tool>)];
    let out = capped(tools, &caps(&[("cat", 50)]));
    let ran = out[0].1.run_asking(
        &Map::new(),
        &fakes::CancelToken::new(),
        &fakes::Recorder::default(),
        &Named,
    );
    assert_eq!(ran.details, Some(json!("a_9")));
}

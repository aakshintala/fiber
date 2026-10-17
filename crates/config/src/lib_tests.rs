//! Borrowing layer views and the `get` fast path (`super::view`,
//! `super::Config::get_segments`): what the clone count is made of, and
//! that the fast path reads what `merged` reads.

#![allow(clippy::unwrap_used, reason = "a failure is the test's")]

use std::borrow::Cow;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::{Config, ProjectKey, Source, Sources, full_name, keys, path, view};

#[test]
fn view_borrows_without_a_model() {
    let layer = json!({"model": "a/b", "models": {"a/b": {"model": "c/d"}}});
    assert!(matches!(view(&layer, None), Cow::Borrowed(_)));
}

#[test]
fn view_borrows_a_layer_with_no_entry_for_the_model() {
    let layer = json!({"model": "a/b", "models": {"x/y": {"model": "c/d"}}});
    let seen = view(&layer, Some("a/b"));
    assert!(matches!(seen, Cow::Borrowed(_)));
    assert_eq!(seen["model"], json!("a/b"));
}

#[test]
fn view_owns_a_layer_with_an_entry_for_the_model() {
    let layer = json!({"model": "a/b", "models": {"a/b": {"model": "c/d"}}});
    let seen = view(&layer, Some("a/b"));
    assert!(matches!(seen, Cow::Owned(_)));
    assert_eq!(seen["model"], json!("c/d"));
}

/// The layers lowest first.
type Layers = Vec<(Source, Value)>;

/// A `Config` loaded over empty directories, holding `layers`, past the
/// file checks, so the table reaches shapes no file may hold (a scalar
/// over an object, a scalar per-model entry).
fn config_with(layers: Layers) -> Config {
    let root = fakes::TempDir::new("fiber-get-differential");
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = Config::load(Sources {
        home,
        workspace,
        project: ProjectKey::new("-test").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    config.layers = layers;
    config
}

fn global(layer: Value) -> (Source, Value) {
    (Source::Global(PathBuf::from("/home/config.json")), layer)
}

fn project(layer: Value) -> (Source, Value) {
    (
        Source::Project(PathBuf::from("/home/projects/k/config.json")),
        layer,
    )
}

fn repository(layer: Value) -> (Source, Value) {
    (
        Source::Repository(PathBuf::from("/ws/.fiber/config.json")),
        layer,
    )
}

fn default() -> (Source, Value) {
    (Source::Default, keys::defaults())
}

/// The `merged`-route answer for the table: the merged value with the
/// highest layer holding the key, or the schema default.
fn reference(config: &Config, key: &str, model: Option<&str>) -> Option<(Value, Source)> {
    let segments = path::parse(key).unwrap();
    let mut aliased = segments.clone();
    if let [area, name, ..] = aliased.as_mut_slice()
        && area == "extensions"
    {
        *name = full_name(name);
    }
    let from = config
        .layers
        .iter()
        .rev()
        .find(|(_, layer)| path::get(&view(layer, model), &aliased).is_some());
    match from {
        Some((source, _)) => {
            let value = path::get(&config.merged(model), &aliased)?.clone();
            Some((value, source.clone()))
        }
        None => {
            let default = serde_json::from_str(keys::leaf(&aliased)?.default?).ok()?;
            Some((default, Source::Default))
        }
    }
}

#[test]
fn get_equals_the_merged_value_for_every_key_shape() {
    // Each row: the layers lowest first, the key, and the model. The
    // union, entry-replace, hiding and per-model rows each kill the
    // fast-path check they name: dropping it reads the holder where the
    // merge reads something else.
    let cases: Vec<(Layers, &str, Option<&str>)> = vec![
        // A scalar in three layers: the top wins.
        (
            vec![
                default(),
                global(json!({"handoff": {"tokens": 1}})),
                project(json!({"handoff": {"tokens": 2}})),
            ],
            "handoff.tokens",
            None,
        ),
        // A scalar only in a middle layer.
        (
            vec![default(), global(json!({"handoff": {"tokens": 1}}))],
            "handoff.tokens",
            None,
        ),
        // An object in two layers: the keys merge, naming the highest.
        (
            vec![
                default(),
                global(json!({"handoff": {"tokens": 1}})),
                project(json!({"handoff": {"nudge": false}})),
            ],
            "handoff",
            None,
        ),
        // A union key in three layers: every layer's names, naming the
        // highest setting it. Kills dropping the union check.
        (
            vec![
                default(),
                repository(json!({"skills": {"disabled": ["a"]}})),
                global(json!({"skills": {"disabled": ["b"]}})),
                project(json!({"skills": {"disabled": ["c"]}})),
            ],
            "skills.disabled",
            None,
        ),
        // An entry-replaced key in two layers, read at a leaf only
        // the lower layer's entry holds: the upper entry replaces
        // whole, so it is gone. Kills dropping the
        // entry-replace check.
        (
            vec![
                default(),
                global(
                    json!({"providers": {"acme": {"credentials": {"work": {"command": ["w"], "extra": 1}}}}}),
                ),
                project(
                    json!({"providers": {"acme": {"credentials": {"work": {"command": ["h"]}}}}}),
                ),
            ],
            "providers.acme.credentials.work.extra",
            None,
        ),
        // A scalar under a prefix a higher layer set to a scalar:
        // hidden. Kills dropping the hiding stop.
        (
            vec![
                default(),
                global(json!({"extensions": {"x": {"enabled": false}}})),
                project(json!({"extensions": 5})),
            ],
            "extensions.x.enabled",
            None,
        ),
        // An explicit null in the top layer is returned.
        (
            vec![default(), global(json!({"handoff": {"tokens": null}}))],
            "handoff.tokens",
            None,
        ),
        // A missing key with a default names the defaults.
        (vec![default(), global(json!({}))], "handoff.enabled", None),
        // A missing key without one is unset.
        (vec![default(), global(json!({}))], "reviewer.model", None),
        // A per-model entry in one layer only. Kills skipping the model
        // view.
        (
            vec![
                default(),
                global(json!({"model": "a/b", "models": {"a/b": {"model": "c/d"}}})),
            ],
            "model",
            Some("a/b"),
        ),
        (
            vec![
                default(),
                global(json!({"model": "a/b", "models": {"a/b": {"model": "c/d"}}})),
            ],
            "model",
            None,
        ),
        // An empty object in the top layer over an object below.
        (
            vec![
                default(),
                global(json!({"handoff": {"tokens": 1}})),
                project(json!({"handoff": {}})),
            ],
            "handoff",
            None,
        ),
        // An object in three layers, each naming its own leaves: the
        // leaves merge, naming the highest. Kills returning the highest
        // holder whole instead of merging the subtree.
        (
            vec![
                default(),
                repository(json!({"handoff": {"tokens": 1}})),
                global(json!({"handoff": {"nudge": false}})),
                project(json!({"handoff": {"tokens": 2}})),
            ],
            "handoff",
            None,
        ),
        // An entry-replaced row under an object key: each entry goes
        // whole, while sibling entries survive. Kills merging the
        // subtree key by key past the entry row.
        (
            vec![
                default(),
                global(
                    json!({"providers": {"acme": {"credentials": {"work": {"command": ["w"], "extra": 1}, "home": {"command": ["hh"]}}}}}),
                ),
                project(
                    json!({"providers": {"acme": {"credentials": {"work": {"command": ["h"]}}}}}),
                ),
            ],
            "providers",
            None,
        ),
        // A proper prefix of a union row: the union fill re-creates the
        // object, never the scalar.
        (
            vec![default(), global(json!({"skills": 5}))],
            "skills",
            None,
        ),
        // A scalar over the key with no lower holder: the default.
        (
            vec![default(), global(json!({"extensions": 5}))],
            "extensions.x.enabled",
            None,
        ),
        // A scalar per-model entry wipes the layer's view.
        (
            vec![default(), global(json!({"models": {"a": 5}}))],
            "model",
            Some("a"),
        ),
        // A scalar per-model entry in the top layer over an object
        // below: the entry wipes the merge, so the key is gone. Kills
        // reading past a scalar view.
        (
            vec![
                default(),
                repository(json!({"k": {"a": 1}})),
                project(json!({"k": {"a": 2}, "models": {"m": 5}})),
            ],
            "k.a",
            Some("m"),
        ),
    ];
    for (layers, key, model) in &cases {
        let config = config_with(layers.clone());
        assert_eq!(
            config.get(key, *model),
            reference(&config, key, *model),
            "key `{key}` model `{model:?}`"
        );
    }
}

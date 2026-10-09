//! `docs/extensions.md`, "A fresh install": `extension_missing` names the
//! first-party extension that serves the provider, or gives a generic hint
//! when no first-party package serves it.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test helpers; a failure is the test's")]

use std::path::Path;

use extensions::Error;

fn declared_providers() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers");
    let mut pairs = Vec::new();
    let mut packages: Vec<_> = std::fs::read_dir(&root)
        .expect("providers directory reads")
        .map(|entry| entry.expect("package entry reads").path())
        .collect();
    packages.sort();
    for package_dir in packages {
        if !package_dir.is_dir() {
            continue;
        }
        let package = package_dir
            .file_name()
            .expect("package dir has a name")
            .to_string_lossy()
            .into_owned();
        let providers_dir = package_dir.join("providers");
        let entries = match std::fs::read_dir(&providers_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("{}: {error}", providers_dir.display()),
        };
        let mut files: Vec<_> = entries
            .map(|entry| entry.expect("provider entry reads").path())
            .collect();
        files.sort();
        for file in files {
            if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let stem = file
                .file_stem()
                .expect("json file has a stem")
                .to_string_lossy()
                .into_owned();
            let text = std::fs::read_to_string(&file).expect("provider file reads");
            let data: serde_json::Value =
                serde_json::from_str(&text).expect("provider file parses");
            let name = data
                .get("name")
                .and_then(|name| name.as_str())
                .expect("provider file names itself");
            assert_eq!(name, stem, "{}", file.display());
            pairs.push((name.to_owned(), package.clone()));
        }
    }
    pairs
}

#[test]
fn every_declared_provider_names_its_package() {
    let pairs = declared_providers();
    assert!(
        pairs.contains(&("opencode-go".to_owned(), "opencode".to_owned())),
        "{pairs:?}"
    );
    assert!(
        pairs.contains(&("opencode-zen".to_owned(), "opencode".to_owned())),
        "{pairs:?}"
    );
    for (name, package) in &pairs {
        assert_eq!(
            Error::ProviderMissing {
                provider: name.clone(),
            }
            .to_string(),
            format!(
                "The provider `{name}` is not installed. Run `fiber extension install {package}`."
            )
        );
        assert_eq!(
            config::full_name(package),
            format!("github.com/aakshintala/fiber/providers/{package}")
        );
    }
}

#[test]
fn a_provider_no_first_party_package_serves_gets_the_generic_hint() {
    for provider in ["nobody-serves", "opencode"] {
        assert_eq!(
            Error::ProviderMissing {
                provider: provider.to_owned(),
            }
            .to_string(),
            format!(
                "The provider `{provider}` is not installed. Install the extension that provides it with `fiber extension install <name or URL>`."
            )
        );
    }
}

/// The first-party models whose provider hosts a search (`docs/tools.md`,
/// "Hosted by the provider"): every Anthropic model, every muse model on
/// `/v1/responses`, and OpenCode's OpenAI pass-through.
#[test]
fn first_party_hosted_searches_are_the_marked_models() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../providers");
    let mut packages: Vec<_> = std::fs::read_dir(&root)
        .expect("providers directory reads")
        .map(|entry| entry.expect("package entry reads").path())
        .collect();
    packages.sort();
    let mut pairs = Vec::new();
    for package_dir in packages {
        if !package_dir.is_dir() {
            continue;
        }
        let package = package_dir
            .file_name()
            .expect("package dir has a name")
            .to_string_lossy()
            .into_owned();
        for data in config::read_providers(&package_dir).expect("provider data reads") {
            let mut models = data.models.clone();
            let notices = extensions::leave_out_invalid(&data.name, &package, &mut models);
            assert!(notices.is_empty(), "{package}: {notices:?}");
            for model in &models {
                if let Some(kind) = &model.web_search {
                    pairs.push((format!("{}/{}", data.name, model.id), kind.clone()));
                }
                if let Some(include) = model.extra_body.get("include") {
                    assert_eq!(
                        include.as_array().and_then(|items| items.first()),
                        Some(&serde_json::Value::String(
                            "reasoning.encrypted_content".into()
                        )),
                        "{}/{} keeps reasoning replay first",
                        data.name,
                        model.id
                    );
                }
            }
        }
    }
    pairs.sort();
    let mut expected: Vec<(String, String)> = [
        ("anthropic/claude-fable-5", "web_search_20250305"),
        ("anthropic/claude-fable-5-1", "web_search_20250305"),
        ("anthropic/claude-haiku-4-5", "web_search_20250305"),
        ("anthropic/claude-haiku-4-5-20251001", "web_search_20250305"),
        ("anthropic/claude-haiku-5-5", "web_search_20250305"),
        ("anthropic/claude-opus-4-5", "web_search_20250305"),
        ("anthropic/claude-opus-4-5-20251101", "web_search_20250305"),
        ("anthropic/claude-opus-4-6", "web_search_20250305"),
        ("anthropic/claude-opus-4-7", "web_search_20250305"),
        ("anthropic/claude-opus-4-8", "web_search_20250305"),
        ("anthropic/claude-opus-5", "web_search_20250305"),
        ("anthropic/claude-opus-5-5", "web_search_20250305"),
        ("anthropic/claude-sonnet-4-5", "web_search_20250305"),
        (
            "anthropic/claude-sonnet-4-5-20250929",
            "web_search_20250305",
        ),
        ("anthropic/claude-sonnet-4-6", "web_search_20250305"),
        ("anthropic/claude-sonnet-5", "web_search_20250305"),
        ("anthropic/claude-sonnet-5-5", "web_search_20250305"),
        ("muse/muse-spark-1.1", "web_search"),
        ("muse/muse-spark-1.2", "web_search"),
        ("muse/muse-spark-1.2-contributor", "web_search"),
        ("muse/muse-spark-1.3", "web_search"),
        ("muse/muse-spark-1.3-contributor", "web_search"),
        ("opencode-zen/gpt-6.1-sol", "web_search"),
    ]
    .into_iter()
    .map(|(model, kind)| (model.to_owned(), kind.to_owned()))
    .collect();
    expected.sort();
    assert_eq!(pairs, expected);
}

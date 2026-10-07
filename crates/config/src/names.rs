//! Extension names: short names, full names, and the directory an
//! extension's files live under in Fiber home (`docs/extensions.md`,
//! "Names"; `docs/state.md`, "What each part holds").

/// The first-party provider extensions' short names.
pub const SHORT_NAMES: [&str; 11] = [
    "anthropic",
    "openai",
    "gemini",
    "codex",
    "openrouter",
    "opencode",
    "databricks",
    "muse",
    "bedrock",
    "vertex",
    "azure",
];

/// The first-party extensions under `extensions/` with short names.
const EXTENSION_SHORT_NAMES: [&str; 4] = ["claude", "cursor-agent", "hooks", "memory"];

const PROVIDERS: &str = "github.com/aakshintala/fiber/providers/";
const EXTENSIONS: &str = "github.com/aakshintala/fiber/extensions/";

/// What a person typed as a full extension name: a provider short name
/// becomes `github.com/aakshintala/fiber/providers/<short>`, and an
/// extension short name becomes
/// `github.com/aakshintala/fiber/extensions/<short>`.
pub fn full_name(typed: &str) -> String {
    if SHORT_NAMES.contains(&typed) {
        format!("{PROVIDERS}{typed}")
    } else if EXTENSION_SHORT_NAMES.contains(&typed) {
        format!("{EXTENSIONS}{typed}")
    } else {
        typed.to_owned()
    }
}

/// The name a person types: the short name for a first-party provider or
/// a first-party extension, else the name.
///
/// The inverse of [`full_name`]:
/// `github.com/aakshintala/fiber/providers/opencode` shows as `opencode`;
/// `github.com/aakshintala/fiber/extensions/memory` shows as `memory`;
/// any other name shows whole, so a `remove` command naming it works.
pub fn short_name(name: &str) -> &str {
    name.strip_prefix(PROVIDERS)
        .filter(|s| SHORT_NAMES.contains(s))
        .or_else(|| {
            name.strip_prefix(EXTENSIONS)
                .filter(|s| EXTENSION_SHORT_NAMES.contains(s))
        })
        .unwrap_or(name)
}

/// The directory that names an extension in Fiber home: `extensions/<dir>/`,
/// `data/<dir>/`, `config/<dir>.json` and their project and repository
/// counterparts. A first-party extension's short name, else its full name
/// with every `/` made `-`, as for a project key. The two never collide: a
/// slugged git address keeps its host's dot and a slugged path starts with
/// `-`, and a short name has neither.
pub fn dir_name(name: &str) -> String {
    let short = short_name(name);
    if short == name {
        name.replace('/', "-")
    } else {
        short.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{EXTENSION_SHORT_NAMES, SHORT_NAMES, dir_name, full_name, short_name};

    #[test]
    fn every_short_name_is_a_first_party_provider() {
        for short in SHORT_NAMES {
            assert_eq!(
                full_name(short),
                format!("github.com/aakshintala/fiber/providers/{short}")
            );
        }
        assert_eq!(SHORT_NAMES.len(), 11);
    }

    #[test]
    fn every_extension_short_name_is_a_first_party_extension() {
        for short in ["claude", "cursor-agent", "hooks", "memory"] {
            let full = format!("github.com/aakshintala/fiber/extensions/{short}");
            assert_eq!(full_name(short), full);
            assert_eq!(short_name(&full), short);
        }
    }

    #[test]
    fn a_full_name_is_left_alone() {
        assert_eq!(full_name("github.com/acme/x"), "github.com/acme/x");
        assert_eq!(full_name("other"), "other");
    }

    #[test]
    fn a_first_party_full_name_shows_short_and_any_other_name_shows_whole() {
        assert_eq!(
            short_name("github.com/aakshintala/fiber/providers/opencode"),
            "opencode"
        );
        assert_eq!(short_name("github.com/acme/lint"), "github.com/acme/lint");
        assert_eq!(
            short_name("github.com/aakshintala/fiber/providers/notashort"),
            "github.com/aakshintala/fiber/providers/notashort"
        );
        assert_eq!(short_name("acme"), "acme");
        assert_eq!(
            short_name("github.com/aakshintala/fiber/extensions/other"),
            "github.com/aakshintala/fiber/extensions/other"
        );
        assert_eq!(
            short_name("github.com/aakshintala/fiber/providers/memory"),
            "github.com/aakshintala/fiber/providers/memory"
        );
        assert_eq!(
            short_name("github.com/aakshintala/fiber/extensions/muse"),
            "github.com/aakshintala/fiber/extensions/muse"
        );
    }

    #[test]
    fn a_first_party_extension_is_named_by_its_short_name_and_any_other_by_its_slug() {
        assert_eq!(
            dir_name("github.com/aakshintala/fiber/extensions/memory"),
            "memory"
        );
        assert_eq!(
            dir_name("github.com/aakshintala/fiber/providers/opencode"),
            "opencode"
        );
        assert_eq!(dir_name("github.com/acme/lint"), "github.com-acme-lint");
        assert_eq!(
            dir_name("github.com/aakshintala/fiber/extensions/muse"),
            "github.com-aakshintala-fiber-extensions-muse"
        );
        assert_eq!(dir_name("/tmp/x"), "-tmp-x");
        assert_eq!(dir_name("memory"), "memory");
    }

    /// The package directories under one top-level directory of Fiber's
    /// own repository.
    fn packages(top: &str) -> Vec<String> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(top);
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn every_first_party_package_has_a_short_name() {
        let providers = packages("providers");
        let extensions = packages("extensions");
        assert!(!providers.is_empty() && !extensions.is_empty());
        for name in providers {
            assert!(
                SHORT_NAMES.contains(&name.as_str()),
                "providers/{name} has no short name in SHORT_NAMES"
            );
        }
        for name in extensions {
            assert!(
                EXTENSION_SHORT_NAMES.contains(&name.as_str()),
                "extensions/{name} has no short name in EXTENSION_SHORT_NAMES"
            );
        }
    }
}

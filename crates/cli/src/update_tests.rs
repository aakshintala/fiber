//! `resolve_channel` precedence (`docs/releasing.md`, "Updating"): the
//! flag beats `FIBER_CHANNEL`, and an empty or unset variable means
//! stable. An invalid non-empty value names itself.

#![allow(clippy::unwrap_used, reason = "a failing resolve is the test's")]

use super::*;

#[test]
fn resolve_channel_prefers_the_flag_over_the_environment() {
    assert_eq!(
        resolve_channel(Some(Channel::Stable), Some("nightly")),
        Ok(Channel::Stable)
    );
    assert_eq!(
        resolve_channel(Some(Channel::Nightly), Some("stable")),
        Ok(Channel::Nightly)
    );
    assert_eq!(
        resolve_channel(Some(Channel::Nightly), None),
        Ok(Channel::Nightly)
    );
}

#[test]
fn resolve_channel_reads_fiber_channel_without_a_flag() {
    assert_eq!(resolve_channel(None, None), Ok(Channel::Stable));
    assert_eq!(resolve_channel(None, Some("")), Ok(Channel::Stable));
    assert_eq!(resolve_channel(None, Some("stable")), Ok(Channel::Stable));
    assert_eq!(resolve_channel(None, Some("nightly")), Ok(Channel::Nightly));
}

#[test]
fn resolve_channel_rejects_any_other_value_naming_it() {
    let error = resolve_channel(None, Some("beta")).unwrap_err();
    assert!(error.to_string().contains("`beta`"), "{error}");
}

#[test]
fn production_bases_point_at_the_release_roots() {
    let bases = ReleaseBases::production();
    assert_eq!(bases.web, "https://github.com/aakshintala/fiber");
    assert_eq!(bases.api, "https://api.github.com/repos/aakshintala/fiber");
}

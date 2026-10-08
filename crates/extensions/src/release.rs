//! Putting a release's docs and first-party extensions in place in Fiber
//! home (`docs/releasing.md`, "Installing").

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only its tests call the unpacker until install_release does"
    )
)]
mod unpack;

/// The release's docs archive.
#[expect(dead_code, reason = "install_release names it")]
pub(crate) const DOCS: &str = "fiber-docs.tar.gz";
/// The release's first-party extensions archive.
#[expect(dead_code, reason = "install_release names it")]
pub(crate) const EXTENSIONS: &str = "fiber-extensions.tar.gz";

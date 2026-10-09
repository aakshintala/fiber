//! Which first-party package serves each provider (`docs/extensions.md`,
//! "A fresh install"): the build script's table of the provider data under
//! `providers/`, and the hint `extension_missing` gives for a provider.

/// Each provider a first-party package declares, with that package's short
/// name: `(provider name, package short name)`, sorted by provider.
const PROVIDER_PACKAGES: &[(&str, &str)] =
    include!(concat!(env!("OUT_DIR"), "/provider_packages.rs"));

/// The short name of the first-party package whose data declares `provider`.
fn serving_package(provider: &str) -> Option<&'static str> {
    PROVIDER_PACKAGES
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, package)| *package)
}

/// What to do about a provider that is not installed: name the first-party
/// extension that serves it, or, when no first-party package serves it, say
/// to install the extension that provides it, by name or URL.
pub(crate) fn install_hint(provider: &str) -> String {
    match serving_package(provider) {
        Some(package) => format!("Run `fiber extension install {package}`."),
        None => {
            "Install the extension that provides it with `fiber extension install <name or URL>`."
                .to_owned()
        }
    }
}

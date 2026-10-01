//! Installs extensions into Fiber home and registers what they provide
//! (`docs/extensions.md`). It holds only what a data-only provider needs:
//! installing from a local path, reading each installed provider's data
//! through `config`, and choosing the session's model from them
//! (`docs/model-routing.md`, "Naming a model" and "Choosing the model").

mod install;
mod providers;

use std::io;
use std::path::PathBuf;

use config::ConfigError;
use contract::ErrorCode;

pub use install::install;
pub use providers::{Model, Providers};

/// The extension API's major version this Fiber speaks
/// (`docs/extensions.md`, "The extension API version").
pub const API: u64 = 1;

/// A failure of the `extensions` crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Reading a manifest or a provider's data failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Copying or moving an extension's files failed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The path.
        path: PathBuf,
        /// Why.
        source: io::Error,
    },
    /// A source that holds Fiber home's `extensions/`, or lies inside it.
    #[error("{} holds Fiber home's extensions or lies inside them; install from a directory outside Fiber home.", path.display())]
    Overlaps {
        /// The source, resolved.
        path: PathBuf,
    },
    /// The manifest's `fiber` is newer than the running Fiber.
    #[error(
        "`{name}` needs Fiber {needs} or later, and this is Fiber {running}. Run `fiber upgrade`."
    )]
    NeedsNewerFiber {
        /// The extension.
        name: String,
        /// Its lowest Fiber version.
        needs: String,
        /// The running Fiber's version.
        running: String,
    },
    /// The manifest's `api` is not this Fiber's.
    #[error("`{name}` was written for extension API {api}, and this Fiber speaks API {API}.")]
    ApiVersion {
        /// The extension.
        name: String,
        /// Its `api`.
        api: u64,
    },
    /// A version that is not three numbers, such as `0.3.0` or `v1.4.0`.
    #[error("`{text}` is not a version such as `0.3.0`.")]
    BadVersion {
        /// The text.
        text: String,
    },
    /// An extension name that does not slug to one file name.
    #[error("`{name}` is not an extension name such as `github.com/acme/fiber-acme`.")]
    BadName {
        /// The name.
        name: String,
    },
    /// A model reference whose provider is not installed.
    #[error("The provider `{provider}` is not installed. Run `fiber install {provider}`.")]
    ProviderMissing {
        /// The provider.
        provider: String,
    },
    /// A bare model id no installed provider has.
    #[error(
        "No installed provider has the model `{id}`. Install its provider with `fiber install <name>`."
    )]
    ModelMissing {
        /// The id as typed.
        id: String,
    },
    /// An installed provider that does not list the model.
    #[error("The provider `{provider}` has no model `{model}`.")]
    UnknownModel {
        /// The provider.
        provider: String,
        /// The model id.
        model: String,
    },
    /// A bare model id that more than one installed provider has.
    #[error("The model `{id}` is offered by more than one provider: {}. Name one as `provider/model`.", matches.join(", "))]
    Ambiguous {
        /// The id as typed.
        id: String,
        /// Each match, as `provider/model`.
        matches: Vec<String>,
    },
    /// Neither a resumed session nor configuration chose a model.
    #[error("No model was chosen. Pass `--model provider/model`, or set `model` in configuration.")]
    NoModel,
}

impl Error {
    /// The stable code a caller switches on.
    pub fn code(&self) -> ErrorCode {
        match self {
            Self::Config(e) => e.code(),
            Self::Io { .. } => ErrorCode::IoFailed,
            Self::Overlaps { .. } => ErrorCode::Usage,
            // ponytail: docs/errors.md has no code for an extension this
            // Fiber cannot run; `usage` stands in until the owner names one.
            Self::NeedsNewerFiber { .. } | Self::ApiVersion { .. } => ErrorCode::Usage,
            Self::BadVersion { .. } | Self::BadName { .. } => ErrorCode::ConfigInvalid,
            Self::ProviderMissing { .. } | Self::ModelMissing { .. } => ErrorCode::ExtensionMissing,
            // ponytail: docs/errors.md has no code for a model reference that
            // names no model or several; `no_model` stands in, its message
            // listing the matches, until the owner names one.
            Self::UnknownModel { .. } | Self::Ambiguous { .. } | Self::NoModel => {
                ErrorCode::NoModel
            }
        }
    }
}

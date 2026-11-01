//! First-party provider installs through the terminal and hub configuration seam.

use std::path::Path;
use std::sync::Arc;

use contract::clock::Clock;
use tui::{ConfigureError, ProviderInstall};

pub(super) fn provider(
    seam: &super::Seam,
    workspace: &Path,
    provider: &str,
    clock: Arc<dyn Clock>,
) -> Result<ProviderInstall, ConfigureError> {
    let config = seam.load(workspace)?;
    let installed = cli::install_first_party(
        &seam.home,
        &config,
        provider,
        env!("CARGO_PKG_VERSION"),
        &seam.origin,
        clock.as_ref(),
        // First-party binaries record on the hub writer at debug; at
        // info no writer passes and the downloads stay silent.
        seam.hub_writer(&config, &clock),
    )
    .map_err(|error| ConfigureError {
        code: error.code(),
        message: error.to_string(),
    })?;
    Ok(ProviderInstall {
        installed: installed.installed,
        summary: installed.summary,
        login: installed
            .login
            .into_iter()
            .map(|need| tui::LoginNeed {
                provider: need.provider,
                error: need.error,
            })
            .collect(),
    })
}

#[cfg(test)]
#[path = "install_tests.rs"]
mod tests;

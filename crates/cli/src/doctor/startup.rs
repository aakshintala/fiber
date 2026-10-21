//! Data-only startup: the ordered blocker checks `fiber doctor` runs
//! without loading Lua, connecting, or running a credential command.
//! The order is the session's (`parts_in`): configuration, extensions,
//! the model choice and its checks, the selected key, then the required
//! `mcp.servers` entries. Only the first blocker is reported.

use std::path::Path;

use config::{
    Config, ConfigError, CredentialFrom, CredentialLabelSources, Origin, Presence, ProviderData,
    ScriptRead, Sources, mcp_servers, read_script, read_secret,
};
use contract::ErrorCode;
use extensions::{Model, Providers, is_enabled};

use super::files::clean;
use crate::project_of;

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;

/// Why the report's exit is 1: the session's code for the same cause, its
/// line, and whether it annotates a fresh session's matching failure.
pub(crate) struct Blocker {
    /// The session's code for the same cause.
    pub code: ErrorCode,
    /// The report line, cleaned.
    pub line: String,
    /// A key error (Ruling 4) or the selected label's error (Ruling 5):
    /// the only blockers `with_fix` annotates with.
    pub key: bool,
}

/// The report's middle: the model, providers, permissions and MCP lines,
/// the first blocker, and the loaded configuration and providers.
pub(crate) struct Startup {
    /// The model, providers, permissions and MCP lines, in order.
    pub lines: Vec<String>,
    /// The first blocker, if any.
    pub blocker: Option<Blocker>,
    /// The secrets lines, printed after the hub lines.
    pub secrets: Vec<String>,
    /// The configuration and extension notices for stderr.
    pub notices: Vec<contract::events::Notice>,
}

/// Loads the configuration, extensions and model choice and checks each
/// blocker in the session's order, reading no key but the selected label's
/// (only to test whether it is blank), running no command and starting
/// nothing.
pub(crate) fn startup(
    home: &Path,
    workspace: &Path,
    overrides: &[String],
    env: &dyn Fn(&str) -> Option<String>,
    has_userinfo: &dyn Fn(&str) -> bool,
) -> Startup {
    let (_, project) = match project_of(home, workspace) {
        Ok(found) => found,
        Err(failure) => {
            let line = configuration_line(&failure.message, failure.code.clone());
            return Startup {
                lines: vec![line.clone()],
                blocker: Some(Blocker {
                    code: failure.code,
                    line,
                    key: false,
                }),
                secrets: Vec::new(),
                notices: Vec::new(),
            };
        }
    };
    let config = match Config::load(Sources {
        home: home.to_owned(),
        workspace: workspace.to_owned(),
        project,
        overrides: overrides.to_vec(),
    }) {
        Ok(config) => config,
        Err(error) => {
            let line = configuration_line(&error.to_string(), error.code());
            return Startup {
                lines: vec![line.clone()],
                blocker: Some(Blocker {
                    code: error.code(),
                    line,
                    key: false,
                }),
                secrets: Vec::new(),
                notices: Vec::new(),
            };
        }
    };
    let mut notices: Vec<contract::events::Notice> = config.notices().to_vec();
    // The unfiltered inventory is display only; the enabled path below
    // decides the blockers, as the session starts from it. A disabled
    // package's malformed provider JSON loads here and not there, so
    // its failure is an informational line, never the exit.
    let inventory = Providers::load(home);
    let mut enabled = match Providers::load_enabled(home, &config) {
        Ok((providers, loaded)) => {
            notices.extend(loaded);
            providers
        }
        Err(error) => {
            let blocker = extension_blocker(&error);
            return Startup {
                lines: vec![blocker.line.clone()],
                blocker: Some(blocker),
                secrets: Vec::new(),
                notices,
            };
        }
    };
    match enabled.fill_placeholders(&config, env) {
        Ok(filled) => notices.extend(filled),
        Err(error) => {
            let blocker = extension_blocker(&error);
            return Startup {
                lines: vec![blocker.line.clone()],
                blocker: Some(blocker),
                secrets: Vec::new(),
                notices,
            };
        }
    }
    if let Some((serde_json::Value::String(typed), _)) = config.get("model", None) {
        enabled.add_scripted(&typed);
    }
    let display: &Providers = match &inventory {
        Ok((providers, _)) => providers,
        Err(_) => &enabled,
    };
    let mut checked = Checked {
        lines: Vec::new(),
        blocker: None,
        chosen: None,
        workspace,
        config: &config,
        inventory: display,
        enabled: &enabled,
        env,
        has_userinfo,
    };
    if let Err(error) = &inventory {
        checked.lines.push(extension_blocker(error).line);
    }
    checked.model();
    checked.providers();
    checked.permissions();
    checked.mcp();
    let secrets = secrets(home, &config, display);
    Startup {
        lines: checked.lines,
        blocker: checked.blocker,
        secrets,
        notices,
    }
}

/// The configuration failure's line: the load error, plus the one fix
/// when the error names none. An unreadable file's error says only
/// that it cannot be read; an invalid file's error already says to fix
/// it and try again.
fn configuration_line(message: &str, code: ErrorCode) -> String {
    let message = clean(message);
    if code == ErrorCode::IoFailed {
        format!("configuration does not load: {message} Check that you can read that file.")
    } else {
        format!("configuration does not load: {message}")
    }
}

/// The extension load failure's line: the load error plus the one fix.
fn extension_blocker(error: &extensions::Error) -> Blocker {
    Blocker {
        code: error.code(),
        line: format!(
            "extensions do not load: {} Remove the extension it names with \
             `fiber extension remove <name>`, then install it again.",
            clean(&error.to_string())
        ),
        key: false,
    }
}

/// The model, providers, permissions and MCP lines with the first blocker.
struct Checked<'a> {
    lines: Vec<String>,
    blocker: Option<Blocker>,
    /// The chosen model's provider, when the model resolved to one in data.
    chosen: Option<String>,
    workspace: &'a Path,
    config: &'a Config,
    inventory: &'a Providers,
    enabled: &'a Providers,
    env: &'a dyn Fn(&str) -> Option<String>,
    has_userinfo: &'a dyn Fn(&str) -> bool,
}

/// One label's row: its listed origin, or the selected label's missing
/// source, which is not an error.
enum Row {
    /// The origin `credential_origins` listed, possibly an error.
    Listed(Result<Origin, ConfigError>),
    /// The selected label has nothing stored or configured under it.
    Missing,
}

impl Checked<'_> {
    /// The configured model, or the `NoModel` line when none is chosen.
    fn typed(&self) -> Option<String> {
        self.config
            .get("model", None)
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
    }

    /// The model line and its checks: the window, the `:level` suffix and
    /// the scripted script.
    fn model(&mut self) {
        let Some(typed) = self.typed() else {
            let line = clean(&extensions::Error::NoModel.to_string());
            self.block(line.clone(), ErrorCode::NoModel, false);
            self.lines.push(line);
            return;
        };
        let shown = clean(&typed);
        match self.enabled.resolve(&typed) {
            Err(extensions::Error::ProviderMissing { provider })
                if self.enabled.could_register(&provider) =>
            {
                // An enabled extension could register the provider once a
                // session runs its code: not checked, not a blocker. The
                // line prints here: the provider is absent from data, so
                // the label blocks below never list it.
                self.lines.push(format!("model: {shown}"));
                self.lines.push(format!(
                    "{}: registered by extension code, not checked",
                    clean(&provider)
                ));
            }
            Err(error) => {
                self.lines.push(format!("model: {shown}"));
                let line = clean(&error.to_string());
                self.block(line.clone(), error.code(), false);
                self.lines.push(line);
            }
            Ok(model) => {
                let provider = clean(&model.provider.name);
                self.chosen = Some(model.provider.name.clone());
                self.lines
                    .push(format!("model: {shown} (provider {provider})"));
                self.window(&model);
                self.thinking(&model);
                self.script(&model);
            }
        }
    }

    /// The first blocker wins: each later check needs the earlier one.
    fn block(&mut self, line: String, code: ErrorCode, key: bool) {
        if self.blocker.is_none() {
            self.blocker = Some(Blocker { code, line, key });
        }
    }

    /// The model must declare a positive context window. A model without
    /// one never reaches here: the load leaves it out, so resolving it is
    /// `UnknownModel`; this refusal is pinned through the shared function.
    fn window(&mut self, model: &Model<'_>) {
        if let Err(error) = extensions::context_window(model.model, &model.reference()) {
            let line = format!(
                "{} Run `fiber models` and set another with \
                 `fiber config set model <provider/model>`.",
                clean(&error.to_string())
            );
            self.block(line.clone(), ErrorCode::NoModel, false);
            self.lines.push(line);
        }
    }

    /// The `:level` suffix must name a level the model takes.
    fn thinking(&mut self, model: &Model<'_>) {
        let Some(level) = model.thinking else {
            return;
        };
        if let Some(message) = extensions::thinking_refusal(level, model.model, &model.reference())
        {
            let line = format!(
                "{} Drop the `:<level>` suffix from `model`, or use a level it lists.",
                clean(&message)
            );
            self.block(line.clone(), ErrorCode::InvalidArguments, false);
            self.lines.push(line);
        }
    }

    /// A scripted model's file must read. It is never parsed or run, and
    /// a file that is not regular is never waited on.
    fn script(&mut self, model: &Model<'_>) {
        if model.provider.name != "scripted" {
            return;
        }
        let path = self.workspace.join(&model.model.id);
        match read_script(&path) {
            Ok(ScriptRead::Readable | ScriptRead::NotChecked) => {}
            Err(error) => {
                // The `Io` case names the operating system's sentence, as the
                // session's script read does, so the report's first sentence
                // matches the session's failure line byte for byte.
                let mut why = error.to_string();
                if let ConfigError::Io { source, .. } = &error {
                    why = source.to_string();
                }
                let line = format!(
                    "The script `{}` could not be read: {why}. Check that path, \
                     resolved against the workspace, and that you can read the file.",
                    clean(&path.display().to_string())
                );
                self.block(line.clone(), ErrorCode::IoFailed, false);
                self.lines.push(line);
            }
        }
    }

    /// Every installed provider's label block, with the selected key's
    /// check on the chosen model's provider alone.
    fn providers(&mut self) {
        for name in self.inventory.names() {
            let Some(data) = self.inventory.get(name) else {
                continue;
            };
            self.lines.push(format!("{}:", clean(name)));
            let selected = self.config.credentials().credential_label(data);
            if let CredentialFrom::Function(reads) = &data.credential_from {
                // Its `credential()` reads nothing stored, so no stored file
                // is its credential, and only that code could check the
                // label: doctor runs none (`docs/invocation.md`, "Commands
                // and flags").
                self.lines.push(format!(
                    "  * {}  {}, read by credential() (not checked)",
                    clean(&selected),
                    clean(&label_source(reads, &selected)),
                ));
                continue;
            }
            let mut rows: Vec<(String, Row)> = self
                .config
                .credentials()
                .credential_origins(data)
                .into_iter()
                .map(|(label, origin)| (label, Row::Listed(origin)))
                .collect();
            if rows.iter().all(|(label, _)| label != &selected) {
                // The selected label has nothing under it: the mark still
                // shows, with no source.
                rows.push((selected.clone(), Row::Missing));
            }
            rows.sort_by(|a, b| a.0.cmp(&b.0));
            let width = rows.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
            let chosen = self.chosen.as_deref() == Some(name);
            for (label, row) in &rows {
                let mark = if label == &selected { "*" } else { " " };
                let (origin, note) = self.label_row(data, &selected, label, row, chosen);
                self.lines.push(format!(
                    "  {mark} {label:<width$}  {origin}{note}",
                    label = clean(label),
                    origin = clean(&origin),
                ));
            }
            if chosen
                && let Some(blocker) = self.blocker.as_ref()
                && blocker.key
            {
                self.lines.push(blocker.line.clone());
            }
        }
    }

    /// One label's origin text and, for the chosen model's selected label
    /// alone, its presence note. No other key is read.
    fn label_row(
        &mut self,
        data: &ProviderData,
        selected: &str,
        label: &str,
        row: &Row,
        chosen: bool,
    ) -> (String, &'static str) {
        let origin = match row {
            Row::Listed(Ok(Origin::Stored)) => {
                let stored = data.credential_name.as_deref().unwrap_or(&data.name);
                format!("stored in credentials/{stored}/{label}")
            }
            Row::Listed(Ok(Origin::Env(variable))) => {
                format!("environment variable {variable}")
            }
            Row::Listed(Ok(Origin::File(file))) => format!("file {}", file.display()),
            Row::Listed(Ok(Origin::Command(program))) => {
                format!("command {program} (not run)")
            }
            Row::Listed(Err(error)) => format!("cannot be read: {error}"),
            Row::Missing => "nothing stored or configured".to_owned(),
        };
        if !chosen || label != selected {
            return (origin, "");
        }
        match self
            .config
            .credentials()
            .key_presence(data, label, self.env)
        {
            Ok(Presence::Present) => (origin, ""),
            Ok(Presence::Absent) => {
                self.block(
                    missing_line(&data.name, label),
                    ErrorCode::CredentialMissing,
                    true,
                );
                (origin, " (no key)")
            }
            Ok(Presence::Empty) => {
                // Labels permit newlines and terminal escapes; every
                // interpolation is cleaned, so one line stays one line.
                let shown = clean(label);
                self.block(
                    format!(
                        "the stored key for {} under {shown} is empty: run `fiber logout \
                         {0} --as {shown}`, then `fiber login {0} --as {shown}`.",
                        clean(&data.name)
                    ),
                    ErrorCode::CredentialFailed,
                    true,
                );
                (origin, " (empty)")
            }
            Ok(Presence::Unchecked) => (origin, " (not run)"),
            Err(error) => {
                let code = error.code();
                let line = key_error_line(&data.name, label, &error);
                self.block(line, code, true);
                (origin, "")
            }
        }
    }

    /// The constant permission line.
    fn permissions(&mut self) {
        self.lines.push("permissions: auto".to_owned());
    }

    /// The MCP count and the first required refusal, if any.
    fn mcp(&mut self) {
        let count = match self.config.get("mcp.servers", None) {
            Some((serde_json::Value::Object(servers), _)) => servers.len(),
            _ => 0,
        };
        self.lines.push(format!("MCP servers: {count}"));
        if let Err(refusal) = mcp_servers(self.config, self.has_userinfo) {
            let line = clean(&refusal.message());
            self.block(line.clone(), ErrorCode::ConfigInvalid, false);
            self.lines.push(line);
        }
    }
}

/// Each secret an enabled extension declares that is not stored, with
/// the login that stores it. A missing secret is information, never a
/// blocker, and neither line changes the exit code. A name that is also
/// an installed provider's is skipped, as `fiber login` skips it.
fn secrets(home: &Path, config: &Config, inventory: &Providers) -> Vec<String> {
    let mut lines = Vec::new();
    for (extension, name) in inventory.declared_secrets() {
        if !is_enabled(config, extension) {
            continue;
        }
        if inventory.get(name).is_some() {
            continue;
        }
        let shown = (clean(extension), clean(name));
        match read_secret(home, name) {
            Ok(Some(_)) => {}
            Ok(None) => lines.push(format!(
                "secret {} (from {}) is not stored: run `fiber login {}`",
                shown.1, shown.0, shown.1
            )),
            Err(error) => lines.push(format!(
                "secret {} (from {}) cannot be read: {}",
                shown.1,
                shown.0,
                clean(&error.to_string())
            )),
        }
    }
    lines
}

/// What `label` reads for a provider whose `credential()` reads nothing
/// stored: its `default` source, or the thing a label names, such as
/// `AWS profile work`.
fn label_source(reads: &CredentialLabelSources, label: &str) -> String {
    if label == "default" {
        reads.default.clone()
    } else {
        format!("{} {label}", reads.names)
    }
}

/// The missing-key line: the plain default wording, or the `--as` one.
fn missing_line(provider: &str, label: &str) -> String {
    let provider = clean(provider);
    if label == "default" {
        format!("no key for {provider}: run `fiber login {provider}`")
    } else {
        format!(
            "no key for {provider} under {}: run `fiber login {provider} --as {}`",
            clean(label),
            clean(label)
        )
    }
}

/// The selected label's read error: the invalid label, the unusable
/// stored path, or the unreadable key, each with its fix.
fn key_error_line(provider: &str, label: &str, error: &ConfigError) -> String {
    let provider = clean(provider);
    let label = clean(label);
    let login = if label == "default" {
        format!("`fiber login {provider}`")
    } else {
        format!("`fiber login {provider} --as {label}`")
    };
    match error {
        ConfigError::SecretName { .. } => format!(
            "the credential label {label} for {provider} is not a valid label: set another \
             with `fiber config set providers.\"{provider}\".credential {label}`"
        ),
        ConfigError::NotPlain { .. } => format!(
            "the stored key for {provider} under {label} cannot be used: {} Remove that \
             path, then run {login}.",
            clean(&error.to_string())
        ),
        ConfigError::Json { .. }
        | ConfigError::WrongType { .. }
        | ConfigError::Io { .. }
        | ConfigError::Unsynced { .. }
        | ConfigError::FiberHome(_)
        | ConfigError::Override { .. }
        | ConfigError::Refused { .. }
        | ConfigError::ProjectKey { .. }
        | ConfigError::Shape { .. }
        | ConfigError::CredentialMissing { .. }
        | ConfigError::CredentialFailed { .. }
        | ConfigError::DuplicateExtension { .. }
        | ConfigError::BadThemeName
        | ConfigError::ThemeRead(_) => format!(
            "the key for {provider} under {label} cannot be read: {} Check that you can \
             read that file, or run {login}.",
            clean(&error.to_string())
        ),
    }
}

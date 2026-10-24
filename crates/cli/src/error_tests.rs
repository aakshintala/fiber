//! One row per [`CliError`](super::CliError) case: its code and message.

use std::io;

use config::ConfigError;
use contract::ErrorCode;
use contract::shapes::Failure;

use super::{CliError, Refusal};
use crate::{finish, finish_code};
use config::Layer;

fn check(error: &CliError, code: ErrorCode, message: &str) {
    assert_eq!(error.code(), code);
    assert_eq!(error.to_string(), message);
    let failure = Failure::from(error);
    assert_eq!(failure.code, code);
    assert_eq!(failure.message, message);
    assert_eq!(failure.retry_after_ms, None);
    assert_eq!(failure.provider, None);
}

fn io_error() -> io::Error {
    io::Error::other("boom")
}

fn json_error() -> serde_json::Error {
    serde_json::from_str::<serde_json::Value>("{bad").unwrap_err()
}

#[test]
fn config_forwards_code_and_message() {
    let source = ConfigError::FiberHome("fiber-home");
    let message = source.to_string();
    check(&CliError::Config(source), ErrorCode::Usage, &message);
}

#[test]
fn credential_file_forwards_code_and_message() {
    let source = ConfigError::FiberHome("fiber-home");
    let message = source.to_string();
    check(
        &CliError::CredentialFile(source),
        ErrorCode::Usage,
        &message,
    );
}

#[test]
fn extensions_forwards_code_and_message() {
    let source = extensions::Error::GitMissing;
    let message = source.to_string();
    check(&CliError::Extensions(source), ErrorCode::Usage, &message);
}

#[test]
fn forwarded_returns_the_failure_unchanged() {
    let failure = Failure {
        code: ErrorCode::ConfigInvalid,
        message: "remote refused".to_owned(),
        retry_after_ms: Some(1),
        provider: Some(contract::shapes::ProviderFailure {
            name: "acme".to_owned(),
            status: Some(400),
            message: "bad".to_owned(),
        }),
    };
    let error = CliError::Forwarded(failure.clone());
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert_eq!(error.to_string(), "remote refused");
    assert_eq!(Failure::from(error), failure);
}

#[test]
fn terminal_names_the_terminal() {
    check(
        &CliError::Terminal(io_error()),
        ErrorCode::IoFailed,
        "the terminal: boom",
    );
}

#[test]
fn standard_error_names_standard_error() {
    check(
        &CliError::StandardError(io_error()),
        ErrorCode::IoFailed,
        "standard error: boom",
    );
}

#[test]
fn standard_input_names_standard_input() {
    check(
        &CliError::StandardInput(io_error()),
        ErrorCode::IoFailed,
        "standard input: boom",
    );
}

#[test]
fn current_directory_names_the_directory() {
    check(
        &CliError::CurrentDirectory(io_error()),
        ErrorCode::IoFailed,
        "the current directory: boom",
    );
}

#[test]
fn interrupt_handler_names_the_handler() {
    check(
        &CliError::InterruptHandler(io_error()),
        ErrorCode::IoFailed,
        "the interrupt handler: boom",
    );
}

#[test]
fn model_row_names_the_row() {
    let source = json_error();
    let message = format!("a model row: {source}");
    check(&CliError::ModelRow(source), ErrorCode::IoFailed, &message);
}

#[test]
fn config_value_names_the_key() {
    let source = json_error();
    let message = format!("a value for `model`: {source}");
    check(
        &CliError::ConfigValue {
            key: "model".to_owned(),
            source: json_error(),
        },
        ErrorCode::IoFailed,
        &message,
    );
}

#[test]
fn login_running_names_the_provider() {
    check(
        &CliError::LoginRunning {
            name: "acme".to_owned(),
        },
        ErrorCode::IoFailed,
        "another login for acme is running",
    );
}

#[test]
fn mcp_add_running_names_the_server() {
    check(
        &CliError::McpAddRunning {
            name: "srv".to_owned(),
        },
        ErrorCode::IoFailed,
        "another `fiber mcp add` of `srv` is running",
    );
}

#[test]
fn approval_not_recorded_names_file_and_why() {
    check(
        &CliError::ApprovalNotRecorded {
            name: "srv".to_owned(),
            file: std::path::PathBuf::from("/w/.fiber/config.json"),
            why: "locked".to_owned(),
        },
        ErrorCode::IoFailed,
        "could not record the approval of `srv` in /w/.fiber/config.json: locked. Run `fiber approve` to finish it.",
    );
}

#[test]
fn approval_not_stored_uses_the_source_code() {
    let source = extensions::Error::GitMissing;
    let message = format!(
        "could not record the approval of `srv` in /w/.fiber/config.json: {source}. Run `fiber approve` to finish it."
    );
    check(
        &CliError::ApprovalNotStored {
            name: "srv".to_owned(),
            file: std::path::PathBuf::from("/w/.fiber/config.json"),
            source: extensions::Error::GitMissing,
        },
        ErrorCode::Usage,
        &message,
    );
}

#[test]
fn secrets_kept_keeps_the_source_text_and_code() {
    let source = ConfigError::FiberHome("fiber-home");
    let message = format!("{source} The secrets the declaration refers to were kept.");
    let code = source.code();
    check(&CliError::SecretsKept(source), code, &message);
}

#[test]
fn approval_changed_names_the_server() {
    check(
        &CliError::ApprovalChanged {
            name: "srv".to_owned(),
        },
        ErrorCode::ConfigInvalid,
        "`srv` changed after it was written; the approval was not recorded. Run `fiber approve` to approve the current declaration.",
    );
}

#[test]
fn url_with_userinfo_names_the_server() {
    check(
        &CliError::UrlHasUserinfo {
            server: "srv".to_owned(),
        },
        ErrorCode::ConfigInvalid,
        "`srv`'s url carries a user name or password; remove it.",
    );
}

#[test]
fn mcp_login_refused_prefixes_the_server() {
    check(
        &CliError::McpLoginRefused {
            server: "srv".to_owned(),
            message: "pkce failed".to_owned(),
        },
        ErrorCode::ConfigInvalid,
        "`srv`: pkce failed",
    );
}

#[test]
fn mcp_unapproved_names_the_server() {
    check(
        &CliError::McpUnapproved {
            server: "srv".to_owned(),
        },
        ErrorCode::McpServerUnapproved,
        "`srv` is declared by the repository but not approved; run `fiber approve` in the repository.",
    );
}

#[test]
fn reviewer_name_unrepresentable_names_the_key() {
    check(
        &CliError::ReviewerNameUnrepresentable {
            key: "k".to_owned(),
        },
        ErrorCode::ConfigInvalid,
        "`k` cannot name a provider reviewer: the name is not one path segment.",
    );
}

#[test]
fn reviewer_not_saved_names_the_key() {
    let source = ConfigError::FiberHome("fiber-home");
    let message = format!("could not save `k`: {source}");
    let code = source.code();
    check(
        &CliError::ReviewerNotSaved {
            key: "k".to_owned(),
            source: ConfigError::FiberHome("fiber-home"),
        },
        code,
        &message,
    );
}

#[test]
fn login_cancelled_is_authentication_failed() {
    check(
        &CliError::LoginCancelled,
        ErrorCode::AuthenticationFailed,
        "the login was cancelled; nothing was stored.",
    );
}

#[test]
fn key_from_elsewhere_names_origin() {
    check(
        &CliError::KeyFromElsewhere {
            name: "acme".to_owned(),
            origin: "the FOO environment".to_owned(),
        },
        ErrorCode::CredentialMissing,
        "acme's key comes from the FOO environment; fiber logout cannot remove it",
    );
}

#[test]
fn no_stored_credential_names_the_provider() {
    check(
        &CliError::NoStoredCredential {
            name: "acme".to_owned(),
        },
        ErrorCode::CredentialMissing,
        "no stored credential for acme",
    );
}

#[test]
fn no_stored_label_names_label_and_provider() {
    check(
        &CliError::NoStoredLabel {
            name: "acme".to_owned(),
            label: "work".to_owned(),
        },
        ErrorCode::CredentialMissing,
        "no stored credential work for acme",
    );
}

#[test]
fn label_not_among_lists_the_stored_labels() {
    check(
        &CliError::LabelNotAmong {
            name: "acme".to_owned(),
            label: "work".to_owned(),
            labels: vec!["a".to_owned(), "b".to_owned()],
        },
        ErrorCode::CredentialMissing,
        "no stored credential work for acme; the stored labels are a, b",
    );
}

#[test]
fn model_none_without_closest_lists_models() {
    check(
        &CliError::ModelNone {
            typed: "m".to_owned(),
            closest: Vec::new(),
        },
        ErrorCode::NoModel,
        "No installed model matches `m`. Run `fiber models` to list them.",
    );
}

#[test]
fn model_none_with_one_closest_names_it() {
    check(
        &CliError::ModelNone {
            typed: "m".to_owned(),
            closest: vec!["a/m".to_owned()],
        },
        ErrorCode::NoModel,
        "No installed model matches `m`. Closest: `a/m`. Run `fiber models` to list them.",
    );
}

#[test]
fn model_none_with_three_closest_names_all() {
    check(
        &CliError::ModelNone {
            typed: "m".to_owned(),
            closest: vec!["a/m".to_owned(), "b/m".to_owned(), "c/m".to_owned()],
        },
        ErrorCode::NoModel,
        "No installed model matches `m`. Closest: `a/m`, `b/m`, `c/m`. Run `fiber models` to list them.",
    );
}

#[test]
fn model_ambiguous_keeps_the_fixed_code() {
    let source = extensions::Error::Ambiguous {
        id: "m".to_owned(),
        matches: vec!["a/m".to_owned(), "b/m".to_owned()],
    };
    let message = source.to_string();
    check(
        &CliError::ModelAmbiguous(source),
        ErrorCode::ModelAmbiguous,
        &message,
    );
}

#[test]
fn logout_needs_provider_is_the_logout_shape() {
    check(
        &CliError::LogoutNeedsProvider,
        ErrorCode::Usage,
        "`fiber logout` takes the provider to log out of. Run `fiber --help` for usage.",
    );
}

#[test]
fn provider_not_installed_without_installed() {
    check(
        &CliError::ProviderNotInstalled {
            name: "acme".to_owned(),
            installed: Vec::new(),
        },
        ErrorCode::Usage,
        "`acme` is not an installed provider, and none is installed. Run `fiber --help` for usage.",
    );
}

#[test]
fn provider_not_installed_with_installed() {
    check(
        &CliError::ProviderNotInstalled {
            name: "acme".to_owned(),
            installed: vec!["a".to_owned(), "b".to_owned()],
        },
        ErrorCode::Usage,
        "`acme` is not an installed provider; the installed providers are a, b. Run `fiber --help` for usage.",
    );
}

#[test]
fn login_needs_name_is_usage() {
    check(
        &CliError::LoginNeedsName,
        ErrorCode::Usage,
        "`fiber login` takes a provider or a secret's name when there is no terminal to ask on. Run `fiber --help` for usage.",
    );
}

#[test]
fn nothing_to_log_in_to_is_usage() {
    check(
        &CliError::NothingToLogInTo,
        ErrorCode::Usage,
        "No provider is installed, and no installed extension declares a secret. Run `fiber --help` for usage.",
    );
}

#[test]
fn nothing_chosen_is_usage() {
    check(
        &CliError::NothingChosen,
        ErrorCode::Usage,
        "Nothing was chosen. Run `fiber --help` for usage.",
    );
}

#[test]
fn choice_unknown_names_the_answer() {
    check(
        &CliError::ChoiceUnknown {
            answer: "zzz".to_owned(),
        },
        ErrorCode::Usage,
        "`zzz` is neither a listed number nor an installed provider or declared secret. Run `fiber --help` for usage.",
    );
}

#[test]
fn device_needs_browser_provider_is_usage() {
    check(
        &CliError::DeviceNeedsBrowserProvider,
        ErrorCode::Usage,
        "--device applies only to a provider that logs in by browser. Run `fiber --help` for usage.",
    );
}

#[test]
fn logout_several_names_the_labels() {
    check(
        &CliError::LogoutSeveral {
            name: "acme".to_owned(),
            labels: vec!["a".to_owned(), "b".to_owned()],
        },
        ErrorCode::Usage,
        "`acme` has several stored credentials: a, b; name one with --as <label>, or use --all. Run `fiber --help` for usage.",
    );
}

#[test]
fn label_not_storable_names_the_label() {
    check(
        &CliError::LabelNotStorable {
            label: "a/b".to_owned(),
        },
        ErrorCode::Usage,
        "`a/b` is not a label `fiber login` can store; pass one with `--as <label>`. Run `fiber --help` for usage.",
    );
}

#[test]
fn store_refused_no_key_without_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::NoKeyGiven,
            hint: false,
        },
        ErrorCode::Usage,
        "No key was given; nothing was stored.",
    );
}

#[test]
fn store_refused_no_key_with_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::NoKeyGiven,
            hint: true,
        },
        ErrorCode::Usage,
        "No key was given; nothing was stored. Run `fiber --help` for usage.",
    );
}

#[test]
fn store_refused_no_value_without_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::NoValueGiven,
            hint: false,
        },
        ErrorCode::Usage,
        "No value was given; nothing was stored.",
    );
}

#[test]
fn store_refused_no_value_with_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::NoValueGiven,
            hint: true,
        },
        ErrorCode::Usage,
        "No value was given; nothing was stored. Run `fiber --help` for usage.",
    );
}

#[test]
fn store_refused_as_only_without_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::AsOnlyForProvider {
                name: "sec".to_owned(),
            },
            hint: false,
        },
        ErrorCode::Usage,
        "--as applies only to a provider, and `sec` is a declared secret.",
    );
}

#[test]
fn store_refused_as_only_with_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::AsOnlyForProvider {
                name: "sec".to_owned(),
            },
            hint: true,
        },
        ErrorCode::Usage,
        "--as applies only to a provider, and `sec` is a declared secret. Run `fiber --help` for usage.",
    );
}

#[test]
fn store_refused_unknown_name_without_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::UnknownName {
                name: "zzz".to_owned(),
                providers: Vec::new(),
                secrets: Vec::new(),
            },
            hint: false,
        },
        ErrorCode::Usage,
        "`zzz` is neither an installed provider nor a declared secret; no provider is installed, and no installed extension declares a secret.",
    );
}

#[test]
fn store_refused_unknown_name_with_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::UnknownName {
                name: "zzz".to_owned(),
                providers: vec!["a".to_owned()],
                secrets: vec!["s".to_owned()],
            },
            hint: true,
        },
        ErrorCode::Usage,
        "`zzz` is neither an installed provider nor a declared secret; the installed providers are a, and the declared secrets are s. Run `fiber --help` for usage.",
    );
}

#[test]
fn store_refused_already_stored_without_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::AlreadyStored {
                name: "acme".to_owned(),
                stored: "acme".to_owned(),
                label: "work".to_owned(),
            },
            hint: false,
        },
        ErrorCode::Usage,
        "credentials/acme/work is already stored; log in under another label with --as <label>, or run `fiber logout acme --as work` first.",
    );
}

#[test]
fn store_refused_already_stored_with_hint() {
    check(
        &CliError::StoreRefused {
            refusal: Refusal::AlreadyStored {
                name: "acme".to_owned(),
                stored: "acme".to_owned(),
                label: "work".to_owned(),
            },
            hint: true,
        },
        ErrorCode::Usage,
        "credentials/acme/work is already stored; log in under another label with --as <label>, or run `fiber logout acme --as work` first. Run `fiber --help` for usage.",
    );
}

#[test]
fn config_key_unknown_names_the_key() {
    check(
        &CliError::ConfigKeyUnknown {
            key: "zzz".to_owned(),
        },
        ErrorCode::Usage,
        "`zzz` is not a configuration key. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_name_names_the_name() {
    check(
        &CliError::McpName {
            name: "".to_owned(),
        },
        ErrorCode::Usage,
        "`` is not a server name: it is non-empty and holds no `\"`, `/`, whitespace or control character. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_env_key_names_the_key() {
    check(
        &CliError::McpEnvKey {
            key: "1x".to_owned(),
        },
        ErrorCode::Usage,
        "`1x` is not a variable name: it matches `[A-Za-z_][A-Za-z0-9_]*`. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_env_pair_names_the_raw() {
    check(
        &CliError::McpEnvPair {
            raw: "FOO".to_owned(),
        },
        ErrorCode::Usage,
        "`FOO` is not `KEY=value`. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_url_and_command_is_usage() {
    check(
        &CliError::McpUrlAndCommand,
        ErrorCode::Usage,
        "`fiber mcp add` takes either a url or `-- <command>`, not both. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_transport_missing_is_usage() {
    check(
        &CliError::McpTransportMissing,
        ErrorCode::Usage,
        "`fiber mcp add` takes a url or `-- <command>`. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_url_with_env_is_usage() {
    check(
        &CliError::McpUrlWithEnv,
        ErrorCode::Usage,
        "`-e` and `--secret` apply only to `-- <command>`, not to a url. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_program_missing_is_usage() {
    check(
        &CliError::McpProgramMissing,
        ErrorCode::Usage,
        "`fiber mcp add` takes a program after `--`. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_key_repeated_names_the_key() {
    check(
        &CliError::McpKeyRepeated {
            key: "FOO".to_owned(),
        },
        ErrorCode::Usage,
        "`FOO` is given more than once; each variable is declared once. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_secret_empty_names_the_key() {
    check(
        &CliError::McpSecretEmpty {
            key: "FOO".to_owned(),
        },
        ErrorCode::Usage,
        "No value was given for `--secret FOO`; nothing was stored. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_already_declared_global() {
    check(
        &CliError::McpAlreadyDeclared {
            name: "srv".to_owned(),
            layer: Layer::Global,
        },
        ErrorCode::Usage,
        "`srv` is already declared in the global layer. Run `fiber mcp remove srv` to remove it first. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_already_declared_project() {
    check(
        &CliError::McpAlreadyDeclared {
            name: "srv".to_owned(),
            layer: Layer::Project,
        },
        ErrorCode::Usage,
        "`srv` is already declared in the project layer. Run `fiber mcp remove srv` to remove it first. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_already_declared_repository() {
    check(
        &CliError::McpAlreadyDeclared {
            name: "srv".to_owned(),
            layer: Layer::Repository,
        },
        ErrorCode::Usage,
        "`srv` is already declared in the repository layer. Run `fiber mcp remove srv` to remove it first. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_not_declared_global_without_layers() {
    check(
        &CliError::McpNotDeclared {
            name: "srv".to_owned(),
            layer: Layer::Global,
            declared_in: Vec::new(),
        },
        ErrorCode::Usage,
        "`srv` is not declared in the global layer; no layer declares it. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_not_declared_project_with_layers() {
    check(
        &CliError::McpNotDeclared {
            name: "srv".to_owned(),
            layer: Layer::Project,
            declared_in: vec![Layer::Global, Layer::Repository],
        },
        ErrorCode::Usage,
        "`srv` is not declared in the project layer; it is declared in the global, repository layer(s). Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_not_declared_repository_with_one_layer() {
    check(
        &CliError::McpNotDeclared {
            name: "srv".to_owned(),
            layer: Layer::Repository,
            declared_in: vec![Layer::Project],
        },
        ErrorCode::Usage,
        "`srv` is not declared in the repository layer; it is declared in the project layer(s). Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_server_unknown_names_the_server() {
    check(
        &CliError::McpServerUnknown {
            server: "srv".to_owned(),
        },
        ErrorCode::Usage,
        "no server named `srv` is declared. Run `fiber --help` for usage.",
    );
}

#[test]
fn mcp_server_stdio_names_the_server() {
    check(
        &CliError::McpServerStdio {
            server: "srv".to_owned(),
        },
        ErrorCode::Usage,
        "`srv` declares a command, not a url; only a remote server holds a token. Run `fiber --help` for usage.",
    );
}

#[test]
fn nobody_to_ask_keeps_the_calling_command_sentence() {
    check(
        &CliError::NobodyToAsk {
            message: "`fiber approve` has no terminal to ask on and no input. Pass `--yes` to approve without asking.".to_owned(),
        },
        ErrorCode::Usage,
        "`fiber approve` has no terminal to ask on and no input. Pass `--yes` to approve without asking.",
    );
}

#[test]
fn approve_failed_takes_its_source_code() {
    let source = extensions::Error::GitMissing;
    let message = format!("could not approve extension fiber.test/bad: {source}");
    check(
        &CliError::ApproveFailed {
            kind: "extension",
            name: "fiber.test/bad".to_owned(),
            source,
        },
        ErrorCode::Usage,
        &message,
    );
}

#[test]
fn release_version_mismatch_names_both_versions() {
    check(
        &CliError::ReleaseVersionMismatch {
            fiber_version: "0.1.0".to_owned(),
            version: "0.2.0".to_owned(),
        },
        ErrorCode::Usage,
        "This is Fiber 0.1.0; it cannot install the docs and extensions of 0.2.0.",
    );
}

#[test]
fn release_no_commit_is_extension_incompatible() {
    check(
        &CliError::ReleaseNoCommit,
        ErrorCode::ExtensionIncompatible,
        "This build records no commit, so it cannot install a release's extensions.",
    );
}

#[test]
fn extension_directory_prints_the_bare_error() {
    check(
        &CliError::ExtensionDirectory(io_error()),
        ErrorCode::IoFailed,
        "boom",
    );
}

#[test]
fn fiber_executable_prints_the_bare_error() {
    check(
        &CliError::FiberExecutable("no fiber binary".to_owned()),
        ErrorCode::IoFailed,
        "no fiber binary",
    );
}

#[test]
fn doctor_report_keeps_its_sentence() {
    check(
        &CliError::DoctorReport(io_error()),
        ErrorCode::IoFailed,
        "doctor could not write its report: boom",
    );
}

#[test]
fn doctor_directory_keeps_its_sentence() {
    check(
        &CliError::DoctorDirectory(io_error()),
        ErrorCode::IoFailed,
        "doctor could not read the current directory: boom",
    );
}

#[test]
fn no_first_party_package_names_the_provider() {
    check(
        &CliError::NoFirstPartyPackage {
            provider: "scripted".to_owned(),
        },
        ErrorCode::InvalidArguments,
        "No first-party package serves the provider `scripted`.",
    );
}

#[test]
fn older_than_unreadable_is_usage() {
    check(
        &CliError::OlderThanUnreadable,
        ErrorCode::Usage,
        "`--older-than` takes a duration such as `30d`: a whole number and `s`, `m`, `h` or `d`.",
    );
}

#[test]
fn prune_failed_forwards_a_non_io_code() {
    check(
        &CliError::PruneFailed {
            code: ErrorCode::SessionHeld,
            failed: 2,
            total: 7,
        },
        ErrorCode::SessionHeld,
        "2 of 7 could not be deleted",
    );
}

#[test]
fn log_ambiguous_is_usage_naming_both_matches() {
    let source = log::Error::Ambiguous {
        selector: "s_".to_owned(),
        matches: vec!["s_a".to_owned(), "s_b".to_owned()],
    };
    let message = source.to_string();
    check(&CliError::Log(source), ErrorCode::Usage, &message);
}

#[test]
fn log_not_found_is_session_not_found() {
    let source = log::Error::NotFound("s_missing".into());
    let message = source.to_string();
    check(&CliError::Log(source), ErrorCode::SessionNotFound, &message);
}

#[test]
fn hub_local_names_the_hub() {
    check(
        &CliError::Hub(doors::Error::Local(io_error())),
        ErrorCode::IoFailed,
        "the hub: boom",
    );
}

#[test]
fn hub_keeps_the_doors_text_and_code() {
    let source = doors::Error::UnknownHub { name: "box".into() };
    let message = source.to_string();
    check(&CliError::Hub(source), ErrorCode::Usage, &message);
}

#[test]
fn hub_closed_names_the_command() {
    check(
        &CliError::HubClosed {
            command: "delete".to_owned(),
        },
        ErrorCode::IoFailed,
        "the hub closed the connection before answering `delete`",
    );
}

#[test]
fn rejected_carries_its_code_and_message() {
    check(
        &CliError::Rejected {
            code: ErrorCode::SessionHasDependents,
            message: "It has forks.".to_owned(),
        },
        ErrorCode::SessionHasDependents,
        "It has forks.",
    );
}

#[test]
fn rejected_with_another_code_forwards_that_code() {
    check(
        &CliError::Rejected {
            code: ErrorCode::InvalidArguments,
            message: "no".to_owned(),
        },
        ErrorCode::InvalidArguments,
        "no",
    );
}

#[test]
fn rejected_with_an_unknown_wire_code_is_other() {
    check(
        &CliError::Rejected {
            code: ErrorCode::Other("future".into()),
            message: "m".to_owned(),
        },
        ErrorCode::Other("future".into()),
        "m",
    );
}

#[test]
fn command_encode_keeps_its_sentence() {
    let source = json_error();
    let message = format!("a command could not be written as JSON: {source}");
    check(
        &CliError::CommandEncode(source),
        ErrorCode::IoFailed,
        &message,
    );
}

#[test]
fn sessions_answer_keeps_its_sentence() {
    let source = json_error();
    let message = format!("the hub's answer to `sessions`: {source}");
    check(
        &CliError::SessionsAnswer(source),
        ErrorCode::IoFailed,
        &message,
    );
}

#[test]
fn session_row_keeps_its_sentence() {
    let source = json_error();
    let message = format!("a session row: {source}");
    check(&CliError::SessionRow(source), ErrorCode::IoFailed, &message);
}

#[test]
fn search_hit_keeps_its_sentence() {
    let source = json_error();
    let message = format!("a search hit: {source}");
    check(&CliError::SearchHit(source), ErrorCode::IoFailed, &message);
}

#[test]
fn delete_nobody_is_usage() {
    check(
        &CliError::DeleteNobody,
        ErrorCode::Usage,
        crate::sessions::DELETE_NOBODY,
    );
}

#[test]
fn session_not_found_on_hub_names_the_selector_and_hub() {
    check(
        &CliError::SessionNotFoundOnHub {
            selector: "s_nope".to_owned(),
            hub: "box".to_owned(),
        },
        ErrorCode::SessionNotFound,
        "no session matches \"s_nope\" on hub `box`",
    );
}

#[test]
fn session_not_found_on_hub_with_an_empty_selector() {
    check(
        &CliError::SessionNotFoundOnHub {
            selector: String::new(),
            hub: "box".to_owned(),
        },
        ErrorCode::SessionNotFound,
        "no session matches \"\" on hub `box`",
    );
}

fn usage_cause() -> CliError {
    CliError::Forwarded(Failure {
        code: ErrorCode::Usage,
        message: "it failed.".to_owned(),
        retry_after_ms: None,
        provider: None,
    })
}

fn unit_lost() -> CliError {
    CliError::UnitDeleteFailed {
        unit: "/u/x.service".into(),
        source: io::Error::other("denied"),
    }
}

#[test]
fn current_exe_names_the_binary_path() {
    check(
        &CliError::CurrentExe(io_error()),
        ErrorCode::IoFailed,
        "the running binary's path: boom",
    );
}

#[test]
fn launchd_still_loaded_names_the_service() {
    check(
        &CliError::LaunchdStillLoaded {
            name: "fiber-hub-x".to_owned(),
        },
        ErrorCode::IoFailed,
        "launchd still had fiber-hub-x loaded 10 s after `launchctl bootout`.",
    );
}

fn manager_not_found(program: &str) -> CliError {
    CliError::ManagerNotFound {
        program: program.to_owned(),
    }
}

fn manager_failed(program: &str, args: &[&str], exit: Option<i32>, said: Option<&str>) -> CliError {
    CliError::ManagerFailed {
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        exit,
        said: said.map(str::to_owned),
    }
}

#[test]
fn manager_errors_name_the_manager_and_each_failure_branch() {
    let restart = ["--user", "restart", "x.service"];
    let rows = [
        (
            manager_not_found("launchctl"),
            "`launchctl` was not found; an installed hub needs launchd.",
        ),
        (
            manager_not_found("systemctl"),
            "`systemctl` was not found; an installed hub needs systemd's user instance.",
        ),
        (
            manager_failed("systemctl", &restart, Some(5), None),
            "`systemctl --user restart x.service` failed with exit code 5.",
        ),
        (
            manager_failed("systemctl", &restart, Some(5), Some("boom")),
            "`systemctl --user restart x.service` failed with exit code 5: boom.",
        ),
        (
            manager_failed("systemctl", &restart, None, None),
            "`systemctl --user restart x.service` failed with no exit code.",
        ),
        (
            manager_failed("systemctl", &restart, None, Some("boom")),
            "`systemctl --user restart x.service` failed with no exit code: boom.",
        ),
        (
            manager_failed("launchctl", &[], Some(1), None),
            "`launchctl ` failed with exit code 1.",
        ),
    ];
    for (error, message) in &rows {
        check(error, ErrorCode::IoFailed, message);
    }
}

#[test]
fn manager_run_names_the_program_and_the_cause() {
    check(
        &CliError::ManagerRun {
            program: "systemctl".to_owned(),
            source: io::Error::other("denied"),
        },
        ErrorCode::IoFailed,
        "running `systemctl`: denied",
    );
}

#[test]
fn unit_read_failed_names_the_unit() {
    check(
        &CliError::UnitReadFailed {
            unit: "/u/x.service".into(),
            source: io::Error::other("denied"),
        },
        ErrorCode::IoFailed,
        "reading /u/x.service: denied",
    );
}

#[test]
fn unit_write_failed_is_io_failed_not_the_config_code() {
    let source = ConfigError::FiberHome("fiber-home");
    assert_eq!(source.code(), ErrorCode::Usage);
    let message = source.to_string();
    check(
        &CliError::UnitWriteFailed(source),
        ErrorCode::IoFailed,
        &message,
    );
}

#[test]
fn unit_delete_failed_names_the_unit() {
    check(
        &unit_lost(),
        ErrorCode::IoFailed,
        "deleting /u/x.service: denied",
    );
}

#[test]
fn unit_not_restored_keeps_the_cause_text_and_code() {
    check(
        &CliError::UnitNotRestored {
            cause: Box::new(usage_cause()),
            unit: "/u/x.service".into(),
            lost: Box::new(unit_lost()),
        },
        ErrorCode::Usage,
        "it failed. The unit file /u/x.service could not be put back (deleting /u/x.service: denied); run fiber hub uninstall.",
    );
}

#[test]
fn unit_not_restored_over_an_io_cause_is_io_failed() {
    check(
        &CliError::UnitNotRestored {
            cause: Box::new(CliError::CurrentExe(io_error())),
            unit: "/u/x.service".into(),
            lost: Box::new(unit_lost()),
        },
        ErrorCode::IoFailed,
        "the running binary's path: boom The unit file /u/x.service could not be put back (deleting /u/x.service: denied); run fiber hub uninstall.",
    );
}

#[test]
fn rerun_install_keeps_the_cause_text_and_code() {
    check(
        &CliError::RerunInstall {
            cause: Box::new(usage_cause()),
        },
        ErrorCode::Usage,
        "it failed. Run fiber hub install again, or fiber hub uninstall to remove what was written.",
    );
}

#[test]
fn rerun_install_over_an_io_cause_is_io_failed() {
    check(
        &CliError::RerunInstall {
            cause: Box::new(CliError::CurrentExe(io_error())),
        },
        ErrorCode::IoFailed,
        "the running binary's path: boom Run fiber hub install again, or fiber hub uninstall to remove what was written.",
    );
}

#[test]
fn a_rewrap_takes_the_cause_code_at_every_depth() {
    let inner = CliError::RerunInstall {
        cause: Box::new(usage_cause()),
    };
    let outer = CliError::UnitNotRestored {
        cause: Box::new(inner),
        unit: "/u/x.service".into(),
        lost: Box::new(unit_lost()),
    };
    assert_eq!(outer.code(), ErrorCode::Usage);
}

#[test]
fn hub_request_encode_names_the_hub() {
    let source = json_error();
    let message = format!("the hub: {source}");
    check(
        &CliError::HubRequestEncode(source),
        ErrorCode::IoFailed,
        &message,
    );
}

#[test]
fn hub_closed_before_answer_has_no_backticks() {
    check(
        &CliError::HubClosedBeforeAnswer,
        ErrorCode::IoFailed,
        "the hub closed the connection before answering stop",
    );
}

#[test]
fn hub_stop_late_names_the_seconds() {
    check(
        &CliError::HubStopLate { secs: 15 },
        ErrorCode::IoFailed,
        "the hub did not stop in 15 s",
    );
}

#[test]
fn stop_count_missing_says_the_count_is_missing() {
    check(
        &CliError::StopCountMissing,
        ErrorCode::IoFailed,
        "the hub answered stop without a session count",
    );
}

#[test]
fn pairing_answer_missing_says_the_code_is_missing() {
    check(
        &CliError::PairingAnswerMissing,
        ErrorCode::IoFailed,
        "the hub's pairing answer has no code",
    );
}

#[test]
fn qr_too_long_is_usage() {
    check(
        &CliError::QrTooLong,
        ErrorCode::Usage,
        "The address is too long to draw as a QR code.",
    );
}

#[test]
fn status_no_variables_names_status() {
    check(
        &CliError::StatusNoVariables,
        ErrorCode::IoFailed,
        "the hub's answer to `status` has no variables",
    );
}

#[test]
fn devices_malformed_names_devices() {
    check(
        &CliError::DevicesMalformed,
        ErrorCode::IoFailed,
        "the hub's answer to `devices` is not a device list",
    );
}

#[test]
fn hub_unit_name_too_long_names_the_bytes() {
    check(
        &CliError::HubUnitNameTooLong { bytes: 241 },
        ErrorCode::Usage,
        "Fiber home's path is too long to name the hub's login service (241 bytes, at most \
         240); set FIBER_HOME to a shorter path.",
    );
}

#[test]
fn hub_home_unavailable_names_home() {
    check(
        &CliError::HubHomeUnavailable,
        ErrorCode::Usage,
        "HOME must be an absolute path to find the hub's login service.",
    );
}

#[test]
fn hub_unit_path_unrepresentable_names_the_path() {
    let path = std::path::PathBuf::from("/b\nin/f");
    check(
        &CliError::HubUnitPathUnrepresentable { path: path.clone() },
        ErrorCode::IoFailed,
        &format!(
            "{path:?} is not UTF-8 or holds a control character, so the hub's login \
             service cannot name it."
        ),
    );
}

#[test]
fn hub_pairing_code_missing_is_usage() {
    check(
        &CliError::HubPairingCodeMissing,
        ErrorCode::Usage,
        "No pairing code was given. Run `fiber --help` for usage.",
    );
}

#[test]
fn hub_pairing_token_missing_names_the_token() {
    check(
        &CliError::HubPairingTokenMissing,
        ErrorCode::IoFailed,
        "the hub's pairing answer has no token",
    );
}

#[test]
fn hub_list_locked_names_the_running_command() {
    check(
        &CliError::HubListLocked {
            name: "box".to_owned(),
        },
        ErrorCode::IoFailed,
        "another `fiber hub add` or `fiber hub remove` of `box` is running",
    );
}

#[test]
fn hub_already_declared_names_the_removal() {
    check(
        &CliError::HubAlreadyDeclared {
            name: "box".to_owned(),
        },
        ErrorCode::Usage,
        "`box` is already declared in the global layer. Run `fiber hub remove box` to remove \
         it first. Run `fiber --help` for usage.",
    );
}

#[test]
fn hub_not_found_names_the_list() {
    check(
        &CliError::HubNotFound {
            name: "box".to_owned(),
        },
        ErrorCode::Usage,
        "No hub named `box` in this client's list. Run `fiber --help` for usage.",
    );
}

#[test]
fn hub_entry_failed_keeps_the_source_code_and_suffix() {
    let file = std::path::PathBuf::from("/tmp/config.json");
    let unsynced = || ConfigError::Unsynced {
        file: file.clone(),
        source: io::Error::other("sync failed"),
    };
    let plain = || ConfigError::Io {
        file: file.clone(),
        source: io::Error::other("write failed"),
    };
    let kept = unsynced();
    check(
        &CliError::HubEntryFailed {
            source: unsynced(),
            paired: true,
        },
        ErrorCode::IoFailed,
        &format!("{kept} The device token the entry refers to was kept."),
    );
    let message = unsynced().to_string();
    check(
        &CliError::HubEntryFailed {
            source: unsynced(),
            paired: false,
        },
        ErrorCode::IoFailed,
        &message,
    );
    let message = plain().to_string();
    check(
        &CliError::HubEntryFailed {
            source: plain(),
            paired: true,
        },
        ErrorCode::IoFailed,
        &message,
    );
}

#[test]
fn hub_port_invalid_names_the_file_and_value() {
    check(
        &CliError::HubPortInvalid {
            value: serde_json::json!("fast"),
            path: std::path::PathBuf::from("/home/a/.fiber/config.json"),
        },
        ErrorCode::ConfigInvalid,
        "hub.port in /home/a/.fiber/config.json is not a port: \"fast\"",
    );
}

#[test]
fn hub_devices_read_names_the_directory() {
    check(
        &CliError::HubDevicesRead {
            path: std::path::PathBuf::from("/home/a/.fiber/hub/devices"),
            source: io_error(),
        },
        ErrorCode::IoFailed,
        "reading /home/a/.fiber/hub/devices: boom",
    );
}

#[test]
fn hub_status_late_names_the_seconds() {
    check(
        &CliError::HubStatusLate { seconds: 5.0 },
        ErrorCode::IoFailed,
        "the hub did not answer status in 5 s",
    );
}

#[test]
fn hub_status_closed_names_the_close() {
    check(
        &CliError::HubStatusClosed,
        ErrorCode::IoFailed,
        "the hub closed the connection before answering status",
    );
}

#[test]
fn hub_status_answer_missing_names_version_and_clients() {
    check(
        &CliError::HubStatusAnswerMissing,
        ErrorCode::IoFailed,
        "the hub's answer to status has no fiber_version or clients",
    );
}

#[test]
fn hub_status_json_names_the_status() {
    let source = json_error();
    let message = format!("the hub's status: {source}");
    check(
        &CliError::HubStatusJson(source),
        ErrorCode::IoFailed,
        &message,
    );
}

#[test]
fn finish_maps_a_cli_error_to_its_exit_code() {
    assert_eq!(finish(Err(CliError::LogoutNeedsProvider)), 2);
    assert_eq!(
        finish(Err(CliError::LoginRunning {
            name: "acme".to_owned(),
        })),
        1
    );
}

#[test]
fn finish_code_passes_an_ok_value_through() {
    assert_eq!(finish_code(Ok::<i32, Failure>(7)), 7);
}

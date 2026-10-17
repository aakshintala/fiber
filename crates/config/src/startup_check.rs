//! The startup classification both `main` and `fiber doctor` share:
//! which `mcp.servers` entries can start, which are refused, and which
//! repository servers start only approved; plus a bounded script read.
//! Classification reads no value and runs nothing: the secret names it
//! keeps are names only, and its `Debug` prints no command, argument,
//! environment value or URL.

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::Path;

use serde_json::Value;

use crate::{Config, ConfigError, Source, declared, mcp_secret_name_ok};

/// The `mcp.servers` entries, classified: runnable ones with the
/// repository's flagged, and refusals with their notices' wording. Names
/// come out in name order.
pub struct McpServers {
    /// One per entry with a `command` or a `url`, runnable or not.
    pub servers: Vec<McpServer>,
    /// One per refused entry, in name order.
    pub refused: Vec<McpRefusal>,
}

impl fmt::Debug for McpServers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServers")
            .field(
                "servers",
                &self
                    .servers
                    .iter()
                    .map(|server| server.name.as_str())
                    .collect::<Vec<_>>(),
            )
            .field("refused", &self.refused)
            .finish()
    }
}

/// One runnable `mcp.servers` entry: everything `main` needs to build
/// its spec, with no value read and nothing run.
pub struct McpServer {
    /// The entry's name.
    pub name: String,
    /// Whether the entry is `required`.
    pub required: bool,
    /// The entry's transport, with names but no values.
    pub transport: McpTransport,
    /// Whether the entry starts only approved: the repository declares
    /// it, or any effective `command`, `args`, `env` or `url` entry
    /// comes from the repository's layer.
    pub repository: bool,
}

impl fmt::Debug for McpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServer")
            .field("name", &self.name)
            .field("required", &self.required)
            .field("transport", &self.transport)
            .field("repository", &self.repository)
            .finish()
    }
}

/// One entry's transport: the command line or the URL, by shape only.
pub enum McpTransport {
    /// A stdio server: the command, arguments, literal environment and
    /// secret references by key.
    Stdio {
        /// The `command`.
        command: String,
        /// The `args`.
        args: Vec<String>,
        /// Literal `env` entries, by key.
        env: BTreeMap<String, String>,
        /// `{ "secret" }` references, by key.
        secrets: BTreeMap<String, String>,
    },
    /// A remote server: its `url`.
    Remote {
        /// The `url`.
        url: String,
    },
}

impl fmt::Debug for McpTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            McpTransport::Stdio { env, secrets, .. } => f
                .debug_struct("Stdio")
                .field("command", &"redacted")
                .field("args", &"redacted")
                .field(
                    "env",
                    &env.keys()
                        .map(|name| (name, "redacted"))
                        .collect::<BTreeMap<_, _>>(),
                )
                .field("secrets", &secrets.keys().collect::<Vec<_>>())
                .finish(),
            McpTransport::Remote { .. } => f.debug_tuple("Remote").field(&"redacted").finish(),
        }
    }
}

/// One refused `mcp.servers` entry: its name and why Fiber never starts it.
pub struct McpRefusal {
    /// The entry's name.
    pub name: String,
    /// Whether the entry is `required`: then the refusal fails startup.
    pub required: bool,
    /// Why the entry is refused.
    pub kind: McpRefusalKind,
}

impl fmt::Debug for McpRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpRefusal")
            .field("name", &self.name)
            .field("required", &self.required)
            .field("kind", &self.kind)
            .finish()
    }
}

/// Why an entry is refused. The secret is a name, never a value.
#[derive(Debug)]
pub enum McpRefusalKind {
    /// Both `command` and `url`.
    Both,
    /// A URL holding a user name or password.
    Userinfo,
    /// An env `{ "secret" }` name that is not exactly
    /// `mcp.<server>.<KEY>`.
    BadSecret {
        /// The secret name, never its value.
        secret: String,
    },
}

impl McpRefusal {
    /// The notice's wording: the same sentence `main` logs on startup.
    pub fn message(&self) -> String {
        let name = &self.name;
        match &self.kind {
            McpRefusalKind::Both if self.required => format!(
                "The required MCP server `{name}` declares both `command` and `url`, so Fiber cannot start it. Keep the one you mean."
            ),
            McpRefusalKind::Both => format!(
                "The MCP server `{name}` declares both `command` and `url`, so Fiber starts neither. Keep the one you mean."
            ),
            McpRefusalKind::Userinfo if self.required => format!(
                "The required MCP server `{name}` has a user name or password in its `url`, so Fiber cannot start it. Take them out of the URL."
            ),
            McpRefusalKind::Userinfo => format!(
                "The MCP server `{name}` has a user name or password in its `url`, so Fiber does not start it. Take them out of the URL."
            ),
            McpRefusalKind::BadSecret { secret } if self.required => format!(
                "The required MCP server `{name}` names the secret `{secret}`, which must be `mcp.{name}.<KEY>`, so Fiber cannot start it. Name the secret after the server."
            ),
            McpRefusalKind::BadSecret { secret } => format!(
                "The MCP server `{name}` names the secret `{secret}`, which must be `mcp.{name}.<KEY>`, so Fiber does not start it. Name the secret after the server."
            ),
        }
    }
}

/// What one `mcp.servers` entry declares: runnable, refused, or nothing.
enum McpEntry {
    /// A runnable server, before the repository partition.
    Server {
        /// The entry's name.
        name: String,
        /// Whether the entry is `required`.
        required: bool,
        /// The entry's transport.
        transport: McpTransport,
    },
    /// A refused entry.
    Refused(McpRefusal),
    /// Neither `command` nor `url`: nothing to start.
    Neither,
}

/// Reads the merged `mcp.servers`: one server per entry with a `command`
/// or a `url`, with the repository's flagged, and one refusal per refused
/// entry. An entry with neither is skipped silently. A `required`
/// refusal fails the whole read. `has_userinfo` decides whether a URL
/// holds a user name or password, so tests inject it and read no network.
pub fn mcp_servers(
    config: &Config,
    has_userinfo: &dyn Fn(&str) -> bool,
) -> Result<McpServers, McpRefusal> {
    let mut servers = Vec::new();
    let mut refused = Vec::new();
    let Some((Value::Object(entries), _)) = config.get("mcp.servers", None) else {
        return Ok(McpServers { servers, refused });
    };
    let workspace = config.extensions().workspace().to_path_buf();
    let declared = declared(&workspace).ok();
    // `serde_json::Map` is a `BTreeMap` without `preserve_order`
    // (`docs/dependencies.md`), so this is name order.
    for name in entries.keys() {
        match classify(config, name, has_userinfo) {
            McpEntry::Neither => {}
            McpEntry::Refused(refusal) => {
                if refusal.required {
                    return Err(refusal);
                }
                refused.push(refusal);
            }
            McpEntry::Server {
                name,
                required,
                transport,
            } => {
                let declared_entry = declared
                    .as_ref()
                    .is_some_and(|declared| declared.mcp_servers.contains_key(name.as_str()));
                // An error reading the declarations puts every server
                // with a `command` on the repository side: nothing runs
                // unapproved. The declarations name every server the
                // repository layer can win a field for — both read the
                // workspace's `.fiber/config.json`, and a field the file
                // holds keeps its server declared — so per-field
                // provenance legs could never decide this and are gone:
                // any test reaching one would pass with it deleted.
                let repository = declared_entry || declared.is_none();
                servers.push(McpServer {
                    name: name.clone(),
                    required,
                    transport,
                    repository,
                });
            }
        }
    }
    Ok(McpServers { servers, refused })
}

/// What the server `name` declares: runnable, refused, or nothing. The
/// merged `command`, `args`, `env` and `url` are the person's and the
/// repository's merged key by key; the repository's share is partitioned
/// out by [`mcp_servers`], never skipped here.
fn classify(config: &Config, name: &str, has_userinfo: &dyn Fn(&str) -> bool) -> McpEntry {
    let command = config.text(&mcp_server_key(name, "command"), None);
    let url = config.text(&mcp_server_key(name, "url"), None);
    let required = config
        .flag(&mcp_server_key(name, "required"), None)
        .unwrap_or_default();
    if url.as_deref().is_some_and(has_userinfo) {
        return McpEntry::Refused(McpRefusal {
            name: name.to_owned(),
            required,
            kind: McpRefusalKind::Userinfo,
        });
    }
    let transport = match (command, url) {
        (Some(_), Some(_)) => {
            return McpEntry::Refused(McpRefusal {
                name: name.to_owned(),
                required,
                kind: McpRefusalKind::Both,
            });
        }
        (Some(command), None) => {
            let args = config.list(&mcp_server_key(name, "args"), None);
            let (env, secrets) = server_env(config, name);
            for secret in secrets.values() {
                if !mcp_secret_name_ok(name, secret) {
                    return McpEntry::Refused(McpRefusal {
                        name: name.to_owned(),
                        required,
                        kind: McpRefusalKind::BadSecret {
                            secret: secret.clone(),
                        },
                    });
                }
            }
            McpTransport::Stdio {
                command,
                args,
                env,
                secrets,
            }
        }
        (None, Some(url)) => McpTransport::Remote { url },
        (None, None) => return McpEntry::Neither,
    };
    McpEntry::Server {
        name: name.to_owned(),
        required,
        transport,
    }
}

/// The merged value of `mcp.servers."<name>".<field>` and its layer.
fn server_field(config: &Config, server: &str, field: &str) -> Option<(Value, Source)> {
    config.get(&mcp_server_key(server, field), None)
}

/// The merged `env`: literals by key, and `{ "secret" }` references by
/// key, never their values. The load check holds every layer to strings
/// or `{ "secret" }` objects, so anything else here is unreachable and
/// ignored. A name that is not exactly `mcp.<server>.<KEY>` is kept, and
/// refused by classification, so the refusal names the server.
fn server_env(
    config: &Config,
    server: &str,
) -> (BTreeMap<String, String>, BTreeMap<String, String>) {
    let Some((Value::Object(merged), _)) = server_field(config, server, "env") else {
        return (BTreeMap::new(), BTreeMap::new());
    };
    let mut env = BTreeMap::new();
    let mut secrets = BTreeMap::new();
    for (key, value) in merged {
        if let Some(text) = value.as_str() {
            env.insert(key, text.to_owned());
        } else if let Some(name) = value.get("secret").and_then(Value::as_str) {
            secrets.insert(key, name.to_owned());
        }
    }
    (env, secrets)
}

/// The `mcp.servers."<name>".<field>` key, quoting the name as
/// [`server_field`] does.
pub fn mcp_server_key(name: &str, field: &str) -> String {
    format!("mcp.servers.{}.{field}", quoted_segment(name))
}

/// A dotted-path segment, quoted when it holds a dot.
pub fn quoted_segment(segment: &str) -> String {
    if segment.contains('.') {
        format!("\"{segment}\"")
    } else {
        segment.to_owned()
    }
}

/// What reading one repository script found: doctor checks its content
/// without running it.
#[derive(Debug)]
pub enum ScriptRead {
    /// A regular file, read to the end and discarded.
    Readable,
    /// A FIFO or other non-regular file: never read, so never waited on.
    NotChecked,
}

/// Reads one repository script to the end and discards it, in 64KiB
/// chunks, so memory stays bounded. The open is non-blocking and the
/// descriptor is proven regular first, so a swapped-in FIFO is never
/// waited on; symlinks are followed. A directory, or missing or
/// unreadable file, is an error with the same kind `fs::read` would give.
pub fn read_script(path: &Path) -> Result<ScriptRead, ConfigError> {
    let failed = |source| ConfigError::Io {
        file: path.to_owned(),
        source,
    };
    let file = match crate::credential_origin::open_validated(path, false) {
        Ok(file) => file,
        Err(crate::credential_origin::OpenFail::NotRegular) => return Ok(ScriptRead::NotChecked),
        Err(crate::credential_origin::OpenFail::IsDir) => {
            return Err(failed(crate::credential_origin::is_directory_error()));
        }
        Err(crate::credential_origin::OpenFail::NotFound(source))
        | Err(crate::credential_origin::OpenFail::Other(source)) => {
            return Err(failed(source));
        }
    };
    crate::credential_origin::seam::note_read(path);
    let mut file = file;
    let mut chunk = [0u8; 65536];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => return Ok(ScriptRead::Readable),
            Ok(_) => {}
            Err(source) => return Err(failed(source)),
        }
    }
}

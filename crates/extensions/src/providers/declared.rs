//! Admitting what an extension registers against its manifest's
//! `providers` map (`docs/extensions.md`, "What a package holds"): a
//! provider the map does not declare is not loaded, and a model whose
//! `base_url` the provider's entry does not list is left out.

use std::collections::BTreeMap;
use std::path::Path;

pub(crate) fn provider_scripts(dir: &Path) -> Result<Vec<String>, crate::Error> {
    let path = dir.join("providers");
    let entries = match std::fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(crate::Error::Io { path, source }),
    };
    let mut names = Vec::new();
    for entry in entries {
        let file = entry
            .map_err(|source| crate::Error::Io {
                path: path.clone(),
                source,
            })?
            .path();
        if file.is_file()
            && file.extension().is_some_and(|ext| ext == "lua")
            && let Some(stem) = file.file_stem()
        {
            names.push(stem.to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

use config::ModelData;
use contract::ErrorCode;
use contract::events::Notice;

/// The first name in `names`, in order, with no key in `declared`.
pub(crate) fn first_undeclared<'a>(
    declared: &BTreeMap<String, Vec<String>>,
    names: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    names.into_iter().find(|name| !declared.contains_key(*name))
}

/// Keeps the models whose `base_url` equals one `listed` string, byte for
/// byte: a `{name}` pattern admits its template before placeholders fill
/// it, never the filled URL. Gives one `extension_failed` notice per model
/// it drops, in model order.
pub(crate) fn leave_out_unlisted(
    provider: &str,
    extension: &str,
    listed: &[String],
    models: &mut Vec<ModelData>,
) -> Vec<Notice> {
    let mut notices = Vec::new();
    let mut kept = Vec::new();
    for model in models.drain(..) {
        if listed.iter().any(|url| url == &model.base_url) {
            kept.push(model);
        } else {
            notices.push(Notice {
                code: ErrorCode::ExtensionFailed,
                message: format!(
                    "The model `{provider}/{}` has the base URL `{}`, \
                     which `{extension}`'s manifest does not list for `{provider}`.",
                    model.id, model.base_url
                ),
                extension: Some(extension.to_owned()),
            });
        }
    }
    *models = kept;
    notices
}

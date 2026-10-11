//! Admitting what an extension registers against its manifest's
//! `providers` map (`docs/extensions.md`, "What a package holds"): a
//! provider the map does not declare is not loaded, and a model whose
//! `base_url` the provider's entry does not list is left out.

use std::collections::BTreeMap;

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

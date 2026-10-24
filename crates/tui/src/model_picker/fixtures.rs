//! Catalogue builders the picker's install tests share (`model_picker`,
//! `view::model_picker` and `app::model_picker` each test it).

use crate::catalogue::{Catalogue, ModelEntry};

/// The model `reference` ("provider/id") with `levels` and nothing else set.
pub(crate) fn entry(reference: &str, levels: &[&str]) -> ModelEntry {
    let (provider, id) = reference.split_once('/').unwrap();
    ModelEntry {
        reference: reference.to_owned(),
        provider: provider.to_owned(),
        id: id.to_owned(),
        levels: levels.iter().map(|level| (*level).to_owned()).collect(),
        default_level: None,
        configured: None,
        roles: Vec::new(),
        name: None,
        price: None,
    }
}

/// A catalogue of `models`, no levels, with `installable` not installed.
pub(crate) fn catalogue(models: &[&str], installable: &[&str]) -> Catalogue {
    Catalogue {
        models: models.iter().map(|model| entry(model, &[])).collect(),
        installable: installable.iter().map(|name| (*name).to_owned()).collect(),
        ..Catalogue::default()
    }
}

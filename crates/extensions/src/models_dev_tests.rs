use serde_json::json;

use super::{CatalogMeta, retained_rows};

#[test]
fn retained_rows_keeps_only_resolved_catalog_entries() {
    let catalog = json!({
        "provider": {
            "served": {"status": "active"},
            "fallback": {"status": "deprecated"},
            "irrelevant": {"status": "active"}
        },
        "other": {
            "unserved": {"status": "active"}
        }
    });
    let models: Vec<config::ModelData> = serde_json::from_value(json!([
        {"id": "served", "protocol": "openai-responses", "base_url": "https://example.test"},
        {"id": "fallback", "protocol": "openai-responses", "base_url": "https://example.test"}
    ]))
    .unwrap();
    let meta = CatalogMeta {
        source: None,
        pins: Default::default(),
        strip_prefix: None,
        split_source: false,
        sources: vec!["provider".into()],
        source_aliases: Default::default(),
    };

    assert_eq!(
        retained_rows(&catalog, &models, &meta),
        json!({
            "provider": {
                "served": {"status": "active"},
                "fallback": {"status": "deprecated"}
            }
        })
    );
}

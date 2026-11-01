use super::*;

#[test]
fn only_a_test_target_declaring_test_false_is_unrun() {
    let packages = vec![serde_json::json!({
        "name": "main",
        "version": "0.0.0",
        "manifest_path": "/ws/crates/main/Cargo.toml",
        "dependencies": [],
        "targets": [
            {"kind": ["lib"], "test": false, "src_path": "/ws/crates/main/src/lib.rs"},
            {"kind": ["example"], "test": false, "src_path": "/ws/crates/main/examples/e.rs"},
            {"kind": ["test"], "test": false, "src_path": "/ws/crates/main/tests/journeys.rs"},
            {"kind": ["test"], "test": true, "src_path": "/ws/crates/main/tests/normal.rs"},
            {"kind": ["test"], "src_path": "/ws/crates/main/tests/absent.rs"},
        ],
    })];
    let members = members_from("/ws", &packages);
    assert_eq!(
        members["main"].unrun_tests,
        vec!["crates/main/tests/journeys.rs".to_owned()]
    );
}

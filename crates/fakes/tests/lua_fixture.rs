//! The fixture Lua extension is where `lua_fixture` says.

#[test]
fn the_fixture_directory_holds_its_entry_script_and_manifest() {
    let dir = fakes::lua_fixture();
    assert!(dir.join("init.lua").is_file());
    assert!(dir.join("extension.json").is_file());
}

use super::*;

fn lua() -> Lua {
    let lua = Lua::new();
    install(&lua, PathBuf::from("/nonexistent-fiber-home")).unwrap();
    lua
}

fn eval(lua: &Lua, code: &str) -> Value {
    to_json(&lua.load(code).eval::<LuaValue>().unwrap()).unwrap()
}

#[test]
fn a_table_is_an_array_only_when_its_keys_are_one_to_its_size() {
    let lua = lua();
    assert_eq!(eval(&lua, "return {}"), serde_json::json!([]));
    assert_eq!(
        eval(&lua, "return {'a', 'b'}"),
        serde_json::json!(["a", "b"])
    );
    assert_eq!(
        eval(&lua, "return {[1] = 'a', [3] = 'c', x = 1}"),
        serde_json::json!({ "1": "a", "3": "c", "x": 1 })
    );
    assert_eq!(
        eval(&lua, "return {[2] = 'b', [1] = 'a'}"),
        serde_json::json!(["a", "b"])
    );
    assert_eq!(
        eval(&lua, "return {[0] = 'z'}"),
        serde_json::json!({ "0": "z" })
    );
    assert_eq!(
        eval(&lua, "return {n = 1.5, i = 2, t = true}"),
        serde_json::json!({ "n": 1.5, "i": 2, "t": true })
    );
}

#[test]
fn what_json_cannot_hold_is_an_error() {
    let lua = lua();
    for code in [
        "local t = {} t.t = t return t",
        "return { f = print }",
        "return 0/0",
        "return { [true] = 1 }",
    ] {
        let value = lua.load(code).eval::<LuaValue>();
        let value = value.unwrap_or(LuaValue::Function(
            lua.create_function(|_, ()| Ok(())).unwrap(),
        ));
        assert!(to_json(&value).is_err(), "{code}");
    }
    // A lightuserdata other than the null sentinel is not JSON null.
    assert!(
        to_json(&LuaValue::LightUserData(mlua::LightUserData(
            std::ptr::dangling_mut::<std::ffi::c_void>()
        )))
        .is_err()
    );
}

#[test]
fn json_keeps_nulls_and_the_shape_of_arrays_and_objects() {
    let lua = lua();
    for (text, encoded) in [
        ("[null]", "[null]"),
        (r#"{"a":null}"#, r#"{"a":null}"#),
        ("[]", "[]"),
        ("{}", "{}"),
        (
            r#"{"a":[null,{"b":null}],"c":[]}"#,
            r#"{"a":[null,{"b":null}],"c":[]}"#,
        ),
    ] {
        lua.globals().set("text", text).unwrap();
        let got: String = lua
            .load("return json.encode(json.decode(text))")
            .eval()
            .unwrap();
        assert_eq!(got, encoded, "{text}");
    }
}

#[test]
fn json_round_trips_through_lua() {
    let lua = lua();
    let value = serde_json::json!({ "a": [1, 2.5, "x", true], "b": { "c": "d" } });
    lua.globals()
        .set("v", to_lua(&lua, &value).unwrap())
        .unwrap();
    assert_eq!(eval(&lua, "return json.decode(json.encode(v))"), value);
    assert_eq!(eval(&lua, "return json.decode('null')"), Value::Null);
    assert!(lua.load("return json.decode('{')").exec().is_err());
}

#[test]
fn the_hashes_match_their_published_test_vectors() {
    let lua = lua();
    // FIPS 180-2's "abc".
    assert_eq!(
        eval(&lua, "return host.sha256('abc')"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    // RFC 4231, test case 2.
    let hex = "return (host.hmac_sha256('Jefe', 'what do ya want for nothing?'):gsub('.', \
               function(c) return string.format('%02x', c:byte()) end))";
    assert_eq!(
        eval(&lua, hex),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
}

#[test]
fn a_secret_that_is_not_stored_is_nil() {
    assert_eq!(eval(&lua(), "return host.secret('nope')"), Value::Null);
}

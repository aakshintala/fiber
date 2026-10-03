// Pass-3 execution scorer, rquickjs backend. Same contract as the mlua scorer:
// install stub host + fiber, load the model's extension, invoke the registered
// `acme` provider, replay the transcript through http_stream, check emitted text.
use rquickjs::{Context, Function, Runtime, Value};
use std::sync::{Arc, Mutex};

const GLUE: &str = r#"
globalThis.__providers = {};
globalThis.__tools = {};
globalThis.fiber = {
  provider: function(name, spec) { globalThis.__providers[name] = spec; },
  tool: function(name, spec) { globalThis.__tools[name] = spec; },
};
"#;

const DRIVER: &str = r#"
__providers.acme.chat({
  model: "acme-large",
  max_tokens: 1024,
  messages: [ { role: "user", content: "hello" } ],
});
"#;

fn main() {
    let path = std::env::args().nth(1).expect("usage: score-js <ext-file>");
    let src = std::fs::read_to_string(&path).expect("read ext");
    let sse = std::fs::read_to_string("../transcript.sse").expect("transcript.sse");
    let lines: Vec<String> = sse.lines().map(|s| s.to_string()).collect();
    let expected = std::fs::read_to_string("../expected.txt").expect("expected.txt");
    let name = std::path::Path::new(&path).file_name().unwrap().to_string_lossy().to_string();

    let rt = Runtime::new().unwrap();
    let ctx = Context::full(&rt).unwrap();
    let emitted = Arc::new(Mutex::new(String::new()));

    let (mut loaded, mut ran, mut err) = ("yes", "no", String::new());
    ctx.with(|c| {
        install_host(&c, emitted.clone(), lines);
        c.eval::<(), _>(GLUE).expect("glue");
        match c.eval::<Value, _>(src.as_bytes()) {
            Err(e) => { loaded = "no"; err = extract_err(&c, e); return; }
            Ok(_) => {}
        }
        match c.eval::<Value, _>(DRIVER.as_bytes()) {
            Err(e) => { err = extract_err(&c, e); }
            Ok(_) => { ran = "yes"; }
        }
    });

    let got = emitted.lock().unwrap().clone();
    let matched = if got.trim_end() == expected.trim_end() { "yes" } else { "no" };
    println!("RESULT score file={name} loaded={loaded} ran={ran} match={matched} \
              emit_len={} expected_len={} err={err}", got.len(), expected.trim_end().len());
    // debt: skip QuickJS Runtime teardown (asserts on a live host-stub ref);
    // the verdict is printed and this one-shot process is exiting anyway.
    // Tear the runtime down if this probe ever outlives one run.
    use std::io::Write;
    std::io::stdout().flush().ok();
    std::process::exit(0);
}

fn install_host<'js>(ctx: &rquickjs::Ctx<'js>, emitted: Arc<Mutex<String>>, lines: Vec<String>) {
    let g = ctx.globals();
    let host = rquickjs::Object::new(ctx.clone()).unwrap();
    host.set("secret", Function::new(ctx.clone(), |_n: String| "test-key".to_string()).unwrap()).unwrap();
    host.set("log", Function::new(ctx.clone(), |_m: Value| {}).unwrap()).unwrap();
    // non-streaming http stub: { status: 200, body: "{}" }
    let cx = ctx.clone();
    host.set("http", Function::new(ctx.clone(), move |_opts: Value| {
        let o = rquickjs::Object::new(cx.clone()).unwrap();
        o.set("status", 200).unwrap();
        o.set("body", "{}").unwrap();
        o
    }).unwrap()).unwrap();
    let em = emitted.clone();
    host.set("emit", Function::new(ctx.clone(), move |s: String| {
        em.lock().unwrap().push_str(&s);
    }).unwrap()).unwrap();
    // streaming http: replay the transcript through the extension's callback
    host.set("http_stream", Function::new(ctx.clone(), move |_opts: Value, on_line: Function| {
        for line in &lines {
            let _: () = on_line.call((line.clone(),)).expect("on_line");
        }
    }).unwrap()).unwrap();
    g.set("host", host).unwrap();
}

fn extract_err(ctx: &rquickjs::Ctx, e: rquickjs::Error) -> String {
    if let rquickjs::Error::Exception = e {
        let v = ctx.catch();
        let msg = v.as_exception().map(|x| format!("{x}"))
            .or_else(|| v.as_string().and_then(|s| s.to_string().ok()))
            .unwrap_or_else(|| format!("{v:?}"));
        first_line(&msg)
    } else {
        first_line(&e.to_string())
    }
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(120).collect()
}

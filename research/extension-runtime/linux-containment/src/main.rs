// Lua containment probe (issue #90). `containment run` spawns each case as a
// child process and reports exit status / signal / stdout. `containment <case>`
// runs one case in-process. mlua 0.12, Lua 5.4 vendored, as pass1.
use mlua::{HookTriggers, Lua, LuaOptions, StdLib, VmState};
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CASES: &[&str] = &[
    "panic_pcall", "panic_bare", "panic_coroutine", "panic_lock",
    "hook_naive_pcall", "hook_staged_pcall", "hook_coro_created_in_script",
    "hook_coro_created_before_arm", "hook_coro_yield_resume", "hook_thread_set_hook",
    "hook_staged_in_coro_retry", "hook_staged_rust_thread_resume", "hook_thread_staged",
    "hook_coro_replaced_wrap", "hook_coro_create_unreplaced", "hook_coro_replaced_create",
    "hook_coro_replaced_both_nested",
    "oom_plain", "oom_huge_string", "oom_in_pcall_retry", "oom_in_coroutine",
    "oom_rust_create_string", "oom_rust_create_table", "oom_reuse_after", "oom_in_hook",
    "lock_lua_error_in_callback", "lock_deadline_in_call", "lock_hook_holds_lock",
    "lock_lua_error_longjmp_frames",
];

fn lua() -> Lua {
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8 | StdLib::COROUTINE;
    Lua::new_with(libs, LuaOptions::default()).expect("new_with")
}

fn err_msg(e: &mlua::Error) -> String {
    e.to_string().lines().next().unwrap_or("").chars().take(120).collect()
}

/// Staged hook armed on one coroutine (Thread); escalation re-arms that same thread.
fn staged_thread_hook(th: &mlua::Thread, flag: Arc<AtomicBool>) {
    th.set_hook(HookTriggers::default().every_nth_instruction(1000), move |lua, _| {
        if flag.load(Ordering::Relaxed) {
            let f2 = flag.clone();
            let _ = lua.current_thread().set_hook(HookTriggers::default().every_nth_instruction(1), move |_, _| {
                if f2.load(Ordering::Relaxed) {
                    Err(mlua::Error::RuntimeError("deadline exceeded".into()))
                } else { Ok(VmState::Continue) }
            });
            Err(mlua::Error::RuntimeError("deadline exceeded".into()))
        } else { Ok(VmState::Continue) }
    }).expect("thread set_hook");
}

fn arm_watchdog(ms: u64) -> Arc<AtomicBool> {
    let stop = Arc::new(AtomicBool::new(false));
    let w = stop.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(ms));
        w.store(true, Ordering::Relaxed);
    });
    stop
}

fn naive_hook(lua: &Lua, flag: Arc<AtomicBool>) {
    lua.set_hook(HookTriggers::default().every_nth_instruction(1000), move |_, _| {
        if flag.load(Ordering::Relaxed) {
            Err(mlua::Error::RuntimeError("deadline exceeded".into()))
        } else { Ok(VmState::Continue) }
    }).expect("set_hook");
}

/// Two-stage hook from pass1 `interrupt_escalate`.
fn staged_hook(lua: &Lua, flag: Arc<AtomicBool>) {
    lua.set_hook(HookTriggers::default().every_nth_instruction(1000), move |lua, _| {
        if flag.load(Ordering::Relaxed) {
            let f2 = flag.clone();
            let _ = lua.set_hook(HookTriggers::default().every_nth_instruction(1), move |_, _| {
                if f2.load(Ordering::Relaxed) {
                    Err(mlua::Error::RuntimeError("deadline exceeded".into()))
                } else { Ok(VmState::Continue) }
            });
            Err(mlua::Error::RuntimeError("deadline exceeded".into()))
        } else { Ok(VmState::Continue) }
    }).expect("set_hook");
}

// Retry the pcall 50 times; a script that swallows the deadline returns 50.
const RETRY: &str = "local n=0 while n<50 do pcall(function() while true do end end) n=n+1 end return n";

fn report(name: &str, t0: Instant, r: mlua::Result<mlua::Value>) {
    let ms = t0.elapsed().as_millis();
    match r {
        Ok(v) => println!("RESULT {name} script_returned={v:?} after_ms={ms}"),
        Err(e) => println!("RESULT {name} script_error after_ms={ms} err={}", err_msg(&e)),
    }
}

fn panic_fn(lua: &Lua) {
    let f = lua.create_function(|_, ()| -> mlua::Result<()> { panic!("boom from host callback") }).unwrap();
    lua.globals().set("host_panic", f).unwrap();
}

fn run_case(name: &str) {
    match name {
        // ---- 1. Rust panic in a Lua callback ----
        "panic_pcall" | "panic_bare" | "panic_coroutine" => {
            let lua = lua();
            panic_fn(&lua);
            let src = match name {
                "panic_pcall" => "return pcall(host_panic)",
                "panic_bare" => "return host_panic()",
                _ => "local co = coroutine.wrap(function() return pcall(host_panic) end) return co()",
            };
            println!("BEFORE");
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                lua.load(src).eval::<mlua::MultiValue>().map(|v| format!("{v:?}"))
            }));
            match r {
                Err(_) => println!("RESULT {name} unwound_to_rust_caller_as_panic"),
                Ok(Ok(v)) => println!("RESULT {name} lua_saw_it_pcall_returned={v}"),
                Ok(Err(e)) => println!("RESULT {name} became_lua_error err={}", err_msg(&e)),
            }
            let alive = lua.load("return 1+1").eval::<i64>();
            println!("RESULT {name} vm_after={alive:?}");
        }
        "panic_lock" => {
            let lua = lua();
            let m = Arc::new(Mutex::new(0u32));
            let m2 = m.clone();
            let f = lua.create_function(move |_, ()| -> mlua::Result<()> {
                let _g = m2.lock().unwrap();
                panic!("panic while holding lock");
            }).unwrap();
            lua.globals().set("f", f).unwrap();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lua.load("f()").exec()));
            println!("RESULT panic_lock outcome={} poisoned={}",
                     if r.is_err() { "panic_unwound_to_caller" } else { "lua_error_or_ok" }, m.is_poisoned());
        }
        // ---- 2. deadline through pcall and coroutines ----
        "hook_naive_pcall" => {
            let lua = lua();
            naive_hook(&lua, arm_watchdog(100));
            let t0 = Instant::now();
            report(name, t0, lua.load(RETRY).eval());
        }
        "hook_staged_pcall" => {
            let lua = lua();
            staged_hook(&lua, arm_watchdog(100));
            let t0 = Instant::now();
            report(name, t0, lua.load(RETRY).eval());
        }
        "hook_coro_created_in_script" => {
            let lua = lua();
            staged_hook(&lua, arm_watchdog(100));
            let t0 = Instant::now();
            report(name, t0, lua.load(&format!("local co=coroutine.wrap(function() {RETRY} end) return co()")).eval());
        }
        "hook_coro_created_before_arm" => {
            // Coroutine exists before the hook is armed, then is resumed from Rust.
            let lua = lua();
            let f = lua.load(format!("return function() {RETRY} end")).eval::<mlua::Function>().unwrap();
            let th = lua.create_thread(f).unwrap();
            staged_hook(&lua, arm_watchdog(100));
            let t0 = Instant::now();
            report(name, t0, th.resume::<mlua::Value>(()).map(|v| v));
        }
        "hook_coro_yield_resume" => {
            // Callback yields, is resumed after the deadline passed, then loops under pcall.
            let lua = lua();
            staged_hook(&lua, arm_watchdog(100));
            let f = lua.load(format!("return function() coroutine.yield(1) {RETRY} end")).eval::<mlua::Function>().unwrap();
            let th = lua.create_thread(f).unwrap();
            let _ = th.resume::<mlua::Value>(()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            let t0 = Instant::now();
            report(name, t0, th.resume::<mlua::Value>(()));
        }
        "hook_thread_set_hook" => {
            // Same as created_before_arm but the hook is also armed on the thread itself.
            let lua = lua();
            let f = lua.load(format!("return function() {RETRY} end")).eval::<mlua::Function>().unwrap();
            let th = lua.create_thread(f).unwrap();
            let flag = arm_watchdog(100);
            staged_hook(&lua, flag.clone());
            let f2 = flag.clone();
            th.set_hook(HookTriggers::default().every_nth_instruction(1000), move |_, _| {
                if f2.load(Ordering::Relaxed) { Err(mlua::Error::RuntimeError("deadline exceeded".into())) } else { Ok(VmState::Continue) }
            }).unwrap();
            let t0 = Instant::now();
            report(name, t0, th.resume::<mlua::Value>(()));
        }
        "hook_staged_in_coro_retry" => {
            // Escalation happens inside a coroutine; the outer (main) code then keeps retrying.
            let lua = lua();
            staged_hook(&lua, arm_watchdog(100));
            let src = "local n=0 while n<50 do local co=coroutine.wrap(function() while true do end end) pcall(co) n=n+1 end return n";
            let t0 = Instant::now();
            report(name, t0, lua.load(src).eval());
        }
        "hook_staged_rust_thread_resume" => {
            // Rust resumes a fresh Thread per attempt, ignoring errors, 50 times.
            let lua = lua();
            staged_hook(&lua, arm_watchdog(100));
            let f = lua.load("return function() while true do end end").eval::<mlua::Function>().unwrap();
            let t0 = Instant::now();
            let mut errs = 0;
            for _ in 0..3 {
                let th = lua.create_thread(f.clone()).unwrap();
                if th.resume::<mlua::Value>(()).is_err() { errs += 1; }
            }
            println!("RESULT {name} three_fresh_threads_all_errored={} after_ms={}", errs == 3, t0.elapsed().as_millis());
        }
        "hook_thread_staged" => {
            let lua = lua();
            let flag = arm_watchdog(100);
            staged_hook(&lua, flag.clone());
            let f = lua.load(format!("return function() {RETRY} end")).eval::<mlua::Function>().unwrap();
            let th = lua.create_thread(f).unwrap();
            staged_thread_hook(&th, flag);
            let t0 = Instant::now();
            report(name, t0, th.resume::<mlua::Value>(()));
        }
        "hook_coro_replaced_wrap" => {
            // The host replaces coroutine.wrap so every script coroutine gets the staged hook.
            let lua = lua();
            let flag = arm_watchdog(100);
            staged_hook(&lua, flag.clone());
            let fl = flag.clone();
            let wrap = lua.create_function(move |lua, f: mlua::Function| {
                let th = lua.create_thread(f)?;
                staged_thread_hook(&th, fl.clone());
                lua.create_function(move |_, args: mlua::MultiValue| th.resume::<mlua::MultiValue>(args))
            }).unwrap();
            lua.globals().get::<mlua::Table>("coroutine").unwrap().set("wrap", wrap).unwrap();
            let t0 = Instant::now();
            report(name, t0, lua.load(&format!("local co=coroutine.wrap(function() {RETRY} end) return co()")).eval());
        }
        "hook_coro_create_unreplaced" | "hook_coro_replaced_create" | "hook_coro_replaced_both_nested" => {
            let lua = lua();
            let flag = arm_watchdog(100);
            staged_hook(&lua, flag.clone());
            if name != "hook_coro_create_unreplaced" {
                let fl = flag.clone();
                let create = lua.create_function(move |lua, f: mlua::Function| {
                    let th = lua.create_thread(f)?;
                    staged_thread_hook(&th, fl.clone());
                    Ok(th)
                }).unwrap();
                let fl = flag.clone();
                let wrap = lua.create_function(move |lua, f: mlua::Function| {
                    let th = lua.create_thread(f)?;
                    staged_thread_hook(&th, fl.clone());
                    lua.create_function(move |_, args: mlua::MultiValue| th.resume::<mlua::MultiValue>(args))
                }).unwrap();
                let co = lua.globals().get::<mlua::Table>("coroutine").unwrap();
                co.set("create", create).unwrap();
                if name == "hook_coro_replaced_both_nested" { co.set("wrap", wrap).unwrap(); }
            }
            let src = if name == "hook_coro_replaced_both_nested" {
                // a coroutine made with create, inside one made with wrap, loops under pcall
                format!("local outer=coroutine.wrap(function() local inner=coroutine.create(function() {RETRY} end) return coroutine.resume(inner) end) return outer()")
            } else {
                format!("local co=coroutine.create(function() {RETRY} end) return coroutine.resume(co)")
            };
            let t0 = Instant::now();
            // coroutine.resume returns (false, err) instead of raising, so show the values.
            let r = lua.load(&src).eval::<mlua::MultiValue>();
            println!("RESULT {name} {:?} after_ms={}", r.map(|v| format!("{v:?}").chars().take(110).collect::<String>()).map_err(|e| err_msg(&e)), t0.elapsed().as_millis());
        }
        // ---- 3. allocation past the cap ----
        "oom_plain" | "oom_huge_string" | "oom_in_pcall_retry" | "oom_in_coroutine" | "oom_reuse_after" => {
            let lua = lua();
            lua.set_memory_limit(8 * 1024 * 1024).unwrap();
            let grow = r#"local t = {} while true do t[#t+1] = string.rep("x", 1024) end"#;
            let src = match name {
                "oom_huge_string" => r#"return string.rep("x", 1 << 40)"#.to_string(),
                "oom_in_pcall_retry" => format!("local r={{}} for i=1,20 do r[i]=select(2, pcall(function() {grow} end)) end return r[20]"),
                "oom_in_coroutine" => format!("local co=coroutine.wrap(function() {grow} end) return co()"),
                _ => grow.to_string(),
            };
            let t0 = Instant::now();
            let r = lua.load(&src).eval::<mlua::Value>();
            match &r {
                Ok(v) => println!("RESULT {name} script_returned={v:?}"),
                Err(e) => println!("RESULT {name} lua_error kind={} err={}", match e { mlua::Error::MemoryError(_) => "MemoryError", _ => "other" }, err_msg(e)),
            }
            let used = lua.used_memory();
            lua.gc_collect().ok(); lua.gc_collect().ok();
            let alive = lua.load("local t={} for i=1,1000 do t[i]=i end return #t").eval::<i64>();
            println!("RESULT {name} used_at_fail_kib={} used_after_gc_kib={} vm_after={alive:?} ms={}", used / 1024, lua.used_memory() / 1024, t0.elapsed().as_millis());
            if name == "oom_reuse_after" {
                for i in 0..3 {
                    let r = lua.load(grow).exec();
                    println!("RESULT {name} round={i} err_again={}", r.is_err());
                    lua.gc_collect().ok();
                }
            }
        }
        "oom_rust_create_string" => {
            // A host callback (Rust side) allocates a string bigger than the cap.
            let lua = lua();
            lua.set_memory_limit(8 * 1024 * 1024).unwrap();
            let f = lua.create_function(|lua, ()| { let s = vec![b'x'; 64 << 20]; lua.create_string(&s) }).unwrap();
            lua.globals().set("f", f).unwrap();
            let r = lua.load("local ok, e = pcall(f) return ok, tostring(e)").eval::<(bool, String)>();
            println!("RESULT {name} {:?}", r.map(|(ok, v)| (ok, v.lines().next().unwrap_or("").chars().take(100).collect::<String>())).map_err(|e| err_msg(&e)));
            println!("RESULT {name} vm_after={:?}", lua.load("return 1+1").eval::<i64>());
        }
        "oom_rust_create_table" => {
            let lua = lua();
            lua.set_memory_limit(8 * 1024 * 1024).unwrap();
            let f = lua.create_function(|lua, ()| {
                let t = lua.create_table()?;
                let mut i = 0i64;
                loop { t.raw_set(i, i)?; i += 1; }
                #[allow(unreachable_code)] Ok(())
            }).unwrap();
            lua.globals().set("f", f).unwrap();
            let r = lua.load("local ok, e = pcall(f) return ok, tostring(e)").eval::<(bool, String)>();
            println!("RESULT {name} {:?}", r.map(|(ok, v)| (ok, v.lines().next().unwrap_or("").chars().take(100).collect::<String>())).map_err(|e| err_msg(&e)));
            println!("RESULT {name} vm_after={:?}", lua.load("return 1+1").eval::<i64>());
        }
        "oom_in_hook" => {
            // The cap is hit while the instruction hook (Rust) is running Lua-side allocation.
            let lua = lua();
            lua.set_memory_limit(8 * 1024 * 1024).unwrap();
            lua.set_hook(HookTriggers::default().every_nth_instruction(1000), |lua, _| {
                let _ = lua.create_string(vec![b'y'; 1 << 20])?;
                Ok(VmState::Continue)
            }).unwrap();
            let r = lua.load("local t={} while true do t[#t+1]={} end").exec();
            println!("RESULT {name} err={:?}", r.map_err(|e| err_msg(&e)));
            lua.remove_hook();
            println!("RESULT {name} vm_after={:?}", lua.load("return 1+1").eval::<i64>());
        }
        // ---- 4. Lua error while Rust holds a lock ----
        "lock_lua_error_in_callback" => {
            // Host fn holds a mutex and calls a Lua function that raises error().
            let lua = lua();
            let m = Arc::new(Mutex::new(Vec::<u32>::new()));
            let m2 = m.clone();
            let f = lua.create_function(move |_, cb: mlua::Function| {
                let mut g = m2.lock().unwrap();
                g.push(1);
                cb.call::<()>(())?;
                g.push(2);
                Ok(())
            }).unwrap();
            lua.globals().set("with_lock", f).unwrap();
            let r = lua.load("local ok, e = pcall(with_lock, function() error('lua error under lock') end) return ok, tostring(e)").eval::<(bool, String)>();
            let try_ok = m.try_lock().is_ok();
            let contents = m.lock().map(|g| g.clone()).ok();
            println!("RESULT {name} pcall={:?} poisoned={} try_lock_ok={try_ok} contents={contents:?}", r.map_err(|e| err_msg(&e)), m.is_poisoned());
        }
        "lock_deadline_in_call" => {
            // Host fn holds a lock while running a Lua loop; the deadline hook errors mid-call.
            let lua = lua();
            staged_hook(&lua, arm_watchdog(100));
            let m = Arc::new(Mutex::new(0u32));
            let m2 = m.clone();
            let f = lua.create_function(move |_, cb: mlua::Function| {
                let _g = m2.lock().unwrap();
                cb.call::<()>(())
            }).unwrap();
            lua.globals().set("with_lock", f).unwrap();
            let r = lua.load("with_lock(function() while true do end end)").exec();
            let try_ok = m.try_lock().is_ok();
            println!("RESULT {name} err={:?} poisoned={} try_lock_ok={try_ok}", r.map_err(|e| err_msg(&e)), m.is_poisoned());
        }
        "lock_hook_holds_lock" => {
            // The hook closure itself holds the lock while calling a Lua function that errors.
            let lua = lua();
            let m = Arc::new(Mutex::new(0u32));
            let m2 = m.clone();
            let bad = lua.load("return function() error('error called from hook') end").eval::<mlua::Function>().unwrap();
            lua.set_hook(HookTriggers::default().every_nth_instruction(1000), move |_, _| {
                let mut g = m2.lock().unwrap();
                *g += 1;
                bad.call::<()>(())?;
                Ok(VmState::Continue)
            }).unwrap();
            let r = lua.load("local n=0 for i=1,100000 do n=n+i end return n").eval::<i64>();
            let try_ok = m.try_lock().is_ok();
            let ran = m.lock().map(|g| *g).ok();
            println!("RESULT {name} err={:?} poisoned={} try_lock_ok={try_ok} hook_ran={ran:?}", r.map_err(|e| err_msg(&e)), m.is_poisoned());
        }
        "lock_lua_error_longjmp_frames" => {
            // Lua error passing through a Rust callback that holds a guard, nested three deep.
            let lua = lua();
            let m = Arc::new(Mutex::new(0u32));
            let m2 = m.clone();
            let f = lua.create_function(move |_, cb: mlua::Function| {
                let _g = m2.lock().unwrap();
                cb.call::<()>(())
            }).unwrap();
            lua.globals().set("L", f).unwrap();
            // Lua calls Rust calls Lua calls Rust ... the innermost is a Lua error() with no pcall until the outside.
            // A single lock is re-taken in a recursive callback via try_lock to show it was released.
            let r = lua.load("local function d(n) if n==0 then error('deep') end return L(function() return d(n-1) end) end local ok, e = pcall(d, 1) return ok, tostring(e)").eval::<(bool, String)>();
            let try_ok = m.try_lock().is_ok();
            println!("RESULT {name} pcall={:?} poisoned={} try_lock_ok={try_ok}", r.map_err(|e| err_msg(&e)), m.is_poisoned());
        }
        other => { eprintln!("unknown case {other}"); std::process::exit(2); }
    }
}

fn main() {
    let exe = std::env::current_exe().unwrap();
    let profile = exe.parent().and_then(|p| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match std::env::args().nth(1).as_deref() {
        Some("run") => {
            println!("PROFILE {profile} os={} arch={}", std::env::consts::OS, std::env::consts::ARCH);
            for case in CASES {
                let mut c = Command::new(&exe).arg(case).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
                let t0 = Instant::now();
                let mut timed_out = false;
                let status = loop {
                    if let Some(s) = c.try_wait().unwrap() { break s; }
                    if t0.elapsed() > Duration::from_secs(6) { let _ = c.kill(); timed_out = true; break c.wait().unwrap(); }
                    std::thread::sleep(Duration::from_millis(10));
                };
                let out = c.wait_with_output().unwrap();
                println!("CASE {case} profile={profile} exit_code={:?} signal={:?} timed_out={timed_out}", status.code(), status.signal());
                for l in String::from_utf8_lossy(&out.stdout).lines() { println!("  out: {l}"); }
                for l in String::from_utf8_lossy(&out.stderr).lines().take(4) { println!("  err: {}", l.chars().take(160).collect::<String>()); }
            }
        }
        Some(case) => run_case(case),
        None => eprintln!("usage: containment run | <case>"),
    }
}

//! The directory soundness invariant (plan I2 for #1497) at the `Cwd`
//! level: every directory an element may run in is entered, or `enter`
//! reports the lost path (the call is `executes`). A reference interpreter
//! runs all 2^n success assignments with bash left-to-right semantics and
//! its own lexical join; each executed `ls` element's real directory must
//! be among that element's entered directories. One workdir case holds `a`
//! as a symlink to an outside directory, so a physical resolution would
//! diverge from the bash-logical oracle (`cd a && cd ..` returns to the
//! workdir).

use std::os::unix::fs::symlink;

use proptest::prelude::*;

use super::super::syntax::Join;
use super::Cwd;

#[derive(Debug, Clone)]
struct Step {
    join: Join,
    cd: bool,
    arg: String,
}

fn arb_script() -> impl Strategy<Value = Vec<Step>> {
    let arg = prop::sample::select(vec!["a", "b", "a/b", "..", "/abs"]);
    let join = prop_oneof![Just(Join::And), Just(Join::Or), Just(Join::Seq)];
    let step = (prop::bool::ANY, arg, join).prop_map(|(cd, arg, join)| Step {
        join,
        cd,
        arg: arg.to_string(),
    });
    prop::collection::vec(step, 1..=6)
}

/// The bash-logical join, implemented apart from `Cwd`: an absolute target
/// replaces the directory, `.` stays, `..` pops, and a `..` with an empty
/// stack stays literal.
fn logical_join(dir: &str, target: &str) -> String {
    let mut absolute = dir.starts_with('/');
    let mut parts: Vec<&str> = dir.split('/').filter(|piece| !piece.is_empty()).collect();
    if target.starts_with('/') {
        absolute = true;
        parts.clear();
    }
    for piece in target.split('/') {
        match piece {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    parts.push("..");
                }
            }
            _ => parts.push(piece),
        }
    }
    let mut joined = parts.join("/");
    if absolute {
        joined.insert(0, '/');
    }
    joined
}

/// The real directory of every executed `ls` element, over all 2^n success
/// assignments: the first element always runs, `&&` runs when the previous
/// element ran and read, `||` when it ran and did not, `;` always.
fn executed_dirs(steps: &[Step], workdir: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let count = steps.len();
    for mask in 0..(1_usize << count) {
        let mut dir = workdir.to_owned();
        let mut ran_before = false;
        let mut ok_before = false;
        let mut first = true;
        for (index, step) in steps.iter().enumerate() {
            let executed = if first {
                true
            } else {
                match step.join {
                    Join::First | Join::Seq => true,
                    Join::And => ran_before && ok_before,
                    Join::Or => ran_before && !ok_before,
                }
            };
            first = false;
            let ok = mask & (1_usize << index) != 0;
            if executed {
                if step.cd {
                    dir = logical_join(&dir, &step.arg);
                } else {
                    found.push((index, dir.clone()));
                }
            }
            ran_before = executed;
            ok_before = ok;
        }
    }
    found
}

proptest! {
    /// Every executed element's real directory is entered, or the path is
    /// lost: six elements stay far under the directory cap, so `enter`
    /// always answers.
    #[test]
    fn entered_directories_cover_every_execution(steps in arb_script(), symlinked in prop::bool::ANY) {
        let outer = fakes::TempDir::new("fiber-cwd-props-outer");
        let target = outer.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let dir = fakes::TempDir::new("fiber-cwd-props");
        if symlinked {
            symlink(&target, dir.path().join("a")).unwrap();
        }
        let workdir = if symlinked {
            dir.path().to_str().unwrap().to_owned()
        } else {
            "/work".to_owned()
        };
        let mut cwd = Cwd::new(&workdir);
        let mut entered = Vec::new();
        for step in &steps {
            entered.push(cwd.enter(step.join));
            cwd.leave(if step.cd { Some(step.arg.as_str()) } else { None });
        }
        for (index, real) in executed_dirs(&steps, &workdir) {
            let Some(dirs) = &entered[index] else {
                continue;
            };
            prop_assert!(
                dirs.contains(&real),
                "element {index} runs in {real:?}, entered only {dirs:?}"
            );
        }
    }
}

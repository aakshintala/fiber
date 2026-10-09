//! Prototype cases. A surface declares its own `CASES` slice next to the code
//! that draws it; `--help`, the unknown-case error and `check/<surface>.md`
//! are all built from those slices.

/// One case of a surface: `--<flag> <name>` draws what `build` returns.
pub(crate) struct Case<T: 'static> {
    pub name: &'static str,
    /// one line for `--help`
    pub help: &'static str,
    /// what to look for when checking it in Ghostty; read by the `check/` test
    #[allow(dead_code)]
    pub check: &'static str,
    pub build: fn() -> T,
}

/// A case's text, without its builder.
pub(crate) struct Doc {
    pub name: &'static str,
    pub help: &'static str,
    #[allow(dead_code)]
    pub check: &'static str,
}

/// A surface and the flag that selects one of its cases.
pub(crate) struct Surface {
    pub flag: &'static str,
    /// the file under `check/`, without `.md`; read by the `check/` test
    #[allow(dead_code)]
    pub file: &'static str,
    #[allow(dead_code)]
    pub title: &'static str,
    pub docs: fn() -> Vec<Doc>,
}

pub(crate) fn docs<T>(cases: &[Case<T>]) -> Vec<Doc> {
    cases.iter().map(|c| Doc { name: c.name, help: c.help, check: c.check }).collect()
}

/// The case called `name`, built.
pub(crate) fn lookup<T>(cases: &[Case<T>], name: &str) -> Option<T> {
    cases.iter().find(|c| c.name == name).map(|c| (c.build)())
}

pub(crate) fn names<T>(cases: &[Case<T>]) -> String {
    cases.iter().map(|c| c.name).collect::<Vec<_>>().join(", ")
}

/// The cases section of `--help`.
pub(crate) fn help(surfaces: &[Surface]) -> String {
    let mut out = String::new();
    for s in surfaces {
        out.push_str(&format!("\n{} CASE:\n", s.flag));
        for c in (s.docs)() {
            out.push_str(&format!("  {:<16} {}\n", c.name, c.help));
        }
    }
    out
}

/// The body of `check/<surface>.md`.
#[cfg(test)]
pub(crate) fn check_md(s: &Surface) -> String {
    let mut out = format!("# {}\n\nOne run per case: `{} CASE`.\n\n", s.title, s.flag);
    for c in (s.docs)() {
        out.push_str(&format!("- {}: {}\n", c.name, c.check));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: &[Case<u8>] = &[Case { name: "a", help: "first", check: "see a", build: || 7 }];
    const DEMO: Surface = Surface { flag: "--demo", file: "demo", title: "Demo", docs: || docs(CASES) };

    #[test]
    fn help_check_and_lookup_come_from_the_slice() {
        assert_eq!(names(CASES), "a");
        assert_eq!(lookup(CASES, "a"), Some(7));
        assert_eq!(lookup(CASES, "b"), None);
        assert_eq!(help(&[DEMO]), "\n--demo CASE:\n  a                first\n");
        assert_eq!(check_md(&DEMO), "# Demo\n\nOne run per case: `--demo CASE`.\n\n- a: see a\n");
    }
}

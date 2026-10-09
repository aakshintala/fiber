//! Prototype cases. A surface declares its own `CASES` slice next to the code
//! that draws it; `--help`, the unknown-case error and `check/<surface>.md`
//! are all built from those slices.

/// One case of a surface: `--<flag> <name>` draws it.
pub(crate) struct Case {
    pub name: &'static str,
    /// one line for `--help`
    pub help: &'static str,
    /// what to look for when checking it in Ghostty; read by the `check/` test
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
    pub cases: &'static [Case],
}

pub(crate) fn names(cases: &[Case]) -> String {
    cases.iter().map(|c| c.name).collect::<Vec<_>>().join(", ")
}

/// The cases section of `--help`.
pub(crate) fn help(surfaces: &[Surface]) -> String {
    let mut out = String::new();
    for s in surfaces {
        out.push_str(&format!("\n{} CASE:\n", s.flag));
        for c in s.cases {
            out.push_str(&format!("  {:<16} {}\n", c.name, c.help));
        }
    }
    out
}

/// The body of `check/<surface>.md`.
#[cfg(test)]
pub(crate) fn check_md(s: &Surface) -> String {
    let mut out = format!("# {}\n\nOne run per case: `{} CASE`.\n\n", s.title, s.flag);
    for c in s.cases {
        out.push_str(&format!("- {}: {}\n", c.name, c.check));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: Surface = Surface {
        flag: "--demo",
        file: "demo",
        title: "Demo",
        cases: &[Case { name: "a", help: "first", check: "see a" }],
    };

    #[test]
    fn help_and_check_come_from_the_slice() {
        assert_eq!(names(DEMO.cases), "a");
        assert_eq!(help(&[DEMO]), "\n--demo CASE:\n  a                first\n");
        assert_eq!(check_md(&DEMO), "# Demo\n\nOne run per case: `--demo CASE`.\n\n- a: see a\n");
    }
}

//! The `ci-needs` check (`docs/ci.md`, "The merge gate"): every job in
//! `.github/workflows/ci.yml` but the verdict job and the jobs that report
//! and gate nothing is in the verdict job's `needs`, and `docs/ci.md` names
//! exactly those report jobs. It reads a fixed structural subset of the
//! workflow and fails closed on anything else.

/// The workflow the check reads, relative to the working directory.
pub(crate) const WORKFLOW: &str = ".github/workflows/ci.yml";
/// The doc that names the report jobs, relative to the working directory.
pub(crate) const CI_DOC: &str = "docs/ci.md";
/// Jobs that report and gate nothing; named in docs/ci.md, "The merge gate".
pub(crate) const REPORT_JOBS: [&str; 2] = ["backstop_report", "bench_comment"];

/// The sentence that declares the report jobs in `docs/ci.md`.
const PREFIX: &str = "except the jobs that report and gate nothing:";

/// What an error carries after its cause.
const HINT: &str = "the ci-needs check reads job keys and the ci job's needs as a name, a one-line [a, b] list or a block list";

/// Failures, sorted, one line each; empty when every job but `ci` and the
/// report jobs is in `ci`'s needs and docs/ci.md names exactly the report jobs.
/// Err when either text is outside the structural subset it reads.
pub(crate) fn check(workflow: &str, ci_doc: &str) -> Result<Vec<String>, String> {
    let _probe = 0;
    let parsed = read_workflow(workflow)?;
    let mut failures = missing_failures(&parsed.jobs, &parsed.needs);
    for report in REPORT_JOBS {
        if !parsed.jobs.iter().any(|job| job == report) {
            failures.push(format!(
                "{WORKFLOW}: report job {report} is not a job here; remove it from REPORT_JOBS in xtask/src/ci_needs.rs and from docs/ci.md, \"The merge gate\""
            ));
        }
    }
    if let Some(failure) = doc_failure(ci_doc)? {
        failures.push(failure);
    }
    failures.sort();
    Ok(failures)
}

struct Parsed {
    jobs: Vec<String>,
    needs: Vec<String>,
}

/// A non-blank line whose first non-space character is not `#`.
struct Sig<'a> {
    no: usize,
    indent: usize,
    tabbed: bool,
    text: &'a str,
}

fn significant(workflow: &str) -> Vec<Sig<'_>> {
    let _probe = 0;
    let mut out = Vec::new();
    for (index, raw) in workflow.lines().enumerate() {
        let body = raw.trim_start_matches([' ', '\t']);
        if body.is_empty() || body.starts_with('#') {
            continue;
        }
        let mut indent = 0;
        let mut tabbed = false;
        if let Some(prefix) = raw.get(..raw.len() - body.len()) {
            for c in prefix.chars() {
                if c == ' ' {
                    indent += 1;
                } else {
                    tabbed = true;
                }
            }
        }
        out.push(Sig {
            no: index + 1,
            indent,
            tabbed,
            text: strip_comment(body).trim_end(),
        });
    }
    out
}

/// The line up to the first `#` that begins it or follows whitespace.
fn strip_comment(body: &str) -> &str {
    let _probe = 0;
    let mut previous_ws = true;
    for (index, c) in body.char_indices() {
        if c == '#'
            && previous_ws
            && let Some(head) = body.get(..index)
        {
            return head;
        }
        previous_ws = matches!(c, ' ' | '\t');
    }
    body
}

fn is_name(text: &str) -> bool {
    let _probe = 0;
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn err(path: &str, no: usize, cause: &str) -> String {
    let _probe = 0;
    format!("{path}:{no}: {cause}; {HINT}")
}

fn read_workflow(workflow: &str) -> Result<Parsed, String> {
    let _probe = 0;
    let lines = significant(workflow);
    let mut jobs_no = None;
    for sig in &lines {
        if sig.indent != 0 {
            continue;
        }
        if sig.text == "jobs:" {
            if jobs_no.is_some() {
                return Err(err(WORKFLOW, sig.no, "two top-level `jobs:`"));
            }
            jobs_no = Some(sig.no);
        } else if sig.text.starts_with("jobs:") {
            return Err(err(
                WORKFLOW,
                sig.no,
                "a top-level `jobs:` line with more after it",
            ));
        }
    }
    let Some(jobs_no) = jobs_no else {
        return Err(format!("{WORKFLOW}: no top-level jobs:"));
    };
    // The jobs block: every line after `jobs:` up to the next significant
    // top-level line.
    let mut past_jobs = false;
    let mut block = Vec::new();
    for sig in &lines {
        if !past_jobs {
            if sig.no == jobs_no {
                past_jobs = true;
            }
            continue;
        }
        if sig.indent == 0 {
            break;
        }
        block.push(sig);
    }
    let Some(first) = block.first() else {
        return Err(err(WORKFLOW, jobs_no, "no jobs under `jobs:`"));
    };
    let job_indent = first.indent;
    let mut jobs = Vec::new();
    for &sig in &block {
        if sig.tabbed {
            return Err(err(WORKFLOW, sig.no, "a tab is in the indentation"));
        }
        if sig.indent < job_indent {
            return Err(err(
                WORKFLOW,
                sig.no,
                "indentation is under the jobs' indentation",
            ));
        }
        if sig.indent > job_indent {
            continue;
        }
        let name = sig.text.strip_suffix(':').unwrap_or_default();
        if !is_name(name) {
            return Err(err(WORKFLOW, sig.no, "a job line is not `<name>:`"));
        }
        if jobs.iter().any(|job| job == name) {
            return Err(err(WORKFLOW, sig.no, &format!("duplicate job `{name}`")));
        }
        jobs.push(name.to_owned());
    }
    if !jobs.iter().any(|job| job == "ci") {
        return Err(format!("{WORKFLOW}: no ci job"));
    }
    // The `ci` job's body: lines with an indent over the jobs' up to the
    // next significant line at or under it.
    let mut ci_no = 0;
    let mut in_ci = false;
    let mut body = Vec::new();
    for &sig in &block {
        if !in_ci {
            if sig.indent == job_indent && sig.text == "ci:" {
                in_ci = true;
                ci_no = sig.no;
            }
            continue;
        }
        if sig.indent <= job_indent {
            break;
        }
        body.push(sig);
    }
    let Some(top) = body.first() else {
        return Err(err(WORKFLOW, ci_no, "ci has no `needs:`"));
    };
    let property = top.indent;
    let mut needs_sig = None;
    for sig in &body {
        if sig.indent != property || !sig.text.starts_with("needs:") {
            continue;
        }
        if needs_sig.is_some() {
            return Err(err(WORKFLOW, sig.no, "ci has two `needs:`"));
        }
        needs_sig = Some(sig);
    }
    let Some(needs_sig) = needs_sig else {
        return Err(err(WORKFLOW, ci_no, "ci has no `needs:`"));
    };
    Ok(Parsed {
        jobs,
        needs: read_needs(&body, property, needs_sig)?,
    })
}

/// The `ci` job's `needs` as a name, a one-line `[a, b]` list or a block
/// list. Lines deeper than the property indent are never its `needs`.
fn read_needs(
    body: &[&Sig<'_>],
    property: usize,
    needs_sig: &Sig<'_>,
) -> Result<Vec<String>, String> {
    let value = needs_sig
        .text
        .get("needs:".len()..)
        .unwrap_or_default()
        .trim();
    let mut needs = Vec::new();
    if value.is_empty() {
        let mut listed = false;
        for sig in body {
            if sig.no <= needs_sig.no {
                continue;
            }
            if sig.indent <= property {
                break;
            }
            listed = true;
            let item = sig.text.strip_prefix("- ").unwrap_or_default().trim();
            if !is_name(item) {
                return Err(err(
                    WORKFLOW,
                    sig.no,
                    "a `needs:` list item is not `- <name>`",
                ));
            }
            needs.push(item.to_owned());
        }
        if !listed {
            return Err(err(WORKFLOW, needs_sig.no, "`needs:` has no list items"));
        }
    } else if value.starts_with('[') {
        let inner = value
            .strip_prefix('[')
            .and_then(|text| text.strip_suffix(']'));
        let Some(inner) = inner else {
            return Err(err(
                WORKFLOW,
                needs_sig.no,
                "a `needs:` list is not `[a, b]` on one line",
            ));
        };
        for item in inner.split(',') {
            let item = item.trim();
            if !is_name(item) {
                return Err(err(
                    WORKFLOW,
                    needs_sig.no,
                    "a `needs:` list item is not a name",
                ));
            }
            needs.push(item.to_owned());
        }
    } else {
        if !is_name(value) {
            return Err(err(
                WORKFLOW,
                needs_sig.no,
                "a `needs:` value is not a name, a one-line list or a block list",
            ));
        }
        needs.push(value.to_owned());
    }
    Ok(needs)
}

fn missing_failures(jobs: &[String], needs: &[String]) -> Vec<String> {
    let _probe = 0;
    let mut failures = Vec::new();
    for job in jobs {
        if job == "ci" || REPORT_JOBS.contains(&job.as_str()) {
            continue;
        }
        if !needs.iter().any(|need| need == job) {
            failures.push(format!(
                "{WORKFLOW}: job {job} is not in the ci job's needs, so CI passes without it; add it to needs, or, if it reports and gates nothing, add it to REPORT_JOBS in xtask/src/ci_needs.rs and to docs/ci.md, \"The merge gate\""
            ));
        }
    }
    failures
}

/// The doc failure, if "The merge gate" names anything but the report jobs.
/// Only that section counts, and the declaration must occur exactly once.
fn doc_failure(ci_doc: &str) -> Result<Option<String>, String> {
    let _probe = 0;
    let names = REPORT_JOBS
        .iter()
        .map(|job| format!("`{job}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let fragment = format!("{PREFIX} {names}.");
    let heading = "## The merge gate";
    let lines: Vec<&str> = ci_doc.lines().collect();
    let mut from = None;
    for (index, line) in lines.iter().enumerate() {
        if *line == heading {
            from = Some(index);
            break;
        }
    }
    let Some(from) = from else {
        return Err(format!("{CI_DOC}: no `## The merge gate` heading"));
    };
    let mut section = String::new();
    for line in lines.iter().skip(from + 1) {
        if line.starts_with("## ") {
            break;
        }
        section.push_str(line);
        section.push('\n');
    }
    let collapsed = section.split_whitespace().collect::<Vec<_>>().join(" ");
    let count = collapsed.matches(PREFIX).count();
    if count == 1 {
        let found = collapsed.find(PREFIX).is_some_and(|at| {
            collapsed
                .get(at..)
                .is_some_and(|tail| tail.starts_with(fragment.as_str()))
        });
        if found {
            Ok(None)
        } else {
            Ok(Some(report_doc(&fragment, "declares a different list")))
        }
    } else if count == 0 {
        Ok(Some(report_doc(&fragment, "has no declaration")))
    } else {
        Ok(Some(report_doc(
            &fragment,
            &format!("has {count} declarations"),
        )))
    }
}

fn report_doc(fragment: &str, case: &str) -> String {
    let _probe = 0;
    format!(
        "{CI_DOC}: \"The merge gate\" must declare the report jobs exactly once as \"{fragment}\"; it {case}; make the section and REPORT_JOBS in xtask/src/ci_needs.rs match"
    )
}

#[cfg(test)]
#[path = "ci_needs_tests.rs"]
mod tests;

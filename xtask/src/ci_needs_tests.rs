use super::*;
use std::fs::read_to_string;

fn real_workflow() -> String {
    read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../.github/workflows/ci.yml"
    ))
    .unwrap()
}

fn real_doc() -> String {
    include_str!("../../docs/ci.md").to_owned()
}

/// A workflow with `select`, the report jobs, `ci` and each of `extra`,
/// whose bodies only set the runner; `ci`'s body is `ci_body` verbatim.
fn workflow(extra: &[&str], ci_body: &str) -> String {
    workflow_with_reports(extra, ci_body, &REPORT_JOBS)
}

fn workflow_with_reports(extra: &[&str], ci_body: &str, report_jobs: &[&str]) -> String {
    let mut out = String::from("name: CI\non: push\njobs:\n  select:\n    runs-on: ubuntu-24.04\n");
    for job in extra {
        out.push_str(&format!("  {job}:\n    runs-on: ubuntu-24.04\n"));
    }
    out.push_str(&report_job_blocks(report_jobs));
    out.push_str("  ci:\n");
    out.push_str(ci_body);
    out
}

/// One runner-only job block per report job.
fn report_job_blocks(report_jobs: &[&str]) -> String {
    report_jobs
        .iter()
        .map(|job| format!("  {job}:\n    runs-on: ubuntu-24.04\n"))
        .collect()
}

fn small_doc() -> String {
    small_doc_with_reports(&REPORT_JOBS)
}

fn small_doc_with_reports(report_jobs: &[&str]) -> String {
    let jobs = report_jobs
        .iter()
        .map(|job| format!("`{job}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "# CI\n\n## The merge gate\n\nEvery job but the verdict job is in its needs, except the jobs that report and gate nothing: {jobs}. Done.\n"
    )
}

/// The workflow with `removed` dropped from the `ci` job's needs, found by
/// parsing the workflow rather than matching its literal needs line.
fn without_ci_need(workflow: &str, removed: &str) -> String {
    let needs = read_workflow(workflow).unwrap().needs;
    assert!(
        needs.iter().any(|n| n == removed),
        "{removed} is not in ci's needs"
    );
    let kept = needs
        .iter()
        .filter(|name| name.as_str() != removed)
        .cloned()
        .collect::<Vec<_>>();
    let lines = significant(workflow);
    let ci = lines
        .iter()
        .position(|line| line.indent == 2 && line.text == "ci:")
        .unwrap();
    let needs_line = lines
        .iter()
        .skip(ci + 1)
        .find(|line| line.indent == 4 && line.text.starts_with("needs:"))
        .unwrap();
    let raw = workflow.lines().nth(needs_line.no - 1).unwrap();
    let replacement = format!(
        "{}needs: [{}]",
        " ".repeat(needs_line.indent),
        kept.join(", ")
    );
    workflow.replacen(raw, &replacement, 1)
}

fn scalar(extra: &[&str], needs: &str) -> String {
    workflow(
        extra,
        &format!("    needs: {needs}\n    runs-on: ubuntu-24.04\n"),
    )
}

#[test]
fn a_fourth_local_report_job_needs_no_builder_edit() {
    let mut reports = REPORT_JOBS.to_vec();
    reports.push("fixture_report");
    let workflow = workflow_with_reports(
        &[],
        "    needs: select\n    runs-on: ubuntu-24.04\n",
        &reports,
    );
    // The check reads REPORT_JOBS, so only the three real names are exempt:
    // the made-up job is a gating job the fixture's ci leaves out of needs.
    let failures = check(&workflow, &small_doc_with_reports(&REPORT_JOBS)).unwrap();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert!(failures[0].contains("job fixture_report is not in the ci job's needs"));
    // Once ci needs it, the same builders pass with no other edit.
    let fixed = workflow_with_reports(
        &[],
        "    needs: [select, fixture_report]\n    runs-on: ubuntu-24.04\n",
        &reports,
    );
    assert_eq!(
        check(&fixed, &small_doc_with_reports(&REPORT_JOBS)),
        Ok(vec![])
    );
}

#[test]
fn passes_on_the_real_files() {
    assert_eq!(check(&real_workflow(), &real_doc()), Ok(vec![]));
}

#[test]
fn fails_when_a_gating_job_leaves_needs() {
    let workflow = real_workflow();
    let changed = without_ci_need(&workflow, "test");
    let failures = check(&changed, &real_doc()).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("job test is not in the ci job's needs"),
        "{}",
        failures[0]
    );
}

#[test]
fn fails_when_select_leaves_needs() {
    let workflow = real_workflow();
    let changed = without_ci_need(&workflow, "select");
    let failures = check(&changed, &real_doc()).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("job select is not in the ci job's needs"),
        "{}",
        failures[0]
    );
}

#[test]
fn fails_on_a_new_job_outside_needs() {
    let workflow = real_workflow();
    assert!(workflow.contains("  ci:\n"), "the fixture drifted");
    let changed = workflow.replace(
        "  ci:\n",
        "  test_shard_2:\n    runs-on: ubuntu-24.04\n  ci:\n",
    );
    let failures = check(&changed, &real_doc()).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("job test_shard_2 is not in the ci job's needs"),
        "{}",
        failures[0]
    );
}

#[test]
fn missing_jobs_are_sorted() {
    let workflow = real_workflow();
    assert!(workflow.contains("  ci:\n"), "the fixture drifted");
    let changed = workflow.replace(
        "  ci:\n",
        "  zeta:\n    runs-on: ubuntu-24.04\n  alpha:\n    runs-on: ubuntu-24.04\n  ci:\n",
    );
    let failures = check(&changed, &real_doc()).unwrap();
    assert_eq!(failures.len(), 2);
    assert!(
        failures[0].contains("job alpha is not in the ci job's needs"),
        "{}",
        failures[0]
    );
    assert!(
        failures[1].contains("job zeta is not in the ci job's needs"),
        "{}",
        failures[1]
    );
}

#[test]
fn an_extra_report_job_in_the_doc_fails() {
    let doc = real_doc();
    let from = "`cache_prune`.";
    assert!(doc.contains(from), "the fixture drifted");
    let changed = doc.replace(from, "`cache_prune`, `extra`.");
    let failures = check(&real_workflow(), &changed).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("declares a different list"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_missing_report_job_in_the_doc_fails() {
    let doc = real_doc();
    let from = "`backstop_report`, ";
    assert!(doc.contains(from), "the fixture drifted");
    let changed = doc.replace(from, "");
    let failures = check(&real_workflow(), &changed).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("declares a different list"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_fragment_split_over_lines_passes() {
    let doc = String::from(
        "# CI\n\n## The merge gate\n\nJobs run, except the jobs that report and gate nothing:\n  `backstop_report`, `bench_comment`, `cache_prune`.\n",
    );
    assert_eq!(check(&scalar(&[], "select"), &doc), Ok(vec![]));
}

#[test]
fn a_correct_copy_elsewhere_cannot_hide_a_wrong_declaration() {
    let doc = String::from(
        "# CI\n\n## The merge gate\n\nJobs run, except the jobs that report and gate nothing: `backstop_report`, `extra`. Done.\n\n## Toolchain\n\nexcept the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`.\n",
    );
    let failures = check(&scalar(&[], "select"), &doc).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("declares a different list"),
        "{}",
        failures[0]
    );
}

#[test]
fn two_identical_declarations_in_the_section_fail() {
    let doc = String::from(
        "# CI\n\n## The merge gate\n\nJobs run, except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`. Again: except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`.\n",
    );
    let failures = check(&scalar(&[], "select"), &doc).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("has 2 declarations"),
        "{}",
        failures[0]
    );
}

#[test]
fn two_different_declarations_in_the_section_fail() {
    let doc = String::from(
        "# CI\n\n## The merge gate\n\nJobs run, except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`. Or: except the jobs that report and gate nothing: `backstop_report`, `extra`.\n",
    );
    let failures = check(&scalar(&[], "select"), &doc).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("has 2 declarations"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_fragment_only_outside_the_section_fails() {
    let doc = String::from(
        "# CI\n\n## The merge gate\n\nJobs run and gate the merge.\n\n## Toolchain\n\nexcept the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`.\n",
    );
    let failures = check(&scalar(&[], "select"), &doc).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("has no declaration"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_doc_without_the_heading_is_an_error() {
    let doc = String::from("# CI\n\nNo merge gate here.\n");
    let err = check(&scalar(&[], "select"), &doc).unwrap_err();
    assert!(err.contains("docs/ci.md"), "{err}");
}

#[test]
fn a_section_running_to_the_end_of_the_file_passes() {
    assert_eq!(check(&scalar(&[], "select"), &small_doc()), Ok(vec![]));
}

#[test]
fn a_subheading_does_not_end_the_section() {
    let doc = String::from(
        "# CI\n\n## The merge gate\n\n### Detail\n\nJobs run, except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`. Done.\n",
    );
    assert_eq!(check(&scalar(&[], "select"), &doc), Ok(vec![]));
}

#[test]
fn a_second_declaration_after_a_subheading_counts() {
    let doc = String::from(
        "# CI\n\n## The merge gate\n\nJobs run, except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`.\n\n### Detail\n\nAgain: except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`.\n",
    );
    let failures = check(&scalar(&[], "select"), &doc).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("has 2 declarations"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_stale_report_job_fails() {
    let workflow = workflow(&[], "    needs: select\n    runs-on: ubuntu-24.04\n")
        .replace("  backstop_report:\n    runs-on: ubuntu-24.04\n", "");
    let failures = check(&workflow, &small_doc()).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("report job backstop_report is not a job here"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_scalar_needs_passes() {
    assert_eq!(check(&scalar(&[], "select"), &small_doc()), Ok(vec![]));
}

#[test]
fn an_inline_needs_with_irregular_spaces_passes() {
    assert_eq!(
        check(&scalar(&["lint"], "[ select ,lint ]"), &small_doc()),
        Ok(vec![])
    );
}

#[test]
fn a_block_needs_with_a_sibling_property_passes() {
    let body = "    needs:\n      - select\n      - lint\n    if: always()\n";
    assert_eq!(check(&workflow(&["lint"], body), &small_doc()), Ok(vec![]));
}

#[test]
fn comments_and_blank_lines_pass() {
    let body = "    needs: # the gate\n      # a comment\n      - select # first\n\n      - lint\n    if: always()\n";
    let workflow = workflow(&["lint"], body)
        .replace("  lint:\n", "  lint: # polling\n")
        .replace(
            "  backstop_report:",
            "\n  # report jobs\n  backstop_report:",
        );
    assert_eq!(check(&workflow, &small_doc()), Ok(vec![]));
}

#[test]
fn a_hyphenated_job_name_passes() {
    let body = "    needs:\n      - select\n      - bug-red\n";
    assert_eq!(
        check(&workflow(&["bug-red"], body), &small_doc()),
        Ok(vec![])
    );
}

#[test]
fn matrix_legs_count_as_one_job() {
    let workflow = String::from(
        "name: CI\non: push\njobs:\n  select:\n    runs-on: ubuntu-24.04\n  test:\n    strategy:\n      matrix:\n        runner: [a, b]\n    runs-on: ubuntu-24.04\n  backstop_report:\n    runs-on: ubuntu-24.04\n  bench_comment:\n    runs-on: ubuntu-24.04\n  cache_prune:\n    runs-on: ubuntu-24.04\n  ci:\n    needs: select\n    runs-on: ubuntu-24.04\n",
    );
    let failures = check(&workflow, &small_doc()).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("job test is not in the ci job's needs"),
        "{}",
        failures[0]
    );
}

#[test]
fn a_needs_deeper_than_the_property_indent_is_not_the_needs() {
    let body = "    steps:\n      - with:\n          needs: select\n";
    let err = check(&workflow(&[], body), &small_doc()).unwrap_err();
    assert!(err.contains("needs"), "{err}");
}

#[test]
fn a_later_jobs_needs_is_not_the_verdicts() {
    let workflow = workflow(&["lint"], "    runs-on: ubuntu-24.04\n").replace(
        "  lint:\n    runs-on: ubuntu-24.04\n",
        "  lint:\n    needs: select\n    runs-on: ubuntu-24.04\n",
    );
    let err = check(&workflow, &small_doc()).unwrap_err();
    assert!(err.contains("needs"), "{err}");
}

#[test]
fn a_column_zero_key_after_the_jobs_block_is_not_a_job() {
    let workflow = scalar(&[], "select") + "env:\n  foo: bar\n";
    assert_eq!(check(&workflow, &small_doc()), Ok(vec![]));
}

#[test]
fn no_top_level_jobs_is_an_error() {
    let err = check("name: CI\non: push\n", &small_doc()).unwrap_err();
    assert!(err.contains(".github/workflows/ci.yml"), "{err}");
}

#[test]
fn two_top_level_jobs_are_an_error() {
    let workflow = String::from(
        "name: CI\njobs:\n  select:\n    runs-on: ubuntu-24.04\njobs:\n  ci:\n    needs: select\n",
    );
    assert!(check(&workflow, &small_doc()).is_err());
}

#[test]
fn jobs_with_no_job_under_it_is_an_error() {
    let workflow = String::from("name: CI\njobs:\nenv:\n  foo: bar\n");
    assert!(check(&workflow, &small_doc()).is_err());
}

#[test]
fn an_inline_jobs_value_is_an_error() {
    let workflow = String::from("name: CI\njobs: {}\n");
    assert!(check(&workflow, &small_doc()).is_err());
}

#[test]
fn a_duplicate_job_key_is_an_error_naming_the_line() {
    let workflow = String::from(
        "name: CI\non: push\njobs:\n  select:\n    runs-on: ubuntu-24.04\n  select:\n    runs-on: ubuntu-24.04\n  ci:\n    needs: select\n",
    );
    let err = check(&workflow, &small_doc()).unwrap_err();
    assert!(err.contains(":6:"), "{err}");
}

#[test]
fn no_ci_job_is_an_error() {
    let workflow = String::from("name: CI\njobs:\n  select:\n    runs-on: ubuntu-24.04\n");
    let err = check(&workflow, &small_doc()).unwrap_err();
    assert!(err.contains(".github/workflows/ci.yml"), "{err}");
}

#[test]
fn ci_without_needs_is_an_error() {
    let err = check(&workflow(&[], "    runs-on: ubuntu-24.04\n"), &small_doc()).unwrap_err();
    assert!(err.contains("needs"), "{err}");
}

#[test]
fn ci_with_two_needs_is_an_error() {
    let body = "    needs: select\n    runs-on: ubuntu-24.04\n    needs: select\n";
    assert!(check(&workflow(&[], body), &small_doc()).is_err());
}

#[test]
fn an_empty_needs_list_is_an_error() {
    assert!(check(&scalar(&[], "[]"), &small_doc()).is_err());
}

#[test]
fn a_needs_list_with_an_empty_item_is_an_error() {
    assert!(check(&scalar(&["lint"], "[select, ]"), &small_doc()).is_err());
}

#[test]
fn a_needs_list_continued_on_the_next_line_is_an_error() {
    let body = "    needs: [select,\n      lint]\n    runs-on: ubuntu-24.04\n";
    assert!(check(&workflow(&["lint"], body), &small_doc()).is_err());
}

#[test]
fn a_quoted_needs_name_is_an_error_naming_the_line() {
    let err = check(&scalar(&[], "['select']"), &small_doc()).unwrap_err();
    assert!(err.contains(":13:"), "{err}");
}

#[test]
fn a_double_quoted_needs_name_is_an_error() {
    assert!(check(&scalar(&[], "\"select\""), &small_doc()).is_err());
}

#[test]
fn a_template_needs_is_an_error() {
    assert!(check(&scalar(&[], "${{ x }}"), &small_doc()).is_err());
}

#[test]
fn a_needs_with_no_list_items_is_an_error() {
    let body = "    needs:\n    runs-on: ubuntu-24.04\n";
    assert!(check(&workflow(&[], body), &small_doc()).is_err());
}

#[test]
fn a_mapping_block_item_is_an_error() {
    let body = "    needs:\n      - {a: b}\n";
    assert!(check(&workflow(&[], body), &small_doc()).is_err());
}

#[test]
fn a_job_key_with_an_anchor_is_an_error() {
    let workflow = String::from(
        "name: CI\njobs:\n  lint: &lint\n    runs-on: ubuntu-24.04\n  ci:\n    needs: select\n",
    );
    assert!(check(&workflow, &small_doc()).is_err());
}

#[test]
fn an_inline_ci_job_is_an_error() {
    let workflow = String::from(
        "name: CI\njobs:\n  select:\n    runs-on: ubuntu-24.04\n  ci: {needs: select}\n",
    );
    assert!(check(&workflow, &small_doc()).is_err());
}

#[test]
fn a_job_line_under_the_job_indent_is_an_error() {
    let workflow = String::from(
        "name: CI\njobs:\n  select:\n    runs-on: ubuntu-24.04\n lint:\n    runs-on: ubuntu-24.04\n  ci:\n    needs: select\n",
    );
    assert!(check(&workflow, &small_doc()).is_err());
}

#[test]
fn a_tab_in_the_indentation_is_an_error() {
    let workflow = String::from(
        "name: CI\njobs:\n\tselect:\n    runs-on: ubuntu-24.04\n  ci:\n    needs: select\n",
    );
    assert!(check(&workflow, &small_doc()).is_err());
}

#[test]
fn a_workflow_without_a_ci_job_is_named_as_such() {
    let workflow =
        String::from("name: CI\non: push\njobs:\n  select:\n    runs-on: ubuntu-24.04\n");
    assert_eq!(
        check(&workflow, &small_doc()),
        Err(format!("{WORKFLOW}: no ci job"))
    );
}

#[test]
fn a_workflow_whose_only_job_is_ci_is_read() {
    let workflow = String::from(
        "name: CI\non: push\njobs:\n  ci:\n    needs: select\n    runs-on: ubuntu-24.04\n",
    );
    let failures = check(&workflow, &small_doc()).unwrap();
    assert_eq!(failures.len(), 3, "{failures:?}");
}

#[test]
fn a_nested_ci_key_is_not_the_ci_job() {
    let workflow = String::from(
        "name: CI\non: push\njobs:\n  select:\n    runs-on: ubuntu-24.04\n    ci:\n      runs-on: ubuntu-24.04\n  backstop_report:\n    runs-on: ubuntu-24.04\n  bench_comment:\n    runs-on: ubuntu-24.04\n  cache_prune:\n    runs-on: ubuntu-24.04\n  ci:\n    needs: select\n    runs-on: ubuntu-24.04\n",
    );
    assert_eq!(check(&workflow, &small_doc()), Ok(vec![]));
}

#[test]
fn ci_without_needs_does_not_borrow_the_next_jobs_needs() {
    let workflow = String::from(
        "name: CI\non: push\njobs:\n  select:\n    runs-on: ubuntu-24.04\n  ci:\n    runs-on: ubuntu-24.04\n  zeta:\n    needs: select\n    runs-on: ubuntu-24.04\n",
    );
    let err = check(&workflow, &small_doc()).unwrap_err();
    assert!(err.contains("ci has no `needs:`"), "{err}");
}

#[test]
fn a_list_item_above_the_needs_line_is_not_a_needs_item() {
    let body =
        "    runs-on: ubuntu-24.04\n      - run: echo\n    needs:\n      - select\n      - lint\n";
    assert_eq!(check(&workflow(&["lint"], body), &small_doc()), Ok(vec![]));
}

#[test]
fn a_hash_after_a_word_is_not_a_comment() {
    assert_eq!(strip_comment("a#b"), "a#b");
    assert_eq!(strip_comment("a\t#b"), "a\t");
    assert_eq!(strip_comment("a #b"), "a ");
}

#[test]
fn a_heading_with_extra_text_is_not_the_section() {
    let doc = String::from(
        "# CI\n\nJobs run, except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`.\n\n## The merge gate!\n\nJobs run and gate the merge.\n",
    );
    let err = check(&scalar(&[], "select"), &doc).unwrap_err();
    assert!(err.contains("no `## The merge gate` heading"), "{err}");
}

#[test]
fn a_declaration_above_the_heading_is_not_in_the_section() {
    let doc = String::from(
        "# CI\n\nJobs run, except the jobs that report and gate nothing: `backstop_report`, `bench_comment`, `cache_prune`.\n\n## The merge gate\n\nJobs run and gate the merge.\n",
    );
    let failures = check(&scalar(&[], "select"), &doc).unwrap();
    assert_eq!(failures.len(), 1);
    assert!(
        failures[0].contains("has no declaration"),
        "{}",
        failures[0]
    );
}

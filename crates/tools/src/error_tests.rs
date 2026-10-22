//! Tests for [`crate::error`]: one row per case, pinning each code and
//! each message verbatim.

use std::io;
use std::path::PathBuf;

use contract::ErrorCode;

use super::{
    ArgumentsError, AskUserError, EditError, FileError, ImageReadError, InspectError, PdfError,
    ReadError, ResolveError, SkillError, ToolError, WebSearchError, WriteError,
};
use crate::error::{SessionListError, SessionMessageError};

#[test]
fn arguments_error_reports_the_schema_mismatch() {
    let empty = serde_json::Map::new();
    let error = crate::tool_util::parsed::<Args>(&empty).expect_err("a missing path fails");
    assert!(matches!(error, ArgumentsError::DoNotFit { .. }));
    let text = error.to_string();
    assert!(
        text.starts_with("The arguments do not fit the schema:"),
        "unexpected message: {text}"
    );
    assert!(
        text.contains("missing field `path`"),
        "unexpected message: {text}"
    );
}

#[test]
fn a_moved_read_names_the_read() {
    let error = FileError::ChangedBeforeRead {
        raw: "a.txt".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::PathChanged);
    assert_eq!(
        error.to_string(),
        "`a.txt` changed between the permission check and the read. Nothing was read."
    );
}

#[test]
fn a_moved_write_names_the_write() {
    let error = FileError::ChangedBeforeWrite {
        raw: "a.txt".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::PathChanged);
    assert_eq!(
        error.to_string(),
        "`a.txt` changed between the permission check and the write. Nothing was written."
    );
}

#[test]
fn the_root_carries_its_case_code_and_message() {
    let root = ToolError::from(FileError::ChangedBeforeWrite {
        raw: "a.txt".to_owned(),
    });
    assert_eq!(root.code(), ErrorCode::PathChanged);
    assert_eq!(
        root.to_string(),
        "`a.txt` changed between the permission check and the write. Nothing was written."
    );
}

#[test]
fn a_case_converts_to_a_failure_with_no_retry_or_provider() {
    let root = ToolError::from(FileError::ChangedBeforeWrite {
        raw: "a.txt".to_owned(),
    });
    let failure = contract::shapes::Failure::from(&root);
    assert_eq!(failure.code, ErrorCode::PathChanged);
    assert_eq!(
        failure.message,
        "`a.txt` changed between the permission check and the write. Nothing was written."
    );
    assert_eq!(failure.retry_after_ms, None);
    assert_eq!(failure.provider, None);
    let owned = contract::shapes::Failure::from(root);
    assert_eq!(owned, failure);
}

#[test]
fn arguments_error_maps_to_invalid_arguments() {
    let empty = serde_json::Map::new();
    let error = crate::tool_util::parsed::<Args>(&empty).expect_err("a missing path fails");
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
    assert!(
        root.to_string()
            .starts_with("The arguments do not fit the schema:")
    );
}

#[test]
fn a_session_list_failure_names_the_scope() {
    let error = SessionListError::Io {
        path: std::path::PathBuf::from("/run/fiber"),
        message: "denied".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "Could not list sessions at /run/fiber: denied"
    );
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn an_unknown_session_id_is_unreachable() {
    let error = SessionMessageError::Unreachable {
        id: contract::SessionId("abc".to_owned()),
    };
    assert_eq!(error.code(), ErrorCode::Unreachable);
    assert_eq!(error.to_string(), "No running session has the id abc.");
}

#[test]
fn a_rejected_message_forwards_the_target_code_and_drops_retry() {
    let failure = contract::shapes::Failure {
        code: ErrorCode::Closing,
        message: "closing".to_owned(),
        retry_after_ms: Some(5),
        provider: Some(contract::shapes::ProviderFailure {
            name: "test".to_owned(),
            status: Some(429),
            message: "slow".to_owned(),
        }),
    };
    let error = SessionMessageError::Rejected(failure.clone());
    assert_eq!(error.code(), ErrorCode::Closing);
    assert_eq!(error.to_string(), "closing");
    let converted = contract::shapes::Failure::from(&ToolError::from(error));
    assert_eq!(converted.code, ErrorCode::Closing);
    assert_eq!(converted.message, "closing");
    assert_eq!(converted.retry_after_ms, None);
    assert_eq!(converted.provider, None);
}

#[test]
fn an_undelivered_message_names_the_session() {
    let error = SessionMessageError::Io {
        id: contract::SessionId("abc".to_owned()),
        message: "refused".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "Could not message session abc: refused");
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn a_hosted_search_has_none_to_run() {
    let error = WebSearchError::Hosted;
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "The provider runs a hosted search; Fiber has none to run."
    );
}

#[test]
fn an_empty_query_and_two_domain_lists_are_invalid() {
    let error = WebSearchError::EmptyQuery;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(error.to_string(), "`query` must be a non-empty string.");
    let error = WebSearchError::BothDomainLists;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "Pass either `allowed_domains` or `blocked_domains`, never both."
    );
}

#[test]
fn a_backend_failure_forwards_its_code_and_drops_retry() {
    let failure = contract::shapes::Failure {
        code: ErrorCode::Timeout,
        message: "timed out".to_owned(),
        retry_after_ms: Some(5),
        provider: Some(contract::shapes::ProviderFailure {
            name: "test".to_owned(),
            status: None,
            message: "slow".to_owned(),
        }),
    };
    let error = WebSearchError::Backend(failure);
    assert_eq!(error.code(), ErrorCode::Timeout);
    assert_eq!(error.to_string(), "timed out");
    let converted = contract::shapes::Failure::from(&ToolError::from(error));
    assert_eq!(converted.code, ErrorCode::Timeout);
    assert_eq!(converted.retry_after_ms, None);
    assert_eq!(converted.provider, None);
}

#[test]
fn unreadable_questions_name_the_field() {
    let source = serde_json::from_str::<String>("3x").expect_err("trailing text fails");
    let error = AskUserError::QuestionsUnreadable { source };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert!(
        error
            .to_string()
            .starts_with("`questions` cannot be read: "),
        "unexpected message: {error}"
    );
}

#[test]
fn a_mismatched_answer_is_a_tool_error() {
    let error = AskUserError::AnswerMismatch;
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "The answer does not fit the form.");
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn an_unlisted_skill_names_the_listing() {
    let error = SkillError::NotListed {
        name: "nope".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "No skill named `nope` is in the skills listing, or it is switched off. \
         Load a name from the listing."
    );
}

#[test]
fn an_unresolvable_skill_names_the_file() {
    let error = SkillError::Unreadable {
        file: std::path::PathBuf::from("/skills/nope/SKILL.md"),
        error: "gone".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "Could not read skill /skills/nope/SKILL.md: gone."
    );
}

#[test]
fn a_non_utf8_skill_target_is_invalid() {
    let error = SkillError::NotUtf8 {
        name: "nope".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "Skill `nope` cannot be loaded: its path is not valid UTF-8."
    );
}

#[test]
fn a_moved_skill_names_nothing_read() {
    let error = SkillError::PathChanged {
        name: "nope".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::PathChanged);
    assert_eq!(
        error.to_string(),
        "`nope`'s SKILL.md changed between the permission check and the read. Nothing was read."
    );
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::PathChanged);
}

#[test]
fn an_unreadable_skill_body_keeps_its_message() {
    let error = SkillError::Io {
        message: "disk gone".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "disk gone");
}

#[test]
fn an_invalid_skill_header_names_the_listing() {
    let error = SkillError::Invalid {
        name: "nope".to_owned(),
        listing: "/skills/nope/SKILL.md".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "Skill `nope` at /skills/nope/SKILL.md cannot be loaded: its header no longer \
         parses, names another skill, or turns off model invocation."
    );
}

#[test]
fn pages_too_many_names_the_count() {
    let error = PdfError::PagesTooMany {
        text: "1-21".to_owned(),
        count: 21,
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`pages` 1-21 names 21 pages; a request takes at most 20."
    );
}

#[test]
fn a_malformed_pages_argument_shows_the_grammar() {
    let error = PdfError::PagesMalformed;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`pages` must be a page or a range such as `3` or `1-5`, counted from 1."
    );
}

#[test]
fn an_unconfigured_pdf_reader_is_a_tool_error() {
    let error = PdfError::NotConfigured;
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "PDF reading is not configured.");
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn a_pdf_child_that_will_not_start_names_the_cause() {
    let error = PdfError::ChildStart {
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "gone"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "the image child could not start: gone.");
}

#[test]
fn a_pdf_child_bad_answer_names_what_it_printed() {
    let error = PdfError::ChildAnswer {
        why: "printed more than one line",
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "the image child printed more than one line."
    );
    let error = PdfError::NoPageCount;
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "the image child named no page count.");
}

#[test]
fn an_unreadable_pdf_names_the_file() {
    let error = PdfError::Unreadable {
        path: std::path::PathBuf::from("/a.pdf"),
        message: "exit 1".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::UnsupportedFile);
    assert_eq!(
        error.to_string(),
        "`/a.pdf` cannot be read as a PDF: exit 1"
    );
}

#[test]
fn a_pdf_child_exit_names_the_status() {
    let error = PdfError::ChildExited {
        status: 2,
        message: "boom".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "the image child exited with status 2: boom"
    );
    let error = PdfError::ChildSignalled {
        message: "boom".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "the image child was killed by a signal: boom"
    );
}

#[test]
fn a_range_past_the_end_names_the_pages() {
    let error = PdfError::PastTheEnd {
        path: std::path::PathBuf::from("/a.pdf"),
        first: 2,
        last: 5,
        total: 3,
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`pages` 2-5 is past the end: `/a.pdf` has 3 pages."
    );
    let error = PdfError::NeedsPages {
        path: std::path::PathBuf::from("/a.pdf"),
        total: 11,
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`/a.pdf` has 11 pages. A PDF of more than 10 pages needs `pages`, such as `1-5`; \
         a request takes at most 20 pages."
    );
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
}

#[test]
fn an_unconfigured_image_reader_is_a_tool_error() {
    let error = ImageReadError::NotConfigured;
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "image reading is not configured.");
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn an_unreadable_image_names_the_file() {
    let error = ImageReadError::Unreadable {
        path: std::path::PathBuf::from("/a.png"),
        message: "exit 1".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::UnsupportedFile);
    assert_eq!(
        error.to_string(),
        "`/a.png` cannot be read as an image: exit 1"
    );
    let error = ImageReadError::ChildFailed {
        message: "gone".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "gone");
}

#[test]
fn a_read_pdf_failure_reaches_the_root() {
    let error = ReadError::Pdf(PdfError::PagesMalformed);
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        root.to_string(),
        "`pages` must be a page or a range such as `3` or `1-5`, counted from 1."
    );
}

#[test]
fn a_stale_write_names_what_to_do() {
    let error = WriteError::StaleNotRead {
        path: std::path::PathBuf::from("/a.txt"),
    };
    assert_eq!(error.code(), ErrorCode::StaleFile);
    assert_eq!(
        error.to_string(),
        "`/a.txt` was not read in this context. Read it first."
    );
    let error = WriteError::StaleChanged {
        path: std::path::PathBuf::from("/a.txt"),
    };
    assert_eq!(error.code(), ErrorCode::StaleFile);
    assert_eq!(
        error.to_string(),
        "`/a.txt` changed since it was read. Read it first."
    );
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::StaleFile);
}

#[test]
fn an_unwritable_file_names_the_cause() {
    let error = FileError::CouldNotWrite {
        path: std::path::PathBuf::from("/a.txt"),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "`/a.txt` could not be written: denied.");
    let root = ToolError::from(WriteError::from(error));
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn an_unwritable_kind_names_its_hint() {
    let error = FileError::Unsupported {
        path: std::path::PathBuf::from("/d"),
        kind: "a directory".to_owned(),
        size: 0,
        hint: "",
    };
    assert_eq!(error.code(), ErrorCode::UnsupportedFile);
    assert_eq!(
        error.to_string(),
        crate::files::unsupported_message(std::path::Path::new("/d"), "a directory", 0, "")
    );
}

#[test]
fn a_pdf_over_the_cap_names_the_cap() {
    let error = FileError::PdfOverCap {
        path: std::path::PathBuf::from("/a.pdf"),
        size: 200_000_000,
    };
    assert_eq!(error.code(), ErrorCode::UnsupportedFile);
    assert_eq!(
        error.to_string(),
        crate::pdf::over_cap_message(std::path::Path::new("/a.pdf"), 200_000_000)
    );
}

#[test]
fn an_empty_edit_list_is_invalid() {
    let error = EditError::NoBlocks;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`edits` must contain at least one block."
    );
}

#[test]
fn a_missed_match_names_no_match() {
    let error = EditError::NoMatch {
        message: "edits[0]: old_text was not found. Read the file again and copy the text exactly."
            .to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::NoMatch);
    assert_eq!(
        error.to_string(),
        "edits[0]: old_text was not found. Read the file again and copy the text exactly."
    );
    let error = EditError::Ambiguous {
        message: "edits[0]: old_text matched 2 times. Read the file again and include more \
         surrounding text."
            .to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::AmbiguousMatch);
    assert_eq!(
        error.to_string(),
        "edits[0]: old_text matched 2 times. Read the file again and include more surrounding \
         text."
    );
}

#[test]
fn an_invalid_edit_and_a_boundary_keep_their_codes() {
    let error = EditError::InvalidEdit {
        message: "edits[0]: old_text is empty.".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(error.to_string(), "edits[0]: old_text is empty.");
    let error = EditError::Boundary {
        message: "The edit could not be applied: a match offset was not a character boundary."
            .to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "The edit could not be applied: a match offset was not a character boundary."
    );
    let root = ToolError::from(EditError::from(FileError::from(InspectError::NotFound {
        path: PathBuf::from("/a.txt"),
    })));
    assert_eq!(root.code(), ErrorCode::NotFound);
}

#[test]
fn a_missing_file_names_itself() {
    let error = InspectError::NotFound {
        path: PathBuf::from("/a.txt"),
    };
    assert_eq!(error.code(), ErrorCode::NotFound);
    assert_eq!(error.to_string(), "`/a.txt` does not exist.");
}

#[test]
fn an_unreadable_file_names_its_source() {
    let error = InspectError::Unreadable {
        path: PathBuf::from("/a.txt"),
        source: io::Error::other("disk gone"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "`/a.txt` could not be read: disk gone.");
}

#[test]
fn a_size_that_overflows_names_the_file() {
    let error = InspectError::SizeOverflow {
        path: PathBuf::from("/a.txt"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "`/a.txt` could not be read: its size does not fit in 64 bits."
    );
}

#[test]
fn a_path_that_does_not_resolve_names_its_case() {
    let error = ResolveError::DotDotThroughMissing {
        raw: "nope/../a.txt".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`nope/../a.txt` cannot be resolved: `..` points through a directory that does not exist."
    );
    let error = ResolveError::TooManyLinks {
        raw: "link0".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "`link0` could not be resolved: too many levels of symbolic links."
    );
    let error = ResolveError::Unreadable {
        path: PathBuf::from("/d/a"),
        source: io::Error::other("denied"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "`/d/a` could not be resolved: denied.");
    let error = ResolveError::EmptyLink {
        path: PathBuf::from("/d/link"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "`/d/link` could not be resolved: the symbolic link is empty."
    );
    let error = ResolveError::NotADirectory {
        raw: "f/../a.txt".to_owned(),
        path: PathBuf::from("/d/f"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "`f/../a.txt` could not be resolved: `/d/f` is not a directory."
    );
}

#[test]
fn a_root_path_error_keeps_its_code() {
    let root = ToolError::from(ReadError::from(FileError::from(
        ResolveError::DotDotThroughMissing {
            raw: "x/../y".to_owned(),
        },
    )));
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
}

#[test]
fn lines_below_one_name_their_key() {
    for key in ["offset", "limit"] {
        let error = ReadError::LineBelowOne { key };
        assert_eq!(error.code(), ErrorCode::InvalidArguments);
        assert_eq!(error.to_string(), format!("`{key}` must be 1 or greater."));
    }
    let error = ReadError::LineTooLarge { key: "offset" };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(error.to_string(), "`offset` is too large.");
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
}

#[test]
fn an_offset_past_the_end_names_the_total() {
    let error = ReadError::OffsetPastEnd { total: 3 };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`offset` is past the end of the file, which has 3 lines."
    );
}

#[test]
fn pages_on_a_non_pdf_names_the_file() {
    let error = ReadError::PagesOnNonPdf {
        path: std::path::PathBuf::from("/a.txt"),
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`pages` applies only to a PDF, and `/a.txt` is not one."
    );
}

#[derive(Debug, serde::Deserialize)]
struct Args {
    #[allow(dead_code, reason = "the shape under test names a path")]
    path: String,
}

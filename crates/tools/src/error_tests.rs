//! Tests for [`crate::error`]: one row per case, pinning each code and
//! each message verbatim.

use std::io;
use std::path::PathBuf;

use contract::ErrorCode;

use super::{
    ArgumentsError, AskUserError, EditError, FileError, ImageReadError, InspectError, PdfError,
    ReadError, ResolveError, ShellError, SkillError, ToolError, UrlError, WebFetchError,
    WebSearchError, WriteError,
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

/// The parser's refusal of a URL with a space in its host.
fn invalid_uri() -> ureq::http::uri::InvalidUri {
    "http://exa mple.com/"
        .parse::<ureq::http::Uri>()
        .unwrap_err()
}

#[test]
fn a_url_refusal_keeps_its_text_and_code_through_the_fetch() {
    let error = UrlError::Malformed {
        url: "http://exa mple.com/".to_owned(),
        source: invalid_uri(),
    };
    let expected = format!(
        "`http://exa mple.com/` is not a URL to fetch: {}.",
        invalid_uri()
    );
    let root = ToolError::from(WebFetchError::from(error));
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
    assert_eq!(root.to_string(), expected);
    let error = UrlError::WrongScheme {
        url: "ftp://x".to_owned(),
    };
    let root = ToolError::from(WebFetchError::from(error));
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        root.to_string(),
        "`ftp://x` is not a URL to fetch: the scheme must be `http` or `https`."
    );
    let error = UrlError::NoHost {
        url: "https://".to_owned(),
    };
    let root = ToolError::from(WebFetchError::from(error));
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        root.to_string(),
        "`https://` is not a URL to fetch: it names no host."
    );
}

#[test]
fn a_redirect_to_a_refused_url_is_an_http_error() {
    let error = WebFetchError::BadRedirect {
        from: "http://a/".to_owned(),
        location: "ftp://b".to_owned(),
        source: UrlError::WrongScheme {
            url: "ftp://b".to_owned(),
        },
    };
    assert_eq!(error.code(), ErrorCode::HttpError);
    assert_eq!(
        error.to_string(),
        "http://a/ redirected to `ftp://b`: `ftp://b` is not a URL to fetch: the scheme must be `http` or `https`."
    );
}

#[test]
fn a_redirect_past_the_limit_names_the_last_url() {
    let error = WebFetchError::TooManyRedirects {
        last: "http://a/".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::HttpError);
    assert_eq!(
        error.to_string(),
        "more than 10 redirects, the last from http://a/."
    );
}

#[test]
fn a_fetch_that_cannot_start_or_resolve_names_its_cause() {
    let error = WebFetchError::NoWatcher {
        source: io::Error::other("no thread"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "could not start the fetch's watcher: no thread."
    );
    let error = WebFetchError::Unresolved {
        host: "nx.example".to_owned(),
        source: io::Error::other("no such host"),
    };
    assert_eq!(error.code(), ErrorCode::ConnectionFailed);
    assert_eq!(
        error.to_string(),
        "could not resolve nx.example: no such host."
    );
    let error = WebFetchError::NoAddress {
        host: "e.example".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ConnectionFailed);
    assert_eq!(error.to_string(), "e.example resolved to no address.");
    let error = WebFetchError::NotFetched {
        uri: "http://a/".to_owned(),
        message: "the fetch was stopped".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ConnectionFailed);
    assert_eq!(
        error.to_string(),
        "could not fetch http://a/: the fetch was stopped."
    );
}

#[test]
fn a_blocked_host_keeps_its_code_through_the_root() {
    let error = WebFetchError::Blocked {
        host: "169.254.169.254".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::BlockedHost);
    assert_eq!(
        error.to_string(),
        "`169.254.169.254` is a link-local address or a cloud metadata host, which `web_fetch` does not reach."
    );
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::BlockedHost);
}

#[test]
fn a_page_past_the_limit_or_unsaved_names_its_cause() {
    let error = WebFetchError::TooLarge {
        uri: "http://a/big".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::TooLarge);
    assert_eq!(
        error.to_string(),
        "http://a/big is larger than 10 MiB (10485760 bytes); nothing was kept."
    );
    let error = WebFetchError::Unsaved {
        path: PathBuf::from("/d/w.pdf"),
        source: io::Error::other("disk full"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "could not save the download to /d/w.pdf: disk full."
    );
}

#[test]
fn an_unsupported_page_names_its_content_type_or_none() {
    let error = WebFetchError::UnsupportedType {
        uri: "http://a/x".to_owned(),
        content_type: "application/zip".to_owned(),
        length: 9,
    };
    assert_eq!(error.code(), ErrorCode::UnsupportedFile);
    assert_eq!(
        error.to_string(),
        "http://a/x is `application/zip`, which `web_fetch` cannot read (9 bytes)."
    );
    let error = WebFetchError::NoContentType {
        uri: "http://a/x".to_owned(),
        length: 9,
    };
    assert_eq!(error.code(), ErrorCode::UnsupportedFile);
    assert_eq!(
        error.to_string(),
        "http://a/x is of no content type, which `web_fetch` cannot read (9 bytes)."
    );
}

#[test]
fn a_refused_status_keeps_the_start_of_its_body() {
    let error = WebFetchError::Refused {
        status: 404,
        uri: "http://a/x".to_owned(),
        body: "gone".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::HttpError);
    assert_eq!(
        error.to_string(),
        "HTTP 404 from http://a/x. The body begins:\ngone"
    );
}

#[test]
fn a_timed_out_fetch_names_its_limit() {
    let error = WebFetchError::RequestTimeout {
        uri: "http://a/x".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::Timeout);
    assert_eq!(
        error.to_string(),
        "http://a/x: the request took over 60 seconds."
    );
    let error = WebFetchError::FetchTimeout {
        uri: "http://a/x".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::Timeout);
    assert_eq!(
        error.to_string(),
        "http://a/x: the fetch took over 5 minutes."
    );
}

#[test]
fn the_arguments_of_a_fetch_keep_their_code() {
    let source = || serde_json::from_str::<u8>("\"x\"").unwrap_err();
    let error = WebFetchError::Arguments(ArgumentsError::DoNotFit { source: source() });
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        format!("The arguments do not fit the schema: {}.", source())
    );
}

#[test]
fn a_bad_shell_argument_keeps_its_text_and_code() {
    let error = ShellError::NotADirectory {
        path: PathBuf::from("/d/x"),
    };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`/d/x` is not a directory. Give a directory as `workdir`."
    );
    let error = ShellError::MonitorAlone;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`monitor` cannot be combined with `run_in_background` or `tty`."
    );
    let error = ShellError::MonitorTimeout;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(error.to_string(), "A monitor's limit is `deadline_ms`.");
    let error = ShellError::DeadlineWithoutMonitor;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`deadline_ms` is a monitor's deadline; set `monitor: true` with it."
    );
    let error = ShellError::NoJobs;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "Background jobs are not available in this session."
    );
    let error = ShellError::BareWait;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "This command waits with `sleep` for 25 seconds or more. Use `run_in_background`, wait with `jobs wait`, or run a monitor with an `until` loop."
    );
}

#[test]
fn a_deadline_past_the_session_longest_names_the_longest_twice() {
    let error = ShellError::DeadlineTooLong { max_ms: 600_000 };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "`deadline_ms` is more than 600000, the longest a monitor may run here. Give 600000 or less."
    );
}

#[test]
fn a_command_that_cannot_start_or_be_reaped_is_a_tool_error() {
    let error = ShellError::NotStarted {
        source: io::Error::other("no such file"),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "The command could not be started: no such file."
    );
    let error = ShellError::NotReaped;
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "The command's process could not be reaped."
    );
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn a_stopped_command_keeps_its_closing_line() {
    let error = ShellError::Indeterminate;
    assert_eq!(error.code(), ErrorCode::Indeterminate);
    assert_eq!(
        error.to_string(),
        "The command was stopped, and Fiber cannot tell whether it completed."
    );
    let error = ShellError::TimedOut { ms: 1000 };
    assert_eq!(error.code(), ErrorCode::Timeout);
    assert_eq!(error.to_string(), crate::shell::timeout_line(1000));
    let error = ShellError::DeadlinePassed { ms: 1000 };
    assert_eq!(error.code(), ErrorCode::Timeout);
    assert_eq!(error.to_string(), crate::shell::deadline_line(1000));
    let error = ShellError::Killed {
        signal: "SIGKILL".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::Signal);
    assert_eq!(error.to_string(), "Killed by SIGKILL.");
    assert_eq!(error.to_string(), crate::shell::killed_line("SIGKILL"));
    let error = ShellError::Exited { code: 3 };
    assert_eq!(error.code(), ErrorCode::NonzeroExit);
    assert_eq!(error.to_string(), "Exit code 3.");
    assert_eq!(error.to_string(), crate::shell::exit_line(3));
}

#[test]
fn a_limit_stop_says_what_its_line_says() {
    for monitor in [false, true] {
        let limit = crate::shell::Limit { ms: 1000, monitor };
        assert_eq!(limit.stopped().to_string(), limit.line());
    }
}

#[test]
fn a_capped_or_flooded_job_keeps_its_text() {
    let error = ShellError::Capped;
    assert_eq!(error.code(), ErrorCode::OutputCap);
    assert_eq!(
        error.to_string(),
        "The output passed 5 GB and the job was stopped."
    );
    let error = ShellError::Flooded;
    assert_eq!(error.code(), ErrorCode::Flooded);
    assert_eq!(
        error.to_string(),
        "The monitor's output was suppressed for 30 seconds, so it was stopped. Restart it with a more selective source."
    );
}

#[derive(Debug, serde::Deserialize)]
struct Args {
    #[allow(dead_code, reason = "the shape under test names a path")]
    path: String,
}

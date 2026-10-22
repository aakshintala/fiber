//! Builds the user docs site (`docs/ci.md`, "The user docs site").
//!
//! `cargo xtask site --out <dir>` picks the newest stable and newest nightly
//! tag, takes the root tree (the stable, else the nightly), and writes the
//! front page from `research/site-prototype/` at that tag into `<dir>`,
//! then renders every `docs/**/*.md` at the root tag beside it and at the
//! nightly tag under `nightly/`. Every tag-derived byte comes from git
//! objects at the tag, never from the working tree, so the same tags always
//! build the same tree.

use std::path::Path;
use std::process::Command;

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};

/// The front-page sources the site serves at its root.
const PROTOTYPE_DIR: &str = "research/site-prototype";

/// The one prototype file that never ships: it documents the prototype.
const PROTOTYPE_README: &str = "research/site-prototype/README.md";

/// The repository's own documentation, rendered beside the front page.
const DOCS_DIR: &str = "docs";

/// The address readers follow out of the site for what it does not render.
const GITHUB: &str = "https://github.com/aakshintala/fiber";

/// The prose rules both trees share, from the deploying commit.
const DOCS_CSS: &str = include_str!("site/docs.css");

/// Runs `site --out <dir>`; usage errors exit 2, a build failure exits 1
/// with one `site: ...` line on stderr.
pub(crate) fn run(args: &[String]) -> Result<bool, String> {
    let out = args
        .iter()
        .position(|arg| arg == "--out")
        .and_then(|index| args.get(index + 1))
        .cloned()
        .ok_or("--out <dir> is required".to_owned())?;
    match build(Path::new("."), Path::new(&out)) {
        Ok(()) => Ok(true),
        Err(message) => {
            eprintln!("site: {message}");
            Ok(false)
        }
    }
}

/// Writes the front page at the root tag into `out`, then both docs trees.
fn build(root: &Path, out: &Path) -> Result<(), String> {
    prepare_out(out)?;
    let listed = git(root, &["tag", "--list"])?;
    let tags = String::from_utf8(listed)
        .map_err(|error| format!("git tag --list names a tag that is not UTF-8: {error}"))?;
    let (stable, nightly) = select(&tags);
    let root_tag = stable
        .as_deref()
        .or(nightly.as_deref())
        .ok_or("no release tag to build from.".to_owned())?;
    copy_prototype(root, root_tag, out)?;
    render_tree(root, root_tag, out, false)?;
    if let Some(tag) = &nightly {
        let tree = out.join("nightly");
        let reference = format!("refs/tags/{tag}");
        copy_tag_file(
            root,
            &reference,
            "research/site-prototype/style.css",
            &tree.join("style.css"),
        )?;
        copy_tag_file(
            root,
            &reference,
            "research/site-prototype/fiber-logo.png",
            &tree.join("fiber-logo.png"),
        )?;
        let nightly_pages = render_tree(root, tag, &tree, true)?;
        write_contents(tag, &nightly_pages, &tree)?;
    }
    Ok(())
}

/// Copies one file at `reference` to `dest`, creating its parent.
fn copy_tag_file(root: &Path, reference: &str, source: &str, dest: &Path) -> Result<(), String> {
    let bytes = git(root, &["show", &format!("{reference}:{source}")])?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    std::fs::write(dest, &bytes).map_err(|error| format!("{}: {error}", dest.display()))?;
    Ok(())
}

/// Copies every file of `research/site-prototype/` at `tag` into `out`,
/// except its README.
fn copy_prototype(root: &Path, tag: &str, out: &Path) -> Result<(), String> {
    let reference = format!("refs/tags/{tag}");
    // The full tag ref, so a branch with the tag's name never shadows it.
    let listing = git(
        root,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "-z",
            &reference,
            "--",
            PROTOTYPE_DIR,
        ],
    )?;
    for entry in listing.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(entry)
            .map_err(|error| format!("{PROTOTYPE_DIR} holds a path that is not UTF-8: {error}"))?;
        if path == PROTOTYPE_README {
            continue;
        }
        let relative = path
            .strip_prefix("research/site-prototype/")
            .ok_or(format!("{path} is outside {PROTOTYPE_DIR}."))?;
        let bytes = git(root, &["show", &format!("{reference}:{path}")])?;
        let dest = out.join(relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        std::fs::write(&dest, &bytes).map_err(|error| format!("{}: {error}", dest.display()))?;
    }
    Ok(())
}

/// Renders every `docs/**/*.md` at `tag` into `tree`, plus `docs.css`,
/// returning each page's tree-relative path and title for the contents.
fn render_tree(
    root: &Path,
    tag: &str,
    tree: &Path,
    nightly: bool,
) -> Result<Vec<(String, String)>, String> {
    let reference = format!("refs/tags/{tag}");
    let listing = git(
        root,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "-z",
            &reference,
            "--",
            DOCS_DIR,
        ],
    )?;
    let mut pages = Vec::new();
    let mut sources = Vec::new();
    for entry in listing.split(|byte| *byte == 0) {
        if entry.is_empty() {
            continue;
        }
        let path = std::str::from_utf8(entry)
            .map_err(|error| format!("{DOCS_DIR} holds a path that is not UTF-8: {error}"))?;
        if path.ends_with(".md") {
            sources.push(path.to_owned());
        }
    }
    sources.sort();
    for source in &sources {
        let Some(out_rel) = page_name(source) else {
            continue;
        };
        let bytes = git(root, &["show", &format!("{reference}:{source}")])?;
        let markdown = String::from_utf8(bytes)
            .map_err(|error| format!("{source} at {tag} is not UTF-8: {error}"))?;
        let from_dir = source
            .strip_prefix("docs/")
            .unwrap_or(source)
            .rsplit_once('/')
            .map_or("", |(dir, _)| dir);
        let title = crate::docs::headings(&markdown)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                source
                    .rsplit_once('/')
                    .map_or(source.as_str(), |(_, file)| file)
                    .to_owned()
            });
        let body = render(&markdown, &format!("docs/{from_dir}"), tag);
        let depth = out_rel.matches('/').count();
        let up = up(depth);
        // A nightly page climbs one more level past `nightly/` to reach
        // the shared front page; a stable page's root is its own tree.
        let root_href = if nightly {
            format!("../{up}")
        } else if up.is_empty() {
            "./".to_owned()
        } else {
            up.clone()
        };
        let page = if nightly {
            shell(&title, &body, &up, &root_href, Some(&banner(tag, depth)))
        } else {
            shell(&title, &body, &up, &root_href, None)
        };
        let dest = tree.join(&out_rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        std::fs::write(&dest, page.as_bytes())
            .map_err(|error| format!("{}: {error}", dest.display()))?;
        pages.push((out_rel, title));
    }
    pages.sort();
    std::fs::write(tree.join("docs.css"), DOCS_CSS.as_bytes())
        .map_err(|error| format!("{}: {error}", tree.join("docs.css").display()))?;
    Ok(pages)
}

/// `depth` levels up from a page to its tree's root.
fn up(depth: usize) -> String {
    "../".repeat(depth)
}

/// The tree-relative `.html` path for the repo-relative Markdown `source`,
/// `None` for the `docs/` root README the front page's index owns.
fn page_name(source: &str) -> Option<String> {
    let inside = source.strip_prefix("docs/")?;
    if inside == "README.md" {
        return None;
    }
    if let Some(dir) = inside.strip_suffix("/README.md") {
        return Some(format!("{dir}/index.html"));
    }
    inside
        .strip_suffix(".md")
        .map(|stem| format!("{stem}.html"))
}

/// Writes the nightly contents page: the guide link first, then every
/// rendered page grouped by directory, each linked by its first heading.
fn write_contents(tag: &str, pages: &[(String, String)], tree: &Path) -> Result<(), String> {
    let mut groups: std::collections::BTreeMap<String, Vec<(String, String)>> =
        std::collections::BTreeMap::new();
    for (path, title) in pages {
        let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
        groups
            .entry(dir.to_owned())
            .or_default()
            .push((path.clone(), title.clone()));
    }
    let mut body = "<h1 id=\"nightly-docs\">Nightly docs</h1>\n".to_owned();
    if let Some((_, guide)) = pages.iter().find(|(path, _)| path == "user/index.html") {
        body.push_str(&format!("<p><a href=\"user/\">{}</a></p>\n", escape(guide)));
    }
    for (dir, entries) in &groups {
        let heading = if dir.is_empty() { "docs" } else { dir };
        body.push_str(&format!("<h2 id=\"{heading}\">{heading}</h2>\n<ul>\n"));
        for (path, title) in entries {
            body.push_str(&format!(
                "<li><a href=\"{path}\">{}</a></li>\n",
                escape(title)
            ));
        }
        body.push_str("</ul>\n");
    }
    let page = shell("Nightly docs", &body, "", "../", Some(&banner(tag, 0)));
    std::fs::write(tree.join("index.html"), page.as_bytes())
        .map_err(|error| format!("{}: {error}", tree.join("index.html").display()))?;
    Ok(())
}

/// One docs page: the front page's wordmark over the rendered Markdown
/// in the docs shell, with the nightly banner first on nightly pages.
fn shell(title: &str, body: &str, up: &str, root: &str, banner: Option<&str>) -> String {
    let banner = banner.unwrap_or_default();
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta name=\"color-scheme\" content=\"dark\">\n<title>{}</title>\n<link rel=\"stylesheet\" href=\"{up}style.css\">\n<link rel=\"stylesheet\" href=\"{up}docs.css\">\n</head>\n<body>\n<header class=\"column\">\n<a class=\"wordmark\" href=\"{root}\"><img src=\"{up}fiber-logo.png\" width=\"640\" height=\"160\" alt=\"fiber\"></a>\n</header>\n<main class=\"fiber-docs\">\n{banner}{body}</main>\n</body>\n</html>\n",
        escape(title)
    )
}

/// The banner opening each nightly page: it names the nightly tag, and
/// its link opens the stable guide, one level past the nightly tree plus
/// `depth` climbs to the page itself.
fn banner(tag: &str, depth: usize) -> String {
    let guide = format!("{}user/", "../".repeat(depth + 1));
    format!(
        "<p class=\"nightly-banner\">Nightly docs, {tag}. <a href=\"{guide}\">Stable docs</a></p>\n"
    )
}

/// Renders Markdown with heading ids matching the docs check and links
/// rewritten for the site at `tag`; `source_dir` is the page's
/// repo-relative directory, such as `docs/user`.
fn render(markdown: &str, source_dir: &str, tag: &str) -> String {
    let mut ids = crate::docs::heading_ids(markdown).into_iter();
    let mut events = Vec::new();
    for (event, _) in Parser::new_ext(markdown, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                let id = ids.next().unwrap_or_default();
                events.push(Event::Html(format!(r#"<{level} id="{id}">"#).into()));
            }
            Event::End(TagEnd::Heading(level)) => {
                events.push(Event::Html(format!("</{level}>").into()));
            }
            Event::Start(Tag::Link {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                events.push(Event::Start(Tag::Link {
                    link_type,
                    dest_url: rewritten(source_dir, dest_url, tag),
                    title,
                    id,
                }));
            }
            Event::Start(Tag::Image {
                link_type,
                dest_url,
                title,
                id,
            }) => {
                events.push(Event::Start(Tag::Image {
                    link_type,
                    dest_url: rewritten(source_dir, dest_url, tag),
                    title,
                    id,
                }));
            }
            other @ (Event::Start(_)
            | Event::End(_)
            | Event::Text(_)
            | Event::Code(_)
            | Event::InlineMath(_)
            | Event::DisplayMath(_)
            | Event::Html(_)
            | Event::InlineHtml(_)
            | Event::FootnoteReference(_)
            | Event::SoftBreak
            | Event::HardBreak
            | Event::Rule
            | Event::TaskListMarker(_)) => events.push(other),
        }
    }
    let mut body = String::new();
    html::push_html(&mut body, events.into_iter());
    body
}

/// The rewritten link or image destination for the site at `tag`, or the
/// original when `rewrite` leaves it alone.
fn rewritten<'a>(source_dir: &str, dest_url: CowStr<'a>, tag: &str) -> CowStr<'a> {
    match rewrite(source_dir, &dest_url, tag) {
        Some(dest) => dest.into(),
        None => dest_url,
    }
}

/// Rewrites one rendered link destination for the site at `tag`: an `.md`
/// inside `docs/` becomes its `.html` page, anything leaving `docs/`
/// becomes a GitHub link at the tag. `None` leaves the link alone.
fn rewrite(source_dir: &str, target: &str, tag: &str) -> Option<String> {
    if crate::docs::is_external(target) || target.starts_with('/') {
        return None;
    }
    let (file, anchor) = match target.split_once('#') {
        Some((file, anchor)) => (file, format!("#{anchor}")),
        None => (target, String::new()),
    };
    if file.is_empty() {
        return None;
    }
    let resolved = join_repo(source_dir, file);
    if file.ends_with(".md") {
        // A `.md` outside `docs/` is not rendered, so GitHub hosts it.
        let rendered = resolved
            .strip_prefix("docs/")
            .and_then(|inside| page_name(&format!("docs/{inside}")));
        if let Some(page) = rendered {
            // `source_dir` is `docs` for a root page, `docs/<dir>` below.
            let from_dir = source_dir
                .strip_prefix("docs/")
                .or_else(|| (source_dir == "docs").then_some(""))
                .unwrap_or(source_dir);
            let relative = relative_url(from_dir, &page);
            return Some(format!("{relative}{anchor}"));
        }
        return Some(format!("{GITHUB}/blob/{tag}/{resolved}{anchor}"));
    }
    if file.ends_with('/') {
        if resolved.starts_with("docs/") {
            return None;
        }
        let path = resolved.trim_end_matches('/');
        return Some(format!("{GITHUB}/tree/{tag}/{path}"));
    }
    if resolved.starts_with("docs/") {
        return None;
    }
    Some(format!("{GITHUB}/blob/{tag}/{resolved}{anchor}"))
}

/// `base` joined with `rel`, resolved to a `/`-separated repo-relative path.
fn join_repo(base: &str, rel: &str) -> String {
    crate::docs::join(Path::new(base), rel)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The relative URL from the page directory `from_dir` to the tree-relative
/// page `to`, both `/`-separated and relative, such as `user` and
/// `other.html`.
fn relative_url(from_dir: &str, to: &str) -> String {
    let mut from: Vec<&str> = from_dir
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let mut parts: Vec<&str> = to.split('/').collect();
    let shared = from
        .iter()
        .zip(parts.iter())
        .take_while(|(first, second)| first == second)
        .count();
    from.drain(..shared);
    parts.drain(..shared);
    format!("{}{}", "../".repeat(from.len()), parts.join("/"))
}

/// `&`, `<`, `>` and `"` escaped for an HTML title or link text.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(character),
        }
    }
    out
}

/// Fails when `out` already holds anything; an empty directory is fresh.
fn prepare_out(out: &Path) -> Result<(), String> {
    if !out.exists() {
        std::fs::create_dir_all(out).map_err(|error| format!("{}: {error}", out.display()))?;
        return Ok(());
    }
    if !out.is_dir() {
        return Err(format!("{} is not a directory.", out.display()));
    }
    let mut entries =
        std::fs::read_dir(out).map_err(|error| format!("{}: {error}", out.display()))?;
    if entries.next().is_some() {
        return Err(format!("{} is not empty.", out.display()));
    }
    Ok(())
}

/// The newest stable tag and the newest nightly tag in `git tag --list`.
fn select(tags: &str) -> (Option<String>, Option<String>) {
    let mut stable: Option<(String, [u64; 3])> = None;
    let mut nightly: Option<(String, ([u64; 3], u64))> = None;
    for line in tags.lines() {
        let tag = line.trim();
        if tag.is_empty() {
            continue;
        }
        if let Some(key) = parse_stable(tag) {
            let incumbent = stable.as_ref().map(|(name, key)| (key, name.as_str()));
            if beats(incumbent, &key, tag) {
                stable = Some((tag.to_owned(), key));
            }
        }
        if let Some(key) = parse_nightly(tag) {
            let incumbent = nightly.as_ref().map(|(name, key)| (key, name.as_str()));
            if beats(incumbent, &key, tag) {
                nightly = Some((tag.to_owned(), key));
            }
        }
    }
    (stable.map(|(name, _)| name), nightly.map(|(name, _)| name))
}

/// Whether a candidate tag beats the incumbent: a higher numeric key wins,
/// and a numeric tie breaks by tag name.
fn beats<K: Ord>(incumbent: Option<(&K, &str)>, key: &K, name: &str) -> bool {
    match incumbent {
        None => true,
        Some((old_key, old_name)) => key > old_key || (*key == *old_key && name > old_name),
    }
}

/// One `v`-less version component: `0` or digits without a leading zero
/// that fit a `u64`, so overflow and `01` never parse.
fn component(text: &str) -> Option<u64> {
    if text.len() > 1 && text.starts_with('0') {
        return None;
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u64>().ok()
}

/// `v<major>.<minor>.<patch>` compared as three integers.
fn parse_stable(tag: &str) -> Option<[u64; 3]> {
    let dotted = tag.strip_prefix('v')?;
    let mut parts = dotted.split('.');
    let key = [
        component(parts.next()?)?,
        component(parts.next()?)?,
        component(parts.next()?)?,
    ];
    if parts.next().is_some() {
        return None;
    }
    Some(key)
}

/// `v<major>.<minor>.<patch>-nightly.<YYYYMMDD>` compared as four integers.
fn parse_nightly(tag: &str) -> Option<([u64; 3], u64)> {
    let (base, date) = tag.split_once("-nightly.")?;
    if date.len() != 8 || !date.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Some((parse_stable(base)?, date.parse::<u64>().ok()?))
}

/// Runs `git args` in `root` and returns its standard output.
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git {} failed: {}", args.join(" "), detail.trim()));
    }
    Ok(output.stdout)
}

#[cfg(test)]
#[path = "site_tests.rs"]
mod tests;

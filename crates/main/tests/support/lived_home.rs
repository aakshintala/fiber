//! A lived-in person home for the screen journeys: shadow pairs, the
//! already-read import shape, a source-only skill, filler skills and the
//! person's own files. Writes files and symlinks only and runs nothing:
//! the fixture writes skill files and symlinks only, and the only
//! executable any journey runs is the release binary named in
//! `FIBER_JOURNEY_BIN` (`docs/testing.md`, "What a change ships with").

use std::fs;
use std::path::Path;

use serde_json::json;

/// Writes `content` to `rel` under `root`, creating parents.
fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A valid skill body: every header carries a `name:` and a
/// `description:`, so no pass raises `skill_invalid`.
fn skill_body(name: &str) -> String {
    format!("---\nname: {name}\ndescription: {name} skill.\n---\n{name} does things.\n")
}

/// A lived-in home under `root` (`HOME`, which `Setup::fiber` pins to
/// `setup.root`): `shadowed` Fiber-home / `~/.agents` shadow pairs with
/// valid headers, the already-read import shape, one `~/.claude`-only
/// skill, 25 `~/.agents`-only filler skills, and the person's instruction
/// file, flat command and settings file.
pub(crate) fn lived_home(root: &Path, shadowed: usize) {
    let home = root.join("h");
    // debt: the shadow-pair loop stays duplicated with the ask.rs and
    // terminal.rs inline loops until those tests build on this fixture,
    // when those loops call lived_home with the count their assertions pin.
    for index in 0..shadowed {
        let name = format!("dup{index:02}");
        let body = skill_body(&name);
        write(&home, &format!("skills/{name}/SKILL.md"), &body);
        write(root, &format!(".agents/skills/{name}/SKILL.md"), &body);
    }
    // debt: the already-read shape stays duplicated with import.rs's
    // already_read_home until that file's tests build on this fixture,
    // when the inline writer there calls lived_home with the count its
    // assertions pin.
    write(
        root,
        ".agents/skills/review/SKILL.md",
        "---\nname: review\ndescription: Reviews code.\n---\n\nBody.\n",
    );
    fs::create_dir_all(root.join(".claude/skills")).unwrap();
    std::os::unix::fs::symlink(
        root.join(".agents/skills/review"),
        root.join(".claude/skills/review"),
    )
    .unwrap();
    write(
        root,
        ".claude/skills/mine/SKILL.md",
        "---\nname: review\ndescription: Source copy.\n---\n\nBody.\n",
    );
    write(
        root,
        ".claude/skills/lint/SKILL.md",
        "---\nname: lint\ndescription: Lints.\n---\n\nLint body.\n",
    );
    write(root, ".claude/skills/solo/SKILL.md", &skill_body("solo"));
    // Single-location skills raise no notice, and the `claude-code`
    // import ignores them: they only make the home substantial.
    for index in 0..25 {
        let name = format!("agent{index:02}");
        write(
            root,
            &format!(".agents/skills/{name}/SKILL.md"),
            &skill_body(&name),
        );
    }
    write(root, ".claude/CLAUDE.md", "Be kind.\n");
    write(root, ".claude/commands/greet.md", "Hello there.\n");
    write(
        root,
        ".claude/settings.json",
        &json!({
            "model": "opus",
            "mcpServers": {
                "notes": {"command": "notes-server", "args": ["--fast"]},
                "remote": {"url": "https://mcp.example.com/x"},
            },
        })
        .to_string(),
    );
}

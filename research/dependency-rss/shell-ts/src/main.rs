//! Probe of the tree-sitter-bash shell-parser candidate
//! (research/shell-parser-cost). Standalone: tree-sitter 0.27 and
//! arborium-tree-sitter both declare links = "tree-sitter", so this crate
//! cannot be a feature of the main dependency-rss probe. The workload is
//! identical to the brush-parser feature there: parse ten shell commands
//! and walk every node of each tree, counting nodes.
use std::hint::black_box;

/// The ten shell commands both shell-parser candidates parse and walk.
const SHELL_COMMANDS: &[&str] = &[
    "ls -la",
    "git status --short",
    "grep -rn \"foo bar\" src/",
    "cd crates/tui && git log --oneline -20",
    "cat README.md | head -50",
    "FOO=1 cargo test -p tui 2>&1 | tail -20",
    "find . -name '*.rs' -exec wc -l {} +",
    "echo \"$(git rev-parse HEAD)\"",
    "(cd docs; rg -n 'cap' *.md) > /dev/null",
    "for f in *.md; do wc -l \"$f\"; done",
];

fn main() {
    // Fiber's shell recogniser would parse a command and accept only an
    // allow-list of node kinds. Workload: parse each command and walk
    // every node of its tree, counting nodes.
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .unwrap();
    let dump = std::env::var("SHELL_DUMP").is_ok();
    let mut pass = |dump: bool| -> usize {
        let mut total = 0usize;
        for cmd in SHELL_COMMANDS {
            let tree = parser.parse(cmd, None).unwrap();
            let root = tree.root_node();
            if dump {
                let mut kinds = std::collections::BTreeMap::new();
                let mut stack = vec![root];
                while let Some(node) = stack.pop() {
                    *kinds.entry(node.kind().to_string()).or_insert(0usize) += 1;
                    let mut cursor = node.walk();
                    for child in node.children(&mut cursor) {
                        stack.push(child);
                    }
                }
                let kinds = kinds
                    .iter()
                    .map(|(k, n)| format!("{k}x{n}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                println!("cmd={cmd:?} error={} {kinds}", root.has_error());
            }
            let mut stack = vec![root];
            while let Some(node) = stack.pop() {
                total += 1;
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    stack.push(child);
                }
            }
        }
        total
    };
    pass(false); // warm-up
    let start = std::time::Instant::now();
    let total = pass(dump);
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    println!("nodes={total}");
    eprintln!("parse_walk_ms={ms:.3}");
    black_box(total);
}

# Fiber

Fiber is a coding agent for the terminal, written in Rust. It is built to use
little memory and to talk to many model providers: Anthropic, OpenAI and
ChatGPT/codex, Gemini, AWS Bedrock, Azure, OpenRouter, OpenCode and others
([`docs/model-routing.md`](docs/model-routing.md)).

**Status:** in development. There is no release, install script or docs
website yet. To try it, build it from source.

## Build from source

You need `rustup`. `rust-toolchain.toml` pins the Rust version, and `rustup`
installs it on the first build.

```sh
git clone https://github.com/aakshintala/fiber
cd fiber
cargo build --release -p main
./target/release/fiber --help
```

## Two ways in

- `fiber` opens the terminal UI.
- `fiber ask "<prompt>"` runs one prompt with no UI and prints the event
  stream, for scripts and other agents.

Both reach the same sessions through one background process, the hub
([`docs/invocation.md`](docs/invocation.md)).

## Docs

- Using Fiber: https://aakshintala.com/fiber/ (source in docs/user/)
- How Fiber works, one page per area: [`docs/`](docs/). Start with
  [`docs/architecture.md`](docs/architecture.md).
- Vocabulary: [`GLOSSARY.md`](GLOSSARY.md).
- Decisions and their reasons: [`docs/adr/`](docs/adr/).
- Memory and speed budgets: [`docs/performance.md`](docs/performance.md).
- Contributing, for people and agents: [`AGENTS.md`](AGENTS.md) and
  [`docs/workflow.md`](docs/workflow.md).

## License

MIT. See [`LICENSE`](LICENSE).

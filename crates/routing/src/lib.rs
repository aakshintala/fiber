//! Model routing: picks the model for a session or a switch, looks up its
//! credential, starts its Lua provider, builds its `Provider`, and finds
//! the reviewer (`docs/architecture.md`, module table, `routing` row).
//! The one module that uses `config`, `extensions` and `provider`
//! together. `main` calls it at startup and when a session switches models.

# LazyMerge – notes for Claude

Folder and file compare/merge tool with two front ends: a Rust terminal app (`src/`) and a
self-contained browser app (`lazymerge.html`, no build step). [README.md](README.md) describes usage;
[CHANGELOG.md](CHANGELOG.md) lists changes per version.

## Code map

- `src/app.rs`: state, key handling (including two-character sequences such as `gg`, `dh`, `dl`).
- `src/ui.rs`: ratatui rendering and the help screen.
- `src/model.rs`: decoding, diff alignment and staged edits with undo. It never writes files.
- `src/storage.rs`: folder scan, save and copy (atomic replace, checks for external changes).
- `src/config.rs`: Lua config and key bindings. `config.example.lua` is embedded as the default
  config, and a test requires it to equal `Config::default()`.
- `install.sh`, `install.ps1`: installers; `tests/`: installer tests and a PTY smoke test.

## Checks (as in `.github/workflows/release.yml`)

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all --check
python3 tests/test_install.py                       # Linux/macOS
cargo build --locked && python3 tests/pty_smoke.py  # Linux/macOS, needs a real PTY
pwsh ./tests/test_install.ps1                       # Windows
```

## Conventions

- Keys follow vim: `h` = left, `l` = right (`H` / `L` merge, `Alt+h` / `Alt+l`, `dh` / `dl`). A key
  change touches `default_keys()` in `config.rs`, `config.example.lua`, the help in `ui.rs` and
  the key table in the README.
- Edits and merges are staged in `model::Session` until saved; every change must be undoable with `u`.
- The terminal app follows the browser app's look (Kanagawa colours, hatched gaps).
- Working copies use CRLF (`core.autocrlf`); keep line endings when editing files.
- Commit messages: one line, conventional and lowercase, e.g. `feat: ...`, `fix: ...`.

## Release

1. Raise the version in `Cargo.toml`, `lazymerge.html` (`>vX.Y.Z</span>`) and the README (current
   version, `LAZYMERGE_VERSION` example, tag example), then run `cargo build` so that `Cargo.lock`
   follows; CI uses `--locked`.
2. Add the CHANGELOG entry.
3. Pushing the tag `vX.Y.Z` runs the release workflow. It checks that the tag, Cargo and HTML
   versions match, and it publishes only when all six builds pass.

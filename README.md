# bin

`bin` is a macOS terminal TUI for any dev who has lots of scattered executables and scripts, and would like a quick way to find them and have them in your path via a common bin folder.

## Install

Install from this checkout with Rust 1.74 or newer:

```sh
cargo install --path . --locked
```

Make sure both Cargo’s binary directory and the directory managed by `bin` are on your `PATH`, for example:

```sh
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
```

Then launch the TUI:

```sh
bin
```

Run `bin --help` to see the non-interactive commands for searching, adding, listing, enabling, disabling, renaming, and removing registrations.

## Configuration

`bin` reads `$XDG_CONFIG_HOME/bintui/config.toml`, falling back to `~/.config/bintui/config.toml`. The file is optional; without it, managed links are written to `$XDG_BIN_HOME` or `~/.local/bin` when `XDG_BIN_HOME` is unset.

Example ~/.config/bintui/config.toml

```toml
version = 1
bin_dir = "~/.local/bin"
ignore = [
  ".git",
  "target/",
  "**/node_modules/",
]

[roots]
work = "~/Developer"
tools = "$HOME/Tools"
```

- `version` must be `1`.
- `bin_dir` override where managed links are published, taking precedence over `XDG_BIN_HOME`.
- `ignore` contains gitignore-style patterns excluded during executable discovery.
- `roots` as a convenience, assign short display names to absolute directory paths; paths beneath them are shortened, for example, `[work]/project/script`.

Configured paths must resolve to absolute paths. They may start with `~/` and may reference environment variables as `$NAME` or `${NAME}`.

When a bintui file under `$XDG_CONFIG_HOME` (or the `~/.config` fallback) resolves inside a Git worktree—including through a symlink—every registry, discovery-ignore, or configuration file change made by `bin` is committed as `Update bintui configuration` and pushed to the current branch's configured upstream. Only the file changed by `bin` is included; other staged or unstaged changes are left alone. A commit or push failure is reported as an operation failure after the local configuration change has been written.

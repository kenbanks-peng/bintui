# bin

`bin` is a macOS terminal UI for finding executable files in local projects and publishing them under stable command names. It manages those names as symbolic links in `$XDG_BIN_HOME` or `~/.local/bin`.

## Install

Install from this checkout with Rust 1.74 or newer:

```sh
cargo install --path . --locked
```

Make sure both Cargo’s binary directory and the directory managed by `bin` are on your `PATH`:

```sh
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
```

Then launch the TUI:

```sh
bin
```

Run `bin --help` to see the non-interactive commands for searching, adding, listing, enabling, disabling, renaming, and removing registrations.

# bin

`bin` is a macOS terminal TUI for any dev who has lots of scattered executables and scripts, and would like a quick way to find them and have them in your path via a common bin folder.

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

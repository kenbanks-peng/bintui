#[cfg(target_os = "macos")]
use std::fs;
#[cfg(target_os = "macos")]
use std::io::Write;
#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "macos")]
use std::process::{Command as ProcessCommand, Stdio};

use assert_cmd::Command;
use predicates::prelude::*;
#[cfg(target_os = "macos")]
use tempfile::TempDir;

#[test]
fn version_flag_prints_package_version_without_tui() {
    Command::cargo_bin("bin")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(format!("bin {}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_subcommand_lists_commands_without_tui() {
    Command::cargo_bin("bin")
        .unwrap()
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Commands:"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("\n  tui").not());

    Command::cargo_bin("bin")
        .unwrap()
        .arg("tui")
        .assert()
        .code(2);
}

#[cfg(target_os = "macos")]
#[test]
fn tui_preserves_missing_managed_links_and_restores_a_real_pseudo_terminal() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let registry = temp.path().join(".config/bintui/registry.toml");
    fs::create_dir_all(registry.parent().unwrap()).unwrap();
    fs::write(
        registry,
        format!(
            "version = 1\n[[command]]\nname = \"tool\"\ntarget = {:?}\nenabled = true\n",
            target
        ),
    )
    .unwrap();
    let binary = assert_cmd::cargo::cargo_bin("bin");
    let mut child = ProcessCommand::new("/usr/bin/script")
        .args(["-q", "/dev/null"])
        .arg(binary)
        .current_dir(temp.path())
        .env_clear()
        .env("HOME", temp.path())
        .env("TERM", "xterm-256color")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"q").unwrap();

    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "PTY session failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output
        .stdout
        .windows(8)
        .any(|bytes| bytes == b"\x1b[?1049h"));
    assert!(output
        .stdout
        .windows(8)
        .any(|bytes| bytes == b"\x1b[?1049l"));
    assert!(fs::symlink_metadata(temp.path().join(".local/bin/tool")).is_err());
}

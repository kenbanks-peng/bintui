#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn command(home: &Path) -> Command {
    let mut command = Command::cargo_bin("bin").unwrap();
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home);
    command
}

#[test]
fn rename_exposes_structured_outcome_and_path_guidance() {
    let temp = TempDir::new().unwrap();
    let original = temp.path().join("project/tool");
    executable(&original);
    command(temp.path())
        .args(["add", original.to_str().unwrap(), "--name", "old"])
        .assert()
        .success();

    command(temp.path())
        .args(["rename", "old", "new", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "\"identifier\": \"registration-renamed\"",
        ))
        .stdout(predicate::str::contains("\"name\": \"new\""))
        .stdout(predicate::str::contains("\"path_diagnostic\""))
        .stdout(predicate::str::contains("\"managed-bin-not-on-path\""));
}

#[test]
fn rename_failures_are_actionable_and_keep_exit_status_contract() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("tool");
    executable(&target);
    command(temp.path())
        .args(["add", target.to_str().unwrap(), "--name", "old"])
        .assert()
        .success();
    fs::write(temp.path().join(".local/bin/new"), "keep").unwrap();

    command(temp.path())
        .args(["rename", "old", "new"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("rename-destination-conflict"))
        .stdout(predicate::str::contains("remove or rename"));
    assert_eq!(
        fs::read_to_string(temp.path().join(".local/bin/new")).unwrap(),
        "keep"
    );
}

#[test]
fn removed_maintenance_commands_are_not_in_the_cli() {
    let temp = TempDir::new().unwrap();

    command(temp.path())
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("find").not())
        .stdout(predicate::str::contains("relocate").not())
        .stdout(predicate::str::contains("repair").not());

    for operation in ["find", "relocate", "repair"] {
        command(temp.path()).arg(operation).assert().code(2);
    }
}

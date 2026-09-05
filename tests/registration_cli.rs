#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use assert_cmd::{assert::OutputAssertExt, Command};
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\nprintf invoked").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn command(home: &Path, cwd: &Path) -> Command {
    let mut command = Command::cargo_bin("bin").unwrap();
    command.env_clear().env("HOME", home).current_dir(cwd);
    command
}

#[test]
fn json_add_exposes_stable_identifiers_and_resulting_state() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);

    let output = command(temp.path(), temp.path())
        .args([
            "add",
            "--name",
            "work",
            "--format",
            "json",
            target.to_str().unwrap(),
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["identifier"], "registration-added");
    assert!(json["registration"].get("defect").is_none());

    let unrelated = temp.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    std::process::Command::new(temp.path().join(".local/bin/work"))
        .current_dir(unrelated)
        .assert()
        .success()
        .stdout("invoked");
}

#[test]
fn lifecycle_commands_and_list_present_the_shared_application_results() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);

    command(temp.path(), temp.path())
        .args(["add", "--disabled", target.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("registration-added"));
    command(temp.path(), temp.path())
        .args(["enable", "tool"])
        .assert()
        .success();
    command(temp.path(), temp.path())
        .args(["list", "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tool -> "))
        .stdout(predicate::str::contains("enabled").not())
        .stdout(predicate::str::contains("no defects").not())
        .stdout(predicate::str::contains("disabled").not());
    command(temp.path(), temp.path())
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("registrations-listed"));
    command(temp.path(), temp.path())
        .args(["disable", "tool"])
        .assert()
        .success();
    command(temp.path(), temp.path())
        .args(["list", "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("tool -> "))
        .stdout(predicate::str::contains("[disabled]"))
        .stdout(predicate::str::contains("no defects").not());
    command(temp.path(), temp.path())
        .args(["remove", "tool"])
        .assert()
        .success();
}

#[test]
fn list_restores_missing_links_and_presents_remaining_defects_with_the_exit_status_contract() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    command(temp.path(), temp.path())
        .args(["add", target.to_str().unwrap()])
        .assert()
        .success();

    command(temp.path(), temp.path())
        .args(["list", "--format", "json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"defect\"").not());

    let managed_link = temp.path().join(".local/bin/tool");
    fs::remove_file(&managed_link).unwrap();
    command(temp.path(), temp.path())
        .args(["list", "--format", "text"])
        .assert()
        .success()
        .stdout(predicate::str::contains("link-missing").not());
    assert_eq!(fs::read_link(managed_link).unwrap(), target);

    fs::remove_file(&target).unwrap();
    command(temp.path(), temp.path())
        .args(["list", "--format", "text"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("target-missing"));
}

#[test]
fn simultaneous_cli_processes_serialize_registry_writes_without_lost_updates() {
    let temp = TempDir::new().unwrap();
    let binary = assert_cmd::cargo::cargo_bin("bin");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut writers = Vec::new();
    for index in 0..8 {
        let target = temp.path().join(format!("project/tool-{index}"));
        executable(&target);
        let home = temp.path().to_path_buf();
        let binary = binary.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        writers.push(std::thread::spawn(move || {
            barrier.wait();
            std::process::Command::new(binary)
                .env_clear()
                .env("HOME", &home)
                .current_dir(&home)
                .args(["add", "--disabled"])
                .arg(target)
                .output()
                .unwrap()
        }));
    }

    for writer in writers {
        let output = writer.join().unwrap();
        assert!(
            output.status.success(),
            "writer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let output = command(temp.path(), temp.path())
        .args(["list", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    let registrations = json["registrations"].as_array().unwrap();
    assert_eq!(registrations.len(), 8);
    for index in 0..8 {
        assert!(registrations
            .iter()
            .any(|state| { state["registration"]["name"] == format!("tool-{index}") }));
    }
}

#[test]
fn blocked_and_invalid_lifecycle_requests_use_the_exit_status_contract() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let managed = temp.path().join(".local/bin/tool");
    executable(&managed);

    command(temp.path(), temp.path())
        .args(["add", target.to_str().unwrap()])
        .assert()
        .code(1)
        .stdout(predicate::str::contains("managed-path-occupied"));
    command(temp.path(), temp.path())
        .args(["disable", "missing", "--format", "json"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("disable-failed"));
}

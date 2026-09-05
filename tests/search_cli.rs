#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

fn executable(path: &std::path::Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn command(home: &std::path::Path, cwd: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("bin").unwrap();
    command.env_clear().env("HOME", home).current_dir(cwd);
    command
}

#[test]
fn text_search_prints_only_the_target_and_abbreviates_home_paths() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    executable(&project.join("tool"));

    command(temp.path(), &project)
        .args(["search", "--format", "text"])
        .assert()
        .success()
        .stdout("~/project/tool\n");
}

#[test]
fn configured_roots_shorten_text_paths_but_not_json_paths() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    executable(&project.join("nested/tool"));
    let config = temp.path().join(".config/bintui");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        "version = 1\n[roots]\nproject = \"~/project\"\n",
    )
    .unwrap();

    command(temp.path(), temp.path())
        .args(["search", project.to_str().unwrap(), "--format", "text"])
        .assert()
        .success()
        .stdout("[project]/nested/tool\n");

    let output = command(temp.path(), temp.path())
        .args(["search", project.to_str().unwrap(), "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert!(json["search_root"].as_str().unwrap().starts_with('/'));
    assert!(json["candidates"][0]["target"]
        .as_str()
        .unwrap()
        .starts_with('/'));
}

#[test]
fn json_search_exposes_stable_status_and_conflict_identifiers() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    executable(&project.join("one/tool"));
    executable(&project.join("two/tool"));

    let output = command(temp.path(), temp.path())
        .args(["search", project.to_str().unwrap(), "--format", "json"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "blocked");
    assert_eq!(
        json["candidates"][0]["conflict"]["kind"],
        "duplicate-proposed-name"
    );
    assert!(json["search_root"].as_str().unwrap().starts_with('/'));
}

#[test]
fn json_search_reports_structured_warnings_without_failing_the_search() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("private")).unwrap();
    fs::set_permissions(project.join("private"), fs::Permissions::from_mode(0o000)).unwrap();

    let output = command(temp.path(), temp.path())
        .args(["search", project.to_str().unwrap(), "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    fs::set_permissions(project.join("private"), fs::Permissions::from_mode(0o700)).unwrap();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "healthy");
    assert_eq!(json["warnings"][0]["kind"], "unreadable-directory");
}

#[test]
fn ignore_persists_the_target_and_removes_it_from_discovery() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let target = project.join("tool");
    executable(&target);

    let output = command(temp.path(), temp.path())
        .args(["ignore", target.to_str().unwrap(), "--format", "json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "healthy");
    assert_eq!(json["identifier"], "target-ignored");

    command(temp.path(), temp.path())
        .args(["search", project.to_str().unwrap(), "--format", "text"])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn text_search_omits_registered_targets() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let registered = project.join("registered");
    let available = project.join("available");
    executable(&registered);
    executable(&available);

    command(temp.path(), temp.path())
        .args(["add", registered.to_str().unwrap(), "--format", "json"])
        .assert()
        .success();

    command(temp.path(), temp.path())
        .args(["search", project.to_str().unwrap(), "--format", "text"])
        .assert()
        .success()
        .stdout(format!("{}\n", available.display()));
}

#[test]
fn invalid_input_has_exit_status_two_and_an_actionable_message() {
    let temp = TempDir::new().unwrap();
    let missing = temp.path().join("missing");

    command(temp.path(), temp.path())
        .args(["search", missing.to_str().unwrap()])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("could not search"));
}

#[test]
fn json_search_errors_have_stable_identifiers() {
    let temp = TempDir::new().unwrap();
    let config = temp.path().join(".config/bintui");
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("config.toml"), "version = 99\n").unwrap();

    let output = command(temp.path(), temp.path())
        .args(["search", "--format", "json"])
        .assert()
        .code(2)
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["status"], "error");
    assert_eq!(json["error"]["kind"], "search-failed");
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("version 99"));
}

#[test]
fn missing_home_is_reported_as_an_operational_failure() {
    let temp = TempDir::new().unwrap();
    let mut command = Command::cargo_bin("bin").unwrap();
    command
        .env_clear()
        .current_dir(temp.path())
        .arg("search")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("HOME"));
}

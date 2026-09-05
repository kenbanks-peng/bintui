#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;
use std::process::Command;

use bintui::application::{add, ignore_target, AddRequest};
use bintui::config_git;
use bintui::environment::Environment;
use tempfile::TempDir;

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        arguments,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn environment(root: &Path, config_home: &Path) -> Environment {
    Environment::from_values(
        root.to_path_buf(),
        root.to_path_buf(),
        BTreeMap::from([("XDG_CONFIG_HOME".into(), config_home.as_os_str().to_owned())]),
    )
}

fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn bintui_configuration_mutations_commit_and_push_only_the_changed_file() {
    let temp = TempDir::new().unwrap();
    let config_home = temp.path().join("config");
    let remote = temp.path().join("remote.git");
    fs::create_dir_all(config_home.join("bintui")).unwrap();
    fs::write(config_home.join("bintui/config.toml"), "version = 1\n").unwrap();
    fs::write(config_home.join("notes.txt"), "initial\n").unwrap();

    git(&config_home, &["init", "-b", "main"]);
    git(&config_home, &["config", "user.name", "Bin TUI Test"]);
    git(
        &config_home,
        &["config", "user.email", "bin-tui@example.test"],
    );
    git(&config_home, &["add", "."]);
    git(&config_home, &["commit", "-m", "Initial configuration"]);
    let output = Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .arg(&remote)
        .output()
        .unwrap();
    assert!(output.status.success());
    git(
        &config_home,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&config_home, &["push", "-u", "origin", "main"]);

    fs::write(config_home.join("notes.txt"), "user change\n").unwrap();
    git(&config_home, &["add", "notes.txt"]);

    let environment = environment(temp.path(), &config_home);
    let target = temp.path().join("project/tool");
    executable(&target);
    add(
        AddRequest {
            target,
            name: Some("tool".to_owned()),
            disabled: true,
        },
        &environment,
    )
    .unwrap();

    let ignored = temp.path().join("ignored");
    fs::create_dir_all(&ignored).unwrap();
    ignore_target(&ignored, &environment).unwrap();

    fs::write(
        config_home.join("bintui/config.toml"),
        "version = 1\nignore = [\"target/\"]\n",
    )
    .unwrap();
    config_git::commit_and_push(&config_home.join("bintui/config.toml"), &environment).unwrap();

    let subjects = git(&config_home, &["log", "-3", "--format=%s"]);
    assert_eq!(
        subjects.lines().collect::<Vec<_>>(),
        vec![
            "Update bintui configuration",
            "Update bintui configuration",
            "Update bintui configuration"
        ]
    );
    assert_eq!(
        git(&config_home, &["diff", "--cached", "--name-only"]).trim(),
        "notes.txt"
    );
    assert_eq!(
        git(&config_home, &["show", "--format=", "--name-only", "HEAD"]).trim(),
        "bintui/config.toml"
    );
    assert_eq!(
        git(&config_home, &["rev-parse", "HEAD"]).trim(),
        git(&config_home, &["rev-parse", "origin/main"]).trim()
    );
    assert!(
        git(&config_home, &["show", "origin/main:bintui/registry.toml"])
            .contains("name = \"tool\"")
    );
    assert!(
        git(&config_home, &["show", "origin/main:bintui/ignore.toml"])
            .contains(ignored.to_str().unwrap())
    );
}

#[test]
fn config_home_inside_a_git_worktree_is_committed_and_pushed() {
    let temp = TempDir::new().unwrap();
    let worktree = temp.path().join("home");
    let config_home = worktree.join(".config");
    let remote = temp.path().join("remote.git");
    fs::create_dir_all(&config_home).unwrap();
    fs::write(worktree.join("README"), "initial\n").unwrap();

    git(&worktree, &["init", "-b", "main"]);
    git(&worktree, &["config", "user.name", "Bin TUI Test"]);
    git(&worktree, &["config", "user.email", "bin-tui@example.test"]);
    git(&worktree, &["add", "README"]);
    git(&worktree, &["commit", "-m", "Initial configuration"]);
    let output = Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .arg(&remote)
        .output()
        .unwrap();
    assert!(output.status.success());
    git(
        &worktree,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&worktree, &["push", "-u", "origin", "main"]);

    let environment = environment(&worktree, &config_home);
    let target = temp.path().join("project/tool");
    executable(&target);
    add(
        AddRequest {
            target,
            name: Some("tool".to_owned()),
            disabled: true,
        },
        &environment,
    )
    .unwrap();

    assert_eq!(
        git(&worktree, &["log", "-1", "--format=%s"]).trim(),
        "Update bintui configuration"
    );
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD"]).trim(),
        git(&worktree, &["rev-parse", "origin/main"]).trim()
    );
    assert!(git(
        &worktree,
        &["show", "origin/main:.config/bintui/registry.toml"]
    )
    .contains("name = \"tool\""));
}

#[test]
fn symlinked_bintui_directory_uses_the_physical_git_worktree() {
    let temp = TempDir::new().unwrap();
    let home = temp.path().join("home");
    let config_home = home.join(".config");
    let worktree = temp.path().join("dotfiles");
    let remote = temp.path().join("remote.git");
    fs::create_dir_all(&config_home).unwrap();
    fs::create_dir_all(worktree.join("bintui")).unwrap();
    symlink(worktree.join("bintui"), config_home.join("bintui")).unwrap();
    fs::write(worktree.join("README"), "initial\n").unwrap();

    git(&worktree, &["init", "-b", "main"]);
    git(&worktree, &["config", "user.name", "Bin TUI Test"]);
    git(&worktree, &["config", "user.email", "bin-tui@example.test"]);
    git(&worktree, &["add", "README"]);
    git(&worktree, &["commit", "-m", "Initial configuration"]);
    let output = Command::new("git")
        .args(["init", "--bare", "-b", "main"])
        .arg(&remote)
        .output()
        .unwrap();
    assert!(output.status.success());
    git(
        &worktree,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&worktree, &["push", "-u", "origin", "main"]);

    let environment = environment(&home, &config_home);
    let target = temp.path().join("project/tool");
    executable(&target);
    add(
        AddRequest {
            target,
            name: Some("tool".to_owned()),
            disabled: true,
        },
        &environment,
    )
    .unwrap();

    assert_eq!(
        git(&worktree, &["log", "-1", "--format=%s"]).trim(),
        "Update bintui configuration"
    );
    assert_eq!(
        git(&worktree, &["rev-parse", "HEAD"]).trim(),
        git(&worktree, &["rev-parse", "origin/main"]).trim()
    );
    assert!(
        git(&worktree, &["show", "origin/main:bintui/registry.toml"]).contains("name = \"tool\"")
    );
}

#[test]
fn a_push_failure_is_reported_after_the_registry_change_is_committed() {
    let temp = TempDir::new().unwrap();
    let config_home = temp.path().join("config");
    fs::create_dir_all(&config_home).unwrap();
    git(&config_home, &["init", "-b", "main"]);
    git(&config_home, &["config", "user.name", "Bin TUI Test"]);
    git(
        &config_home,
        &["config", "user.email", "bin-tui@example.test"],
    );

    let environment = environment(temp.path(), &config_home);
    let target = temp.path().join("project/tool");
    executable(&target);
    let error = add(
        AddRequest {
            target,
            name: Some("tool".to_owned()),
            disabled: true,
        },
        &environment,
    )
    .unwrap_err();

    assert_eq!(error.identifier(), "add-failed");
    assert!(error.to_string().contains("pushing the bintui change"));
    assert!(config_home.join("bintui/registry.toml").exists());
    assert_eq!(
        git(&config_home, &["log", "-1", "--format=%s"]).trim(),
        "Update bintui configuration"
    );
}

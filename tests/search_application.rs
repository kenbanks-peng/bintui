#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use bintui::application::{ignore_target, search, SearchRequest};
use bintui::environment::Environment;
use bintui::model::{
    ConflictKind, ManagedPathKind, RegistrationDefectKind, SearchStatus, WarningKind,
};
use tempfile::TempDir;

fn environment(home: &Path, cwd: &Path) -> Environment {
    Environment::from_values(home.to_path_buf(), cwd.to_path_buf(), BTreeMap::new())
}

fn file(path: &Path, mode: u32) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn search_finds_only_files_with_an_executable_bit_and_normalizes_targets() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("project/./nested/..");
    let project = temp.path().join("project");
    for (name, mode) in [("owner", 0o100), ("group", 0o010), ("other", 0o001)] {
        file(&project.join(name), mode);
    }
    file(&project.join("plain"), 0o644);

    let result = search(
        SearchRequest {
            search_root: Some(root),
        },
        &environment(temp.path(), temp.path()),
    )
    .unwrap();

    let names: Vec<_> = result
        .candidates
        .iter()
        .map(|item| item.proposed_name.as_str())
        .collect();
    assert_eq!(names, ["group", "other", "owner"]);
    assert!(result
        .candidates
        .iter()
        .all(|item| item.target.is_absolute()));
    assert_eq!(result.status, SearchStatus::Healthy);
}

#[test]
fn search_defaults_to_working_directory_and_never_mutates_it() {
    let temp = TempDir::new().unwrap();
    file(&temp.path().join("tool"), 0o755);
    let before = directory_snapshot(temp.path());

    let result = search(
        SearchRequest { search_root: None },
        &environment(temp.path(), temp.path()),
    )
    .unwrap();

    assert_eq!(result.search_root, temp.path());
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(directory_snapshot(temp.path()), before);
}

#[test]
fn search_includes_valid_file_links_but_does_not_follow_directory_links() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("project");
    let outside = temp.path().join("outside");
    file(&root.join("real-tool"), 0o755);
    file(&outside.join("hidden-tool"), 0o755);
    symlink(root.join("real-tool"), root.join("linked-tool")).unwrap();
    symlink(root.join("missing"), root.join("broken-tool")).unwrap();
    symlink(&outside, root.join("linked-directory")).unwrap();

    let result = search(
        SearchRequest {
            search_root: Some(root),
        },
        &environment(temp.path(), temp.path()),
    )
    .unwrap();

    let names: Vec<_> = result
        .candidates
        .iter()
        .map(|item| item.proposed_name.as_str())
        .collect();
    assert_eq!(names, ["linked-tool", "real-tool"]);
}

#[test]
fn search_applies_configured_and_managed_directory_exclusions() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("project");
    for directory in [".git", "node_modules", "target", "managed"] {
        file(&root.join(directory).join("tool"), 0o755);
    }
    file(&root.join("kept/tool"), 0o755);
    let config_dir = temp.path().join("config/bintui");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        format!(
            "version = 1\nbin_dir = {:?}\nignore = [\".git\", \"node_modules\", \"target\"]\n",
            root.join("managed")
        ),
    )
    .unwrap();
    let mut variables = BTreeMap::new();
    variables.insert(
        "XDG_CONFIG_HOME".into(),
        temp.path().join("config").into_os_string(),
    );

    let result = search(
        SearchRequest {
            search_root: Some(root),
        },
        &Environment::from_values(
            temp.path().to_path_buf(),
            temp.path().to_path_buf(),
            variables,
        ),
    )
    .unwrap();

    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].proposed_name, "tool");
    assert!(result.candidates[0].target.ends_with("kept/tool"));
}

#[test]
fn search_applies_gitignore_paths_at_any_depth_below_the_search_root() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("workspace");
    file(
        &root.join("tools/tui/bintui/target/release/build/ignored-tool"),
        0o755,
    );
    file(&root.join("target/release/build/ignored-root-tool"), 0o755);
    file(
        &root.join("tools/tui/bintui/target/release/package/kept-tool"),
        0o755,
    );
    file(
        &root.join("tools/tui/bintui/target/debug/build/debug-tool"),
        0o755,
    );
    let config_dir = temp.path().join("config/bintui");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        "version = 1\nignore = [\"target/release/build/\"]\n",
    )
    .unwrap();
    let variables = BTreeMap::from([(
        "XDG_CONFIG_HOME".into(),
        temp.path().join("config").into_os_string(),
    )]);

    let result = search(
        SearchRequest {
            search_root: Some(root),
        },
        &Environment::from_values(
            temp.path().to_path_buf(),
            temp.path().to_path_buf(),
            variables,
        ),
    )
    .unwrap();

    let names: Vec<_> = result
        .candidates
        .iter()
        .map(|candidate| candidate.proposed_name.as_str())
        .collect();
    assert_eq!(names, ["debug-tool", "kept-tool"]);
}

#[test]
fn ignored_full_path_is_persisted_separately_and_filtered_until_manually_removed() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("project");
    let ignored = root.join("nested/tool");
    let kept = root.join("other/tool");
    file(&ignored, 0o755);
    file(&kept, 0o755);
    let variables = BTreeMap::from([(
        "XDG_CONFIG_HOME".into(),
        temp.path().join("config").into_os_string(),
    )]);
    let environment = Environment::from_values(
        temp.path().to_path_buf(),
        temp.path().to_path_buf(),
        variables,
    );

    assert_eq!(
        ignore_target(&ignored, &environment).unwrap(),
        "target-ignored"
    );
    let ignore_path = temp.path().join("config/bintui/ignore.toml");
    let stored = fs::read_to_string(&ignore_path).unwrap();
    assert!(stored.contains("version = 1"));
    assert!(stored.contains(ignored.to_str().unwrap()));
    assert!(!temp.path().join("config/bintui/config.toml").exists());

    let filtered = search(
        SearchRequest {
            search_root: Some(root.clone()),
        },
        &environment,
    )
    .unwrap();
    assert_eq!(filtered.candidates.len(), 1);
    assert_eq!(filtered.candidates[0].target, kept);

    fs::write(ignore_path, "version = 1\npaths = []\n").unwrap();
    let restored = search(
        SearchRequest {
            search_root: Some(root),
        },
        &environment,
    )
    .unwrap();
    assert_eq!(restored.candidates.len(), 2);
}

#[test]
fn search_does_not_exclude_unconfigured_directory_names() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("project");
    file(&root.join(".git/git-tool"), 0o755);
    file(&root.join("node_modules/node-tool"), 0o755);

    let result = search(
        SearchRequest {
            search_root: Some(root),
        },
        &environment(temp.path(), temp.path()),
    )
    .unwrap();

    let names: Vec<_> = result
        .candidates
        .iter()
        .map(|candidate| candidate.proposed_name.as_str())
        .collect();
    assert_eq!(names, ["git-tool", "node-tool"]);
}

#[test]
fn search_associates_registrations_and_reports_name_conflicts() {
    let temp = TempDir::new().unwrap();
    let root = temp.path().join("project");
    file(&root.join("same"), 0o755);
    file(&root.join("other/same"), 0o755);
    let registry_dir = temp.path().join("config/bintui");
    fs::create_dir_all(&registry_dir).unwrap();
    fs::write(
        registry_dir.join("registry.toml"),
        format!(
            "version = 1\n[[command]]\nname = \"same\"\ntarget = {:?}\nenabled = true\n",
            root.join("same")
        ),
    )
    .unwrap();
    let mut variables = BTreeMap::new();
    variables.insert(
        "XDG_CONFIG_HOME".into(),
        temp.path().join("config").into_os_string(),
    );

    let result = search(
        SearchRequest {
            search_root: Some(root),
        },
        &Environment::from_values(
            temp.path().to_path_buf(),
            temp.path().to_path_buf(),
            variables,
        ),
    )
    .unwrap();

    let registered = result
        .candidates
        .iter()
        .find(|item| item.target.ends_with("project/same"))
        .unwrap();
    let state = registered.registration.as_ref().unwrap();
    assert_eq!(state.registration.name, "same");
    assert_eq!(
        state.defect.as_ref().map(|defect| &defect.kind),
        Some(&RegistrationDefectKind::LinkMissing)
    );
    assert_eq!(state.actual, ManagedPathKind::Missing);
    assert!(registered.conflict.is_none());
    let serialized = serde_json::to_value(registered).unwrap();
    assert_eq!(serialized["registration"]["name"], "same");
    assert!(serialized["registration"].get("registration").is_none());
    let duplicate = result
        .candidates
        .iter()
        .find(|item| item.target.ends_with("other/same"))
        .unwrap();
    assert_eq!(
        duplicate.conflict.as_ref().unwrap().kind,
        ConflictKind::CommandNameRegistered
    );
    assert_eq!(result.status, SearchStatus::Blocked);
}

#[test]
fn unmanaged_paths_in_the_managed_bin_directory_are_command_name_conflicts() {
    let temp = TempDir::new().unwrap();
    let project = temp.path().join("project");
    let managed = temp.path().join("managed");
    file(&project.join("tool"), 0o755);
    file(&managed.join("tool"), 0o755);
    let config = temp.path().join(".config/bintui");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        format!("version = 1\nbin_dir = {:?}\n", managed),
    )
    .unwrap();

    let result = search(
        SearchRequest {
            search_root: Some(project),
        },
        &environment(temp.path(), temp.path()),
    )
    .unwrap();

    assert_eq!(result.status, SearchStatus::Blocked);
    assert_eq!(
        result.candidates[0].conflict.as_ref().unwrap().kind,
        ConflictKind::ManagedPathOccupied
    );
}

#[test]
fn duplicate_proposed_names_are_reported_without_an_existing_registry() {
    let temp = TempDir::new().unwrap();
    file(&temp.path().join("one/tool"), 0o755);
    file(&temp.path().join("two/tool"), 0o755);

    let result = search(
        SearchRequest {
            search_root: Some(temp.path().to_path_buf()),
        },
        &environment(temp.path(), temp.path()),
    )
    .unwrap();

    assert_eq!(result.status, SearchStatus::Blocked);
    assert!(result.candidates.iter().all(|candidate| {
        candidate.conflict.as_ref().map(|conflict| &conflict.kind)
            == Some(&ConflictKind::DuplicateProposedName)
    }));
}

#[test]
fn unreadable_directories_are_warnings_and_other_candidates_are_returned() {
    let temp = TempDir::new().unwrap();
    file(&temp.path().join("visible"), 0o755);
    let unreadable = temp.path().join("private");
    fs::create_dir(&unreadable).unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

    let result = search(
        SearchRequest {
            search_root: Some(temp.path().to_path_buf()),
        },
        &environment(temp.path(), temp.path()),
    )
    .unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(result.candidates.len(), 1);
    assert!(result.warnings.iter().any(|warning| {
        warning.kind == WarningKind::UnreadableDirectory && warning.path == unreadable
    }));
}

#[test]
fn searching_the_managed_bin_directory_returns_no_candidates() {
    let temp = TempDir::new().unwrap();
    let managed = temp.path().join("managed");
    file(&managed.join("tool"), 0o755);
    let config = temp.path().join(".config/bintui");
    fs::create_dir_all(&config).unwrap();
    fs::write(
        config.join("config.toml"),
        format!("version = 1\nbin_dir = {:?}\n", managed),
    )
    .unwrap();

    let result = search(
        SearchRequest {
            search_root: Some(managed),
        },
        &environment(temp.path(), temp.path()),
    )
    .unwrap();

    assert!(result.candidates.is_empty());
}

fn directory_snapshot(root: &Path) -> Vec<PathBuf> {
    fn visit(path: &Path, paths: &mut Vec<PathBuf>) {
        let mut children: Vec<_> = fs::read_dir(path)
            .unwrap()
            .map(|item| item.unwrap().path())
            .collect();
        children.sort();
        for child in children {
            paths.push(child.clone());
            if child.is_dir() {
                visit(&child, paths);
            }
        }
    }
    let mut paths = Vec::new();
    visit(root, &mut paths);
    paths
}

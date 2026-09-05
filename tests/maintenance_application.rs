#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::Path;

use bintui::application::{add, path_diagnostic, rename, AddRequest, Application};
use bintui::environment::Environment;
use bintui::model::{LifecycleStatus, MutationBoundary, MutationFaultInjector, PathStatus};
use tempfile::TempDir;

fn environment(root: &Path, path: Option<&str>) -> Environment {
    let mut variables = BTreeMap::from([(
        "XDG_CONFIG_HOME".into(),
        root.join("config").into_os_string(),
    )]);
    if let Some(path) = path {
        variables.insert("PATH".into(), path.into());
    }
    Environment::from_values(root.to_path_buf(), root.to_path_buf(), variables)
}

fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[derive(Clone, Copy)]
struct FailAt(MutationBoundary);

impl MutationFaultInjector for FailAt {
    fn check(&self, boundary: MutationBoundary) -> std::io::Result<()> {
        if boundary == self.0 {
            Err(std::io::Error::other("injected maintenance failure"))
        } else {
            Ok(())
        }
    }
}

fn register(root: &Path, name: &str, target: &Path, disabled: bool) {
    add(
        AddRequest {
            target: target.to_path_buf(),
            name: Some(name.to_owned()),
            disabled,
        },
        &environment(root, None),
    )
    .unwrap();
}

#[test]
fn rename_preserves_target_and_enabled_state_while_moving_only_the_owned_link() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    register(temp.path(), "old", &target, false);

    let result = rename("old", "new", &environment(temp.path(), None)).unwrap();

    assert_eq!(result.status, LifecycleStatus::Healthy);
    assert_eq!(result.identifier, "registration-renamed");
    let state = result.registration.unwrap();
    assert_eq!(state.registration.name, "new");
    assert_eq!(state.registration.target, target);
    assert!(state.registration.enabled);
    assert!(state.defect.is_none());
    assert!(fs::symlink_metadata(temp.path().join(".local/bin/old")).is_err());
    assert_eq!(
        fs::read_link(temp.path().join(".local/bin/new")).unwrap(),
        state.registration.target
    );
}

#[test]
fn enabled_rename_reconciles_when_the_old_managed_link_is_missing() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    register(temp.path(), "old", &target, false);
    fs::remove_file(temp.path().join(".local/bin/old")).unwrap();

    let result = rename("old", "new", &environment(temp.path(), None)).unwrap();

    assert_eq!(result.status, LifecycleStatus::Healthy);
    assert_eq!(
        fs::read_link(temp.path().join(".local/bin/new")).unwrap(),
        target
    );
}

#[test]
fn rename_blocks_destination_and_changed_source_without_deleting_them() {
    for changed_source in [false, true] {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("project/tool");
        let other = temp.path().join("project/other");
        executable(&target);
        executable(&other);
        register(temp.path(), "old", &target, false);
        let old = temp.path().join(".local/bin/old");
        let new = temp.path().join(".local/bin/new");
        if changed_source {
            fs::remove_file(&old).unwrap();
            symlink(&other, &old).unwrap();
        } else {
            fs::write(&new, "keep me").unwrap();
        }

        let result = rename("old", "new", &environment(temp.path(), None)).unwrap();

        assert_eq!(result.status, LifecycleStatus::Blocked);
        assert_eq!(
            fs::read_link(&old).ok(),
            Some(if changed_source { other } else { target })
        );
        if !changed_source {
            assert_eq!(fs::read_to_string(new).unwrap(), "keep me");
        }
    }
}

#[test]
fn disabled_rename_changes_only_desired_state_and_no_ops_are_safe() {
    let temp = TempDir::new().unwrap();
    let old = temp.path().join("old/tool");
    executable(&old);
    register(temp.path(), "old-name", &old, true);

    let renamed = rename("old-name", "new-name", &environment(temp.path(), None)).unwrap();
    assert!(!renamed.registration.as_ref().unwrap().registration.enabled);
    assert!(fs::symlink_metadata(temp.path().join(".local/bin/old-name")).is_err());
    assert!(fs::symlink_metadata(temp.path().join(".local/bin/new-name")).is_err());
    let same_name = rename("new-name", "new-name", &environment(temp.path(), None)).unwrap();
    assert_eq!(same_name.status, LifecycleStatus::Healthy);
}

#[test]
fn disabled_rename_leaves_an_unrelated_old_path_entry_untouched() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    register(temp.path(), "old", &target, true);
    let old_path = temp.path().join(".local/bin/old");
    fs::create_dir_all(old_path.parent().unwrap()).unwrap();
    fs::write(&old_path, "unrelated").unwrap();

    let result = rename("old", "new", &environment(temp.path(), None)).unwrap();

    assert_eq!(result.status, LifecycleStatus::Healthy);
    let state = result.registration.unwrap();
    assert_eq!(state.registration.name, "new");
    assert!(!state.registration.enabled);
    assert!(state.defect.is_none());
    assert_eq!(fs::read_to_string(&old_path).unwrap(), "unrelated");
    assert!(fs::symlink_metadata(temp.path().join(".local/bin/new")).is_err());
}

#[test]
fn rename_failures_restore_the_original_registration_and_report_both_paths() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    register(temp.path(), "old", &target, false);
    let environment = environment(temp.path(), None);

    let error = Application::with_faults(
        &environment,
        &FailAt(MutationBoundary::AtomicRegistryReplacement),
    )
    .rename("old", "new")
    .unwrap_err();
    assert_eq!(
        error.resulting_registration().unwrap().registration.name,
        "old"
    );
    assert_eq!(
        fs::read_link(temp.path().join(".local/bin/old")).unwrap(),
        target
    );
    assert!(fs::symlink_metadata(temp.path().join(".local/bin/new")).is_err());

    let error =
        Application::with_faults(&environment, &FailAt(MutationBoundary::ManagedLinkRemoval))
            .rename("old", "new")
            .unwrap_err();
    assert_eq!(
        error.resulting_registration().unwrap().registration.name,
        "old"
    );
    assert!(error.to_string().contains("actual paths after failure"));
    assert_eq!(
        fs::read_link(temp.path().join(".local/bin/old")).unwrap(),
        target
    );
    assert!(fs::symlink_metadata(temp.path().join(".local/bin/new")).is_err());
}

#[test]
fn path_diagnostic_matches_complete_normalized_components_and_never_changes_path() {
    let temp = TempDir::new().unwrap();
    let bin = temp.path().join(".local/bin");
    let misleading = format!("{}-backup:/usr/bin", bin.display());
    let absent = environment(temp.path(), Some(&misleading));
    assert_eq!(
        path_diagnostic(&absent).unwrap().status,
        PathStatus::Missing
    );

    let exact = format!("/usr/bin:{}/../bin:{}", bin.display(), bin.display());
    let present = environment(temp.path(), Some(&exact));
    let result = path_diagnostic(&present).unwrap();
    assert_eq!(result.status, PathStatus::Present);
    assert_eq!(result.matches, 2);
    assert_eq!(present.variable("PATH").unwrap(), exact.as_str());
    assert!(result.guidance.contains(&bin.display().to_string()));
}

#![cfg(unix)]

use bintui::application::{
    add, disable, enable, list, remove, validate_add, AddRequest, Application,
};
use bintui::environment::Environment;
use bintui::model::{LifecycleStatus, MutationBoundary, MutationFaultInjector};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn environment(root: &Path) -> Environment {
    Environment::from_values(
        root.to_path_buf(),
        root.to_path_buf(),
        BTreeMap::from([(
            "XDG_CONFIG_HOME".into(),
            root.join("config").into_os_string(),
        )]),
    )
}

fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn registry_path(root: &Path) -> PathBuf {
    root.join("config/bintui/registry.toml")
}

struct FailAt(MutationBoundary);

struct SwapAtRemoval {
    path: PathBuf,
}

impl MutationFaultInjector for SwapAtRemoval {
    fn check(&self, boundary: MutationBoundary) -> std::io::Result<()> {
        if boundary == MutationBoundary::ManagedLinkRemoval {
            fs::remove_file(&self.path)?;
            fs::write(&self.path, "externally replaced")?;
        }
        Ok(())
    }
}

impl MutationFaultInjector for FailAt {
    fn check(&self, boundary: MutationBoundary) -> std::io::Result<()> {
        if boundary == self.0 {
            Err(std::io::Error::other(format!(
                "injected {boundary:?} failure"
            )))
        } else {
            Ok(())
        }
    }
}

#[test]
fn add_validation_reports_conflicts_without_mutating_registry_or_link() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());

    let valid = validate_add(
        AddRequest {
            target: target.clone(),
            name: Some("work".to_owned()),
            disabled: false,
        },
        &environment,
    )
    .unwrap();
    assert_eq!(valid.identifier, "add-valid");
    assert!(!registry_path(temp.path()).exists());
    assert!(!temp.path().join(".local/bin/work").exists());

    add(
        AddRequest {
            target,
            name: Some("work".to_owned()),
            disabled: true,
        },
        &environment,
    )
    .unwrap();
    let blocked = validate_add(
        AddRequest {
            target: temp.path().join("project/tool"),
            name: Some("work".to_owned()),
            disabled: false,
        },
        &environment,
    )
    .unwrap();
    assert_eq!(blocked.status, LifecycleStatus::Blocked);
    assert_eq!(blocked.identifier, "command-name-registered");
}

#[test]
fn disabled_add_with_a_name_override_is_visible_through_list() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);

    let added = add(
        AddRequest {
            target: temp.path().join("project/./tool"),
            name: Some("work".to_owned()),
            disabled: true,
        },
        &environment(temp.path()),
    )
    .unwrap();

    assert!(added.registration.as_ref().unwrap().defect.is_none());
    assert!(!added.registration.as_ref().unwrap().managed_link.exists());
    let listed = list(&environment(temp.path())).unwrap();
    assert_eq!(listed.identifier, "registrations-listed");
    assert_eq!(listed.registrations, vec![added.registration.unwrap()]);
}

#[test]
fn registry_stores_home_targets_with_a_tilde_and_loads_them_as_absolute_paths() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());

    add(
        AddRequest {
            target: target.clone(),
            name: Some("work".to_owned()),
            disabled: true,
        },
        &environment,
    )
    .unwrap();

    let contents = fs::read_to_string(registry_path(temp.path())).unwrap();
    assert!(contents.contains("target = \"~/project/tool\""));
    assert!(!contents.contains(temp.path().to_str().unwrap()));
    assert_eq!(
        list(&environment).unwrap().registrations[0]
            .registration
            .target,
        target
    );
}

#[test]
fn enable_disable_and_remove_reconcile_only_the_owned_managed_link() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());
    let added = add(
        AddRequest {
            target: target.clone(),
            name: None,
            disabled: true,
        },
        &environment,
    )
    .unwrap();
    let managed_link = added.registration.unwrap().managed_link;

    let enabled = enable("tool", &environment).unwrap();
    assert_eq!(enabled.identifier, "registration-enabled");
    assert_eq!(fs::read_link(&managed_link).unwrap(), target);
    assert!(
        enable("tool", &environment)
            .unwrap()
            .registration
            .unwrap()
            .registration
            .enabled
    );

    let disabled = disable("tool", &environment).unwrap();
    assert_eq!(disabled.identifier, "registration-disabled");
    assert!(disabled.registration.unwrap().defect.is_none());
    assert!(fs::symlink_metadata(&managed_link).is_err());
    assert!(
        !disable("tool", &environment)
            .unwrap()
            .registration
            .unwrap()
            .registration
            .enabled
    );

    let removed = remove("tool", &environment).unwrap();
    assert_eq!(removed.identifier, "registration-removed");
    assert!(removed.registration.is_none());
    assert!(list(&environment).unwrap().registrations.is_empty());
}

#[test]
fn add_rejects_invalid_names_and_ineligible_targets_without_persisting_them() {
    let temp = TempDir::new().unwrap();
    let valid = temp.path().join("project/tool");
    executable(&valid);
    let directory = temp.path().join("project/directory");
    fs::create_dir_all(&directory).unwrap();
    let plain = temp.path().join("project/plain");
    fs::write(&plain, "plain").unwrap();

    for (target, name, expected) in [
        (
            valid.clone(),
            Some("../escape".to_owned()),
            "invalid Command Name",
        ),
        (temp.path().join("missing"), None, "does not exist"),
        (directory, None, "not a file"),
        (plain, None, "not executable"),
    ] {
        let error = add(
            AddRequest {
                target,
                name,
                disabled: false,
            },
            &environment(temp.path()),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "unexpected error: {error}"
        );
    }
    assert!(list(&environment(temp.path()))
        .unwrap()
        .registrations
        .is_empty());
}

#[test]
fn add_rejects_a_non_utf8_target_before_link_or_registry_mutation() {
    let temp = TempDir::new().unwrap();
    let target = temp
        .path()
        .join("project")
        .join(std::ffi::OsString::from_vec(b"tool-\xff".to_vec()));
    let environment = environment(temp.path());

    let error = add(
        AddRequest {
            target,
            name: Some("tool".to_owned()),
            disabled: false,
        },
        &environment,
    )
    .unwrap_err();

    assert!(error.to_string().contains("valid UTF-8"));
    assert!(!temp.path().join(".local/bin/tool").exists());
    assert!(!registry_path(temp.path()).exists());
}

#[test]
fn duplicate_names_and_unmanaged_paths_are_blocked_without_replacement() {
    let temp = TempDir::new().unwrap();
    let first = temp.path().join("project/first");
    let second = temp.path().join("project/second");
    executable(&first);
    executable(&second);
    let environment = environment(temp.path());
    add(
        AddRequest {
            target: first.clone(),
            name: Some("one".to_owned()),
            disabled: true,
        },
        &environment,
    )
    .unwrap();

    let duplicate_name = add(
        AddRequest {
            target: second.clone(),
            name: Some("one".to_owned()),
            disabled: true,
        },
        &environment,
    )
    .unwrap();
    assert_eq!(duplicate_name.status, LifecycleStatus::Blocked);
    assert_eq!(duplicate_name.identifier, "command-name-registered");
    let occupied = temp.path().join(".local/bin/blocked");
    executable(&occupied);
    let contents = fs::read(&occupied).unwrap();
    let blocked = add(
        AddRequest {
            target: second.clone(),
            name: Some("blocked".to_owned()),
            disabled: false,
        },
        &environment,
    )
    .unwrap();
    assert_eq!(blocked.identifier, "managed-path-occupied");
    assert_eq!(fs::read(occupied).unwrap(), contents);

    let occupied_directory = temp.path().join(".local/bin/directory");
    fs::create_dir(&occupied_directory).unwrap();
    let blocked = add(
        AddRequest {
            target: second.clone(),
            name: Some("directory".to_owned()),
            disabled: false,
        },
        &environment,
    )
    .unwrap();
    assert_eq!(blocked.identifier, "managed-path-occupied");
    assert!(occupied_directory.is_dir());

    let occupied_link = temp.path().join(".local/bin/link");
    symlink(&first, &occupied_link).unwrap();
    let blocked = add(
        AddRequest {
            target: second,
            name: Some("link".to_owned()),
            disabled: false,
        },
        &environment,
    )
    .unwrap();
    assert_eq!(blocked.identifier, "managed-path-occupied");
    assert_eq!(fs::read_link(occupied_link).unwrap(), first);
}

#[test]
fn lifecycle_mutations_block_unmanaged_entries_and_changed_registered_links() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    let other = temp.path().join("project/other");
    executable(&target);
    executable(&other);
    let environment = environment(temp.path());
    let added = add(
        AddRequest {
            target,
            name: None,
            disabled: false,
        },
        &environment,
    )
    .unwrap();
    let managed = added.registration.unwrap().managed_link;
    fs::remove_file(&managed).unwrap();
    symlink(&other, &managed).unwrap();

    for result in [
        disable("tool", &environment).unwrap(),
        remove("tool", &environment).unwrap(),
    ] {
        assert_eq!(result.status, LifecycleStatus::Blocked);
        assert_eq!(result.identifier, "managed-path-conflict");
        assert_eq!(fs::read_link(&managed).unwrap(), other);
    }
    assert_eq!(list(&environment).unwrap().registrations.len(), 1);
}

#[test]
fn disable_removes_a_broken_link_to_an_unexpected_target_and_allows_unregister() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());
    let managed = add(
        AddRequest {
            target,
            name: None,
            disabled: false,
        },
        &environment,
    )
    .unwrap()
    .registration
    .unwrap()
    .managed_link;
    fs::remove_file(&managed).unwrap();
    symlink(temp.path().join("missing/other"), &managed).unwrap();

    let disabled = disable("tool", &environment).unwrap();
    assert_eq!(disabled.identifier, "registration-disabled");
    assert!(!disabled.registration.unwrap().registration.enabled);
    assert!(fs::symlink_metadata(&managed).is_err());
    assert!(
        !list(&environment).unwrap().registrations[0]
            .registration
            .enabled
    );

    assert_eq!(
        remove("tool", &environment).unwrap().identifier,
        "registration-removed"
    );
    assert!(list(&environment).unwrap().registrations.is_empty());
}

#[test]
fn missing_target_can_be_enabled_disabled_and_unregistered() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());
    let managed = add(
        AddRequest {
            target: target.clone(),
            name: None,
            disabled: false,
        },
        &environment,
    )
    .unwrap()
    .registration
    .unwrap()
    .managed_link;
    fs::remove_file(&target).unwrap();

    assert_eq!(
        disable("tool", &environment).unwrap().identifier,
        "registration-disabled"
    );
    assert!(fs::symlink_metadata(&managed).is_err());
    let enabled = enable("tool", &environment).unwrap().registration.unwrap();
    assert!(enabled.registration.enabled);
    assert_eq!(
        enabled.defect.unwrap().kind,
        bintui::model::RegistrationDefectKind::TargetMissing
    );
    assert_eq!(fs::read_link(&managed).unwrap(), target);
    assert_eq!(
        disable("tool", &environment).unwrap().identifier,
        "registration-disabled"
    );
    assert!(fs::symlink_metadata(&managed).is_err());
    assert_eq!(
        remove("tool", &environment).unwrap().identifier,
        "registration-removed"
    );
}

#[test]
fn broken_link_disable_preserves_replacements_and_restores_original_link_on_failure() {
    for boundary in [
        MutationBoundary::ManagedLinkRemoval,
        MutationBoundary::TemporaryRegistryWrite,
    ] {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("project/tool");
        executable(&target);
        let environment = environment(temp.path());
        let managed = add(
            AddRequest {
                target,
                name: None,
                disabled: false,
            },
            &environment,
        )
        .unwrap()
        .registration
        .unwrap()
        .managed_link;
        let unexpected = PathBuf::from("../../missing/other");
        fs::remove_file(&managed).unwrap();
        symlink(&unexpected, &managed).unwrap();

        if boundary == MutationBoundary::ManagedLinkRemoval {
            Application::with_faults(
                &environment,
                &SwapAtRemoval {
                    path: managed.clone(),
                },
            )
            .disable("tool")
            .unwrap_err();
            assert_eq!(fs::read_to_string(&managed).unwrap(), "externally replaced");
        } else {
            Application::with_faults(&environment, &FailAt(boundary))
                .disable("tool")
                .unwrap_err();
            assert_eq!(fs::read_link(&managed).unwrap(), unexpected);
        }
        assert!(
            list(&environment).unwrap().registrations[0]
                .registration
                .enabled
        );
    }
}

#[test]
fn disable_revalidates_ownership_at_the_removal_boundary() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());
    let managed = add(
        AddRequest {
            target,
            name: None,
            disabled: false,
        },
        &environment,
    )
    .unwrap()
    .registration
    .unwrap()
    .managed_link;

    let error = Application::with_faults(
        &environment,
        &SwapAtRemoval {
            path: managed.clone(),
        },
    )
    .disable("tool")
    .unwrap_err();

    assert_eq!(error.identifier(), "disable-failed");
    assert_eq!(fs::read_to_string(&managed).unwrap(), "externally replaced");
    assert!(
        list(&environment).unwrap().registrations[0]
            .registration
            .enabled
    );
}

#[test]
fn remove_revalidates_ownership_at_the_removal_boundary() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());
    let managed = add(
        AddRequest {
            target,
            name: None,
            disabled: false,
        },
        &environment,
    )
    .unwrap()
    .registration
    .unwrap()
    .managed_link;

    let error = Application::with_faults(
        &environment,
        &SwapAtRemoval {
            path: managed.clone(),
        },
    )
    .remove("tool")
    .unwrap_err();

    assert_eq!(error.identifier(), "remove-failed");
    assert_eq!(fs::read_to_string(&managed).unwrap(), "externally replaced");
    assert_eq!(list(&environment).unwrap().registrations.len(), 1);
}

#[test]
fn unknown_names_are_actionable_errors() {
    let temp = TempDir::new().unwrap();
    for error in [
        enable("missing", &environment(temp.path())).unwrap_err(),
        disable("missing", &environment(temp.path())).unwrap_err(),
        remove("missing", &environment(temp.path())).unwrap_err(),
    ] {
        assert!(error.to_string().contains("unknown Command Name"));
    }
}

#[test]
fn mutation_boundary_failures_leave_returned_errors_consistent_with_actual_state() {
    for boundary in [
        MutationBoundary::ManagedLinkCreation,
        MutationBoundary::LockAcquisition,
        MutationBoundary::TemporaryRegistryWrite,
        MutationBoundary::RegistrySync,
        MutationBoundary::AtomicRegistryReplacement,
    ] {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("project/tool");
        executable(&target);
        let environment = environment(temp.path());
        let error = Application::with_faults(&environment, &FailAt(boundary))
            .add(AddRequest {
                target,
                name: None,
                disabled: false,
            })
            .unwrap_err();

        assert!(error.to_string().contains("injected"));
        assert_eq!(error.identifier(), "add-failed");
        assert!(error.resulting_registration().is_none());
        assert!(list(&environment).unwrap().registrations.is_empty());
        assert!(fs::symlink_metadata(temp.path().join(".local/bin/tool")).is_err());
    }

    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());
    let added = add(
        AddRequest {
            target,
            name: None,
            disabled: false,
        },
        &environment,
    )
    .unwrap();
    let managed_link = added.registration.unwrap().managed_link;
    let error =
        Application::with_faults(&environment, &FailAt(MutationBoundary::ManagedLinkRemoval))
            .disable("tool")
            .unwrap_err();
    assert!(error.to_string().contains("injected"));
    assert_eq!(error.identifier(), "disable-failed");
    let returned = error.resulting_registration().unwrap();
    assert!(returned.registration.enabled);
    assert!(returned.defect.is_none());
    let result = list(&environment).unwrap();
    let resulting = &result.registrations[0];
    assert_eq!(resulting, returned);
    assert!(fs::symlink_metadata(managed_link).is_ok());
}

#[test]
fn directory_sync_failure_reports_the_committed_registration_and_link_state() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let environment = environment(temp.path());

    let error = Application::with_faults(
        &environment,
        &FailAt(MutationBoundary::RegistryDirectorySync),
    )
    .add(AddRequest {
        target: target.clone(),
        name: None,
        disabled: false,
    })
    .unwrap_err();

    assert_eq!(error.identifier(), "add-failed");
    let state = error.resulting_registration().unwrap();
    assert_eq!(state.registration.target, target);
    assert!(state.defect.is_none());
    let listed = list(&environment).unwrap();
    assert_eq!(listed.registrations.as_slice(), std::slice::from_ref(state));
    assert_eq!(fs::read_link(&state.managed_link).unwrap(), target);
}

#[test]
fn invalid_registry_content_blocks_mutation_without_rewrite() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let path = registry_path(temp.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for contents in ["version = 99\n", "version = broken\n"] {
        fs::write(&path, contents).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(add(
            AddRequest {
                target: target.clone(),
                name: None,
                disabled: true
            },
            &environment(temp.path()),
        )
        .is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn add_uses_the_target_filename_and_publishes_an_owned_managed_link() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);

    let result = add(
        AddRequest {
            target: target.clone(),
            name: None,
            disabled: false,
        },
        &environment(temp.path()),
    )
    .unwrap();

    assert_eq!(result.status, LifecycleStatus::Healthy);
    assert_eq!(result.identifier, "registration-added");
    let state = result.registration.unwrap();
    assert_eq!(state.registration.name, "tool");
    assert_eq!(state.registration.target, target);
    assert!(state.registration.enabled);
    assert!(state.defect.is_none());
    assert_eq!(
        fs::read_link(&state.managed_link).unwrap(),
        state.registration.target
    );
    assert!(fs::read_to_string(registry_path(temp.path()))
        .unwrap()
        .contains("name = \"tool\""));
}

#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};

use bintui::application::{add, list, AddRequest};
use bintui::environment::Environment;
use bintui::model::{LifecycleStatus, ManagedPathKind, RegistrationDefectKind};
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

fn write_registration(root: &Path, name: &str, target: &Path, enabled: bool) {
    let path = registry_path(root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "version = 1\n\n[[command]]\nname = {name:?}\ntarget = {:?}\nenabled = {enabled}\n",
            target.display().to_string()
        ),
    )
    .unwrap();
}

fn add_disabled(root: &Path, name: &str, target: &Path) {
    add(
        AddRequest {
            target: target.to_path_buf(),
            name: Some(name.to_owned()),
            disabled: true,
        },
        &environment(root),
    )
    .unwrap();
}

#[test]
fn list_reports_no_defects_in_stable_command_name_order() {
    let temp = TempDir::new().unwrap();
    let alpha = temp.path().join("project/alpha");
    let zeta = temp.path().join("project/zeta");
    executable(&alpha);
    executable(&zeta);
    add_disabled(temp.path(), "zeta", &zeta);
    add_disabled(temp.path(), "alpha", &alpha);

    let result = list(&environment(temp.path())).unwrap();

    assert_eq!(result.identifier, "registrations-listed");
    assert_eq!(result.registrations.len(), 2);
    assert_eq!(result.registrations[0].registration.name, "alpha");
    assert_eq!(result.registrations[1].registration.name, "zeta");
    assert!(result.registrations[0].defect.is_none());
    assert_eq!(
        result.registrations[0].observed_link_target,
        None::<PathBuf>
    );
}

#[test]
fn list_restores_a_missing_managed_link_before_reporting_registration_state() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    write_registration(temp.path(), "tool", &target, true);
    let registry_before = fs::read(registry_path(temp.path())).unwrap();
    let managed_link = temp.path().join(".local/bin/tool");

    let result = list(&environment(temp.path())).unwrap();

    assert_eq!(fs::read_link(&managed_link).unwrap(), target);
    assert_eq!(
        fs::read(registry_path(temp.path())).unwrap(),
        registry_before,
        "restoring generated state must not rewrite the Registry"
    );
    assert_eq!(result.status, LifecycleStatus::Healthy);
    assert!(result.registrations[0].defect.is_none());
    assert_eq!(result.registrations[0].actual, ManagedPathKind::OwnedLink);
}

#[test]
fn list_does_not_change_a_malformed_registry() {
    let temp = TempDir::new().unwrap();
    fs::create_dir_all(registry_path(temp.path()).parent().unwrap()).unwrap();
    fs::write(registry_path(temp.path()), "version = broken\n").unwrap();
    let malformed = fs::read(registry_path(temp.path())).unwrap();

    assert!(list(&environment(temp.path())).is_err());
    assert_eq!(fs::read(registry_path(temp.path())).unwrap(), malformed);
}

#[test]
fn invalid_target_defect_precedes_absent_broken_incorrect_and_conflicting_paths() {
    for target_defect in [
        RegistrationDefectKind::TargetMissing,
        RegistrationDefectKind::TargetNotExecutable,
    ] {
        for managed_kind in [
            ManagedPathKind::Missing,
            ManagedPathKind::SymbolicLink, // broken
            ManagedPathKind::OwnedLink,    // setup below points to another existing Target
            ManagedPathKind::File,
        ] {
            let temp = TempDir::new().unwrap();
            let target = temp.path().join("project/tool");
            if target_defect == RegistrationDefectKind::TargetNotExecutable {
                executable(&target);
                fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
            }
            let other = temp.path().join("project/other");
            executable(&other);
            write_registration(temp.path(), "tool", &target, true);
            let managed = temp.path().join(".local/bin/tool");
            fs::create_dir_all(managed.parent().unwrap()).unwrap();
            match managed_kind {
                ManagedPathKind::Missing => {}
                ManagedPathKind::SymbolicLink => {
                    symlink(temp.path().join("missing-other"), &managed).unwrap()
                }
                ManagedPathKind::OwnedLink => symlink(&other, &managed).unwrap(),
                ManagedPathKind::File => fs::write(&managed, "conflict").unwrap(),
                _ => unreachable!(),
            }

            let state = list(&environment(temp.path())).unwrap();
            assert_eq!(
                state.registrations[0]
                    .defect
                    .as_ref()
                    .map(|defect| &defect.kind),
                Some(&target_defect)
            );
        }
    }
}

#[test]
fn a_link_appearing_for_a_disabled_registration_is_an_unmanaged_conflict() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    let other = temp.path().join("project/other");
    executable(&target);
    executable(&other);
    add_disabled(temp.path(), "tool", &target);
    let managed = temp.path().join(".local/bin/tool");
    fs::create_dir_all(managed.parent().unwrap()).unwrap();
    symlink(&other, &managed).unwrap();

    let state = list(&environment(temp.path())).unwrap();
    assert_eq!(
        state.registrations[0]
            .defect
            .as_ref()
            .map(|defect| &defect.kind),
        Some(&RegistrationDefectKind::Conflict)
    );
    assert_eq!(fs::read_link(managed).unwrap(), other);
}

#[test]
fn list_classifies_the_defect_matrix_and_precedence() {
    enum LinkSetup {
        Missing,
        Expected,
        WrongExisting,
        WrongMissing,
        File,
        Directory,
    }
    let cases = [
        (
            "healthy",
            true,
            true,
            true,
            LinkSetup::Expected,
            None,
            ManagedPathKind::OwnedLink,
        ),
        (
            "disabled",
            false,
            true,
            true,
            LinkSetup::Missing,
            None,
            ManagedPathKind::Missing,
        ),
        (
            "disabled-target-missing",
            false,
            false,
            false,
            LinkSetup::Missing,
            Some(RegistrationDefectKind::TargetMissing),
            ManagedPathKind::Missing,
        ),
        (
            "target-missing",
            true,
            false,
            false,
            LinkSetup::Missing,
            Some(RegistrationDefectKind::TargetMissing),
            ManagedPathKind::Missing,
        ),
        (
            "target-not-executable",
            true,
            true,
            false,
            LinkSetup::Missing,
            Some(RegistrationDefectKind::TargetNotExecutable),
            ManagedPathKind::Missing,
        ),
        (
            "link-missing-restored",
            true,
            true,
            true,
            LinkSetup::Missing,
            None,
            ManagedPathKind::OwnedLink,
        ),
        (
            "link-broken",
            true,
            true,
            true,
            LinkSetup::WrongMissing,
            Some(RegistrationDefectKind::LinkBroken),
            ManagedPathKind::SymbolicLink,
        ),
        (
            "link-incorrect",
            true,
            true,
            true,
            LinkSetup::WrongExisting,
            Some(RegistrationDefectKind::LinkIncorrect),
            ManagedPathKind::SymbolicLink,
        ),
        (
            "file-conflict",
            true,
            true,
            true,
            LinkSetup::File,
            Some(RegistrationDefectKind::Conflict),
            ManagedPathKind::File,
        ),
        (
            "directory-conflict",
            true,
            true,
            true,
            LinkSetup::Directory,
            Some(RegistrationDefectKind::Conflict),
            ManagedPathKind::Directory,
        ),
        // Target validity has deterministic precedence over every Managed Link defect.
        (
            "missing-with-wrong-link",
            true,
            false,
            false,
            LinkSetup::WrongExisting,
            Some(RegistrationDefectKind::TargetMissing),
            ManagedPathKind::SymbolicLink,
        ),
        (
            "nonexec-with-file",
            true,
            true,
            false,
            LinkSetup::File,
            Some(RegistrationDefectKind::TargetNotExecutable),
            ManagedPathKind::File,
        ),
        // A disabled Registration has no defect only when its Managed Link is absent.
        (
            "disabled-owned",
            false,
            true,
            true,
            LinkSetup::Expected,
            Some(RegistrationDefectKind::Conflict),
            ManagedPathKind::OwnedLink,
        ),
    ];

    for (name, enabled, target_exists, target_executable, link, defect, actual) in cases {
        let temp = TempDir::new().unwrap();
        let target = temp.path().join("project/tool");
        if target_exists {
            executable(&target);
            if !target_executable {
                fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
            }
        }
        let other = temp.path().join("project/other");
        executable(&other);
        write_registration(temp.path(), name, &target, enabled);
        let managed = temp.path().join(".local/bin").join(name);
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        match link {
            LinkSetup::Missing => {}
            LinkSetup::Expected => symlink(&target, &managed).unwrap(),
            LinkSetup::WrongExisting => symlink(&other, &managed).unwrap(),
            LinkSetup::WrongMissing => symlink(temp.path().join("gone"), &managed).unwrap(),
            LinkSetup::File => fs::write(&managed, "untouched").unwrap(),
            LinkSetup::Directory => fs::create_dir(&managed).unwrap(),
        }

        let result = list(&environment(temp.path())).unwrap();
        let state = &result.registrations[0];
        assert_eq!(
            state.defect.as_ref().map(|defect| &defect.kind),
            defect.as_ref(),
            "case {name}"
        );
        assert_eq!(state.actual, actual, "case {name}");
        assert_eq!(
            result.status,
            if defect.is_none() {
                LifecycleStatus::Healthy
            } else {
                LifecycleStatus::Blocked
            },
            "case {name}"
        );
    }
}

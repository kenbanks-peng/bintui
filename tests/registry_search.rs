#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use bintui::application::{search, SearchRequest};
use bintui::environment::Environment;
use tempfile::TempDir;

fn environment(home: &Path) -> Environment {
    let mut variables = BTreeMap::new();
    variables.insert(
        "XDG_CONFIG_HOME".into(),
        home.join("config").into_os_string(),
    );
    Environment::from_values(home.to_path_buf(), home.to_path_buf(), variables)
}

fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "#!/bin/sh\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn search_reads_but_does_not_modify_registry_configuration_candidates_or_managed_paths() {
    let temp = TempDir::new().unwrap();
    let target = temp.path().join("project/tool");
    executable(&target);
    let config_path = temp.path().join("config/bintui/config.toml");
    let registry_path = temp.path().join("config/bintui/registry.toml");
    let managed = temp.path().join("managed");
    fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    fs::create_dir_all(registry_path.parent().unwrap()).unwrap();
    fs::create_dir_all(&managed).unwrap();
    fs::write(
        &config_path,
        format!("version = 1\nbin_dir = {:?}\n", managed),
    )
    .unwrap();
    fs::write(
        &registry_path,
        format!(
            "version = 1\n[[command]]\nname = \"tool\"\ntarget = {:?}\nenabled = false\n",
            target
        ),
    )
    .unwrap();
    let config_before = fs::read(&config_path).unwrap();
    let registry_before = fs::read(&registry_path).unwrap();
    let target_before = fs::read(&target).unwrap();

    let result = search(
        SearchRequest {
            search_root: Some(temp.path().join("project")),
        },
        &environment(temp.path()),
    )
    .unwrap();

    assert!(
        !result.candidates[0]
            .registration
            .as_ref()
            .unwrap()
            .registration
            .enabled
    );
    assert_eq!(fs::read(config_path).unwrap(), config_before);
    assert_eq!(fs::read(registry_path).unwrap(), registry_before);
    assert_eq!(fs::read(target).unwrap(), target_before);
    assert_eq!(fs::read_dir(managed).unwrap().count(), 0);
}

#[test]
fn invalid_registry_data_fails_without_rewriting_it() {
    let temp = TempDir::new().unwrap();
    executable(&temp.path().join("project/tool"));
    let path = temp.path().join("config/bintui/registry.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for (contents, expected) in [
        ("version = 9\n", "unsupported Registry version 9"),
        ("version = nope\n", "invalid Registry"),
        ("version = 1\n[[command]]\nname = \"bad/name\"\ntarget = \"/tmp/tool\"\nenabled = true\n", "invalid Command Name"),
        ("version = 1\n[[command]]\nname = \"tool\"\ntarget = \"relative\"\nenabled = true\n", "Target must be absolute"),
    ] {
        fs::write(&path, contents).unwrap();
        let before = fs::read(&path).unwrap();
        let error = search(
            SearchRequest { search_root: Some(temp.path().join("project")) },
            &environment(temp.path()),
        ).unwrap_err();
        assert!(error.to_string().contains(expected), "unexpected error: {error}");
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

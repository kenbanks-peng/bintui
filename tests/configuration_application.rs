use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use bintui::configuration::{load, ConfigurationError};
use bintui::environment::Environment;
use bintui::model::display_path_with_roots;
use tempfile::TempDir;

fn env(home: &Path, variables: impl IntoIterator<Item = (&'static str, PathBuf)>) -> Environment {
    Environment::from_values(
        home.to_path_buf(),
        home.to_path_buf(),
        variables
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value.into_os_string()))
            .collect(),
    )
}

#[test]
fn missing_configuration_uses_xdg_bin_then_documented_default() {
    let temp = TempDir::new().unwrap();
    let xdg_bin = temp.path().join("commands");
    let configured = load(&env(temp.path(), [("XDG_BIN_HOME", xdg_bin.clone())])).unwrap();
    assert_eq!(configured.bin_dir, xdg_bin);

    let defaulted = load(&env(temp.path(), [])).unwrap();
    assert_eq!(defaulted.bin_dir, temp.path().join(".local/bin"));
}

#[test]
fn xdg_config_home_takes_precedence_and_configured_path_expands_home_and_variables() {
    let temp = TempDir::new().unwrap();
    let xdg = temp.path().join("xdg");
    let fallback = temp.path().join(".config/bintui");
    fs::create_dir_all(xdg.join("bintui")).unwrap();
    fs::create_dir_all(&fallback).unwrap();
    fs::write(
        xdg.join("bintui/config.toml"),
        "version = 1\nbin_dir = \"$TOOLS/bin\"\n",
    )
    .unwrap();
    fs::write(
        fallback.join("config.toml"),
        "version = 1\nbin_dir = \"~/wrong\"\n",
    )
    .unwrap();
    let tools = temp.path().join("tools");
    let environment = Environment::from_values(
        temp.path().to_path_buf(),
        temp.path().to_path_buf(),
        BTreeMap::from([
            ("XDG_CONFIG_HOME".into(), xdg.into_os_string()),
            ("TOOLS".into(), tools.clone().into_os_string()),
        ]),
    );

    assert_eq!(load(&environment).unwrap().bin_dir, tools.join("bin"));

    fs::write(
        temp.path().join(".config/bintui/config.toml"),
        "version = 1\nbin_dir = \"~/commands\"\n",
    )
    .unwrap();
    assert_eq!(
        load(&env(temp.path(), [])).unwrap().bin_dir,
        temp.path().join("commands")
    );
}

#[test]
fn roots_expand_and_normalize_for_path_reporting() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join(".config/bintui/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        "version = 1\n[roots]\nproject = \"~/workspace/./project\"\n",
    )
    .unwrap();

    let configuration = load(&env(temp.path(), [])).unwrap();
    assert_eq!(
        configuration.roots["project"],
        temp.path().join("workspace/project")
    );
    assert_eq!(
        display_path_with_roots(
            &temp.path().join("workspace/project/tools/bin"),
            temp.path(),
            &configuration.roots,
        ),
        "[project]/tools/bin"
    );
}

#[test]
fn invalid_configuration_is_actionable_and_is_not_rewritten() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join(".config/bintui/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for (contents, expected) in [
        ("version = 9\n", "unsupported configuration version 9"),
        ("version = nope\n", "invalid configuration"),
        (
            "version = 1\nbin_dir = \"relative/bin\"\n",
            "bin_dir must resolve to an absolute path",
        ),
        (
            "version = 1\nbin_dir = \"$MISSING/bin\"\n",
            "missing environment variable MISSING",
        ),
        (
            "version = 1\nignore = [\"\"]\n",
            "must be a non-empty gitignore pattern",
        ),
        (
            "version = 1\n[roots]\nproject = \"relative/project\"\n",
            "roots.project must resolve to an absolute path",
        ),
    ] {
        fs::write(&path, contents).unwrap();
        let before = fs::read(&path).unwrap();
        let error = load(&env(temp.path(), [])).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "unexpected error: {error}"
        );
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn relative_xdg_paths_are_rejected() {
    let environment = Environment::from_values(
        PathBuf::from("/home/person"),
        PathBuf::from("/work"),
        BTreeMap::from([("XDG_CONFIG_HOME".into(), OsString::from("relative"))]),
    );
    assert!(matches!(
        load(&environment),
        Err(ConfigurationError::RelativePath {
            field: "XDG_CONFIG_HOME",
            ..
        })
    ));
}

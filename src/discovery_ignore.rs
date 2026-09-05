use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config_git::{self, ConfigGitError};
use crate::configuration::normalize;
use crate::environment::Environment;

#[derive(Debug, Error)]
pub enum DiscoveryIgnoreError {
    #[error("could not read discovery ignore file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid discovery ignore file {path}: {message}")]
    Invalid { path: PathBuf, message: String },
    #[error("could not create discovery ignore directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not lock discovery ignore file {path}: {source}")]
    Lock {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write discovery ignore file {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not replace discovery ignore file {path}: {source}")]
    Replace {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("discovery ignore file was replaced but its Git synchronization failed: {0}")]
    GitSync(#[from] ConfigGitError),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IgnoreFile {
    version: u32,
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Serialize)]
struct StoredIgnoreFile<'a> {
    version: u32,
    paths: Vec<&'a str>,
}

pub fn path(environment: &Environment) -> PathBuf {
    environment
        .variable("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| environment.home().join(".config"))
        .join("bintui/ignore.toml")
}

pub fn load(environment: &Environment) -> Result<BTreeSet<PathBuf>, DiscoveryIgnoreError> {
    load_path(&path(environment))
}

pub fn add(target: &Path, environment: &Environment) -> Result<(), DiscoveryIgnoreError> {
    let target = normalize(target);
    if !target.is_absolute() {
        return Err(DiscoveryIgnoreError::Invalid {
            path: path(environment),
            message: format!("ignored path must be absolute: {}", target.display()),
        });
    }
    if target.to_str().is_none() {
        return Err(DiscoveryIgnoreError::Invalid {
            path: path(environment),
            message: format!("ignored path must be valid UTF-8: {}", target.display()),
        });
    }

    let ignore_path = path(environment);
    let parent = ignore_path
        .parent()
        .expect("discovery ignore file has a parent");
    fs::create_dir_all(parent).map_err(|source| DiscoveryIgnoreError::CreateDirectory {
        path: parent.to_path_buf(),
        source,
    })?;
    let lock_path = parent.join("ignore.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|source| DiscoveryIgnoreError::Lock {
            path: lock_path.clone(),
            source,
        })?;
    lock.lock_exclusive()
        .map_err(|source| DiscoveryIgnoreError::Lock {
            path: lock_path,
            source,
        })?;

    let mut paths = load_path(&ignore_path)?;
    paths.insert(target);
    write_atomic(&ignore_path, &paths)?;
    config_git::commit_and_push(&ignore_path, environment)?;
    Ok(())
}

fn load_path(path: &Path) -> Result<BTreeSet<PathBuf>, DiscoveryIgnoreError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(source) => {
            return Err(DiscoveryIgnoreError::Read {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let parsed: IgnoreFile =
        toml::from_str(&contents).map_err(|error| DiscoveryIgnoreError::Invalid {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    if parsed.version != 1 {
        return Err(DiscoveryIgnoreError::Invalid {
            path: path.to_path_buf(),
            message: format!(
                "unsupported discovery ignore version {}; expected 1",
                parsed.version
            ),
        });
    }
    parsed
        .paths
        .into_iter()
        .map(|value| {
            let ignored = PathBuf::from(&value);
            if value.is_empty() || !ignored.is_absolute() {
                Err(DiscoveryIgnoreError::Invalid {
                    path: path.to_path_buf(),
                    message: format!("ignored path must be a non-empty absolute path: {value:?}"),
                })
            } else {
                Ok(normalize(&ignored))
            }
        })
        .collect()
}

fn write_atomic(path: &Path, paths: &BTreeSet<PathBuf>) -> Result<(), DiscoveryIgnoreError> {
    let stored_paths = paths
        .iter()
        .map(|ignored| {
            ignored
                .to_str()
                .ok_or_else(|| DiscoveryIgnoreError::Invalid {
                    path: path.to_path_buf(),
                    message: format!("ignored path must be valid UTF-8: {}", ignored.display()),
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let contents = toml::to_string_pretty(&StoredIgnoreFile {
        version: 1,
        paths: stored_paths,
    })
    .expect("discovery ignore model is serializable");
    let parent = path.parent().expect("discovery ignore file has a parent");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".ignore.{}.{nonce}.tmp", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| DiscoveryIgnoreError::Write {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(contents.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|source| DiscoveryIgnoreError::Write {
                path: temporary.clone(),
                source,
            })?;
        fs::rename(&temporary, path).map_err(|source| DiscoveryIgnoreError::Replace {
            path: path.to_path_buf(),
            source,
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| DiscoveryIgnoreError::Write {
                path: path.to_path_buf(),
                source,
            })
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

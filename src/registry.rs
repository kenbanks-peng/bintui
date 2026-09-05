use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config_git::{ConfigGitError, ConfigGitSync};
use crate::configuration::{expand_path, normalize};
use crate::environment::Environment;
use crate::model::{MutationBoundary, MutationFaultInjector, Registration};

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("could not read Registry {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid Registry {path}: {message}")]
    Invalid { path: PathBuf, message: String },
    #[error("could not create Registry directory {path}: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not acquire Registry lock {path}: {source}")]
    Lock {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not write temporary Registry {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not sync temporary Registry {path}: {source}")]
    Sync {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not atomically replace Registry {path}: {source}")]
    Replace {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Registry {path} was replaced but its directory could not be synced: {source}")]
    Durability {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Registry was replaced but its Git synchronization failed: {0}")]
    GitSync(#[from] ConfigGitError),
}

impl RegistryError {
    /// True when the atomic replacement already committed the new desired state.
    pub fn replacement_committed(&self) -> bool {
        matches!(self, Self::Durability { .. } | Self::GitSync(_))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    version: u32,
    #[serde(default, rename = "command")]
    registrations: Vec<RegistrationFile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrationFile {
    name: String,
    target: String,
    enabled: bool,
}

#[derive(Serialize)]
struct StoredRegistry<'a> {
    version: u32,
    #[serde(rename = "command")]
    registrations: Vec<StoredRegistration<'a>>,
}

#[derive(Serialize)]
struct StoredRegistration<'a> {
    name: &'a str,
    target: Cow<'a, str>,
    enabled: bool,
}

pub struct LockedRegistry {
    path: PathBuf,
    home: PathBuf,
    _lock: File,
    registrations: Vec<Registration>,
}

impl LockedRegistry {
    pub fn acquire(
        environment: &Environment,
        faults: &dyn MutationFaultInjector,
    ) -> Result<Self, RegistryError> {
        let path = registry_path(environment);
        let parent = path.parent().expect("Registry has a parent");
        fs::create_dir_all(parent).map_err(|source| RegistryError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
        let lock_path = parent.join("registry.lock");
        faults
            .check(MutationBoundary::LockAcquisition)
            .map_err(|source| RegistryError::Lock {
                path: lock_path.clone(),
                source,
            })?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|source| RegistryError::Lock {
                path: lock_path.clone(),
                source,
            })?;
        lock.lock_exclusive()
            .map_err(|source| RegistryError::Lock {
                path: lock_path,
                source,
            })?;
        let registrations = load_path(&path, environment)?;
        Ok(Self {
            path,
            home: environment.home().to_path_buf(),
            _lock: lock,
            registrations,
        })
    }

    pub fn registrations(&self) -> &[Registration] {
        &self.registrations
    }

    pub fn replace(
        &mut self,
        registrations: Vec<Registration>,
        faults: &dyn MutationFaultInjector,
        git_sync: &ConfigGitSync,
        environment: &Environment,
    ) -> Result<(), RegistryError> {
        write_atomic(&self.path, &self.home, &registrations, faults)?;
        self.registrations = registrations;
        git_sync.synchronize(&self.path, environment)?;
        Ok(())
    }
}

pub fn registry_path(environment: &Environment) -> PathBuf {
    environment
        .variable("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| environment.home().join(".config"))
        .join("bintui/registry.toml")
}

pub fn load(environment: &Environment) -> Result<Vec<Registration>, RegistryError> {
    load_path(&registry_path(environment), environment)
}

fn load_path(path: &Path, environment: &Environment) -> Result<Vec<Registration>, RegistryError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(RegistryError::Read {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    let parsed: RegistryFile =
        toml::from_str(&contents).map_err(|error| RegistryError::Invalid {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    if parsed.version != 1 {
        return Err(RegistryError::Invalid {
            path: path.to_path_buf(),
            message: format!(
                "unsupported Registry version {}; expected 1",
                parsed.version
            ),
        });
    }
    let mut names = BTreeSet::new();
    let mut registrations = Vec::new();
    for registration in parsed.registrations {
        validate_name(&registration.name).map_err(|message| RegistryError::Invalid {
            path: path.to_path_buf(),
            message,
        })?;
        if !names.insert(registration.name.clone()) {
            return Err(RegistryError::Invalid {
                path: path.to_path_buf(),
                message: format!("duplicate Command Name {:?}", registration.name),
            });
        }
        let target = expand_path(&registration.target, environment).map_err(|error| {
            RegistryError::Invalid {
                path: path.to_path_buf(),
                message: error.to_string(),
            }
        })?;
        if !target.is_absolute() {
            return Err(RegistryError::Invalid {
                path: path.to_path_buf(),
                message: format!("Target must be absolute: {}", target.display()),
            });
        }
        registrations.push(Registration {
            name: registration.name,
            target: normalize(&target),
            enabled: registration.enabled,
        });
    }
    Ok(registrations)
}

pub fn validate_target_path(target: &Path) -> Result<&str, String> {
    target.to_str().ok_or_else(|| {
        format!(
            "Target path must be valid UTF-8 to be stored in the Registry: {}",
            target.display()
        )
    })
}

fn stored_target_path<'a>(target: &'a Path, home: &Path) -> Result<Cow<'a, str>, String> {
    let relative = match target.strip_prefix(home) {
        Ok(relative) => relative,
        Err(_) => return validate_target_path(target).map(Cow::Borrowed),
    };
    if relative.as_os_str().is_empty() {
        return Ok(Cow::Borrowed("~"));
    }
    let stored = Path::new("~").join(relative);
    validate_target_path(&stored).map(|value| Cow::Owned(value.to_owned()))
}

pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\0')
        || Path::new(name).file_name().and_then(|item| item.to_str()) != Some(name)
    {
        Err(format!("invalid Command Name {name:?}"))
    } else {
        Ok(())
    }
}

fn write_atomic(
    path: &Path,
    home: &Path,
    registrations: &[Registration],
    faults: &dyn MutationFaultInjector,
) -> Result<(), RegistryError> {
    let stored = StoredRegistry {
        version: 1,
        registrations: registrations
            .iter()
            .map(|registration| {
                Ok(StoredRegistration {
                    name: &registration.name,
                    target: stored_target_path(&registration.target, home).map_err(|message| {
                        RegistryError::Invalid {
                            path: path.to_path_buf(),
                            message,
                        }
                    })?,
                    enabled: registration.enabled,
                })
            })
            .collect::<Result<Vec<_>, RegistryError>>()?,
    };
    let contents = toml::to_string_pretty(&stored).expect("Registry model is serializable");
    let parent = path.parent().expect("Registry has a parent");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = parent.join(format!(".registry.{}.{nonce}.tmp", std::process::id()));
    let result = (|| {
        faults
            .check(MutationBoundary::TemporaryRegistryWrite)
            .map_err(|source| RegistryError::Write {
                path: temporary.clone(),
                source,
            })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| RegistryError::Write {
                path: temporary.clone(),
                source,
            })?;
        file.write_all(contents.as_bytes())
            .map_err(|source| RegistryError::Write {
                path: temporary.clone(),
                source,
            })?;
        faults
            .check(MutationBoundary::RegistrySync)
            .and_then(|()| file.sync_all())
            .map_err(|source| RegistryError::Sync {
                path: temporary.clone(),
                source,
            })?;
        faults
            .check(MutationBoundary::AtomicRegistryReplacement)
            .map_err(|source| RegistryError::Replace {
                path: path.to_path_buf(),
                source,
            })?;
        fs::rename(&temporary, path).map_err(|source| RegistryError::Replace {
            path: path.to_path_buf(),
            source,
        })?;
        faults
            .check(MutationBoundary::RegistryDirectorySync)
            .and_then(|()| File::open(parent))
            .and_then(|directory| directory.sync_all())
            .map_err(|source| RegistryError::Durability {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

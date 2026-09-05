use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;
use serde::Deserialize;
use thiserror::Error;

use crate::environment::Environment;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub bin_dir: PathBuf,
    pub ignore: Vec<String>,
    pub roots: BTreeMap<String, PathBuf>,
}

#[derive(Debug, Error)]
pub enum ConfigurationError {
    #[error("HOME is required to resolve configuration paths")]
    MissingHome,
    #[error("configured path references missing environment variable {0}")]
    MissingVariable(String),
    #[error("{field} must resolve to an absolute path, got {path}")]
    RelativePath { field: &'static str, path: String },
    #[error("could not read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid configuration {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default = "current_version")]
    version: u32,
    bin_dir: Option<String>,
    #[serde(default)]
    ignore: Vec<String>,
    #[serde(default)]
    roots: BTreeMap<String, String>,
}

fn current_version() -> u32 {
    1
}

pub fn load(environment: &Environment) -> Result<Configuration, ConfigurationError> {
    if environment.home().as_os_str().is_empty() {
        return Err(ConfigurationError::MissingHome);
    }
    let config_home = environment
        .variable("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| environment.home().join(".config"));
    require_absolute("XDG_CONFIG_HOME", &config_home)?;
    let path = config_home.join("bintui/config.toml");
    let parsed = match fs::read_to_string(&path) {
        Ok(contents) => Some(toml::from_str::<ConfigFile>(&contents).map_err(|error| {
            ConfigurationError::Invalid {
                path: path.clone(),
                message: error.to_string(),
            }
        })?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => return Err(ConfigurationError::Read { path, source }),
    };
    if let Some(config) = &parsed {
        if config.version != 1 {
            return Err(ConfigurationError::Invalid {
                path,
                message: format!(
                    "unsupported configuration version {}; expected 1",
                    config.version
                ),
            });
        }
        for pattern in &config.ignore {
            validate_ignore_pattern(pattern).map_err(|message| ConfigurationError::Invalid {
                path: path.clone(),
                message,
            })?;
        }
        for name in config.roots.keys() {
            validate_root_name(name).map_err(|message| ConfigurationError::Invalid {
                path: path.clone(),
                message,
            })?;
        }
    }
    let mut roots = BTreeMap::new();
    if let Some(config) = &parsed {
        for (name, value) in &config.roots {
            let root = expand_path(value, environment)?;
            if !root.is_absolute() {
                return Err(ConfigurationError::Invalid {
                    path: path.clone(),
                    message: format!(
                        "roots.{name} must resolve to an absolute path, got {}",
                        root.display()
                    ),
                });
            }
            roots.insert(name.clone(), normalize(&root));
        }
    }
    let configured_bin = parsed.as_ref().and_then(|config| config.bin_dir.as_deref());
    let raw_bin = configured_bin
        .map(str::to_owned)
        .or_else(|| {
            environment
                .variable("XDG_BIN_HOME")
                .map(|value| value.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| {
            environment
                .home()
                .join(".local/bin")
                .to_string_lossy()
                .into_owned()
        });
    let bin_dir = expand_path(&raw_bin, environment)?;
    require_absolute("bin_dir", &bin_dir)?;
    Ok(Configuration {
        bin_dir: normalize(&bin_dir),
        ignore: parsed.map(|config| config.ignore).unwrap_or_default(),
        roots,
    })
}

fn validate_root_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('[')
        || name.contains(']')
        || name.contains('\0')
    {
        Err(format!(
            "roots key {name:?} must be a non-empty display name without '/', '[', ']', or NUL"
        ))
    } else {
        Ok(())
    }
}

fn validate_ignore_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() || pattern.contains('\0') {
        return Err(format!(
            "ignore value {pattern:?} must be a non-empty gitignore pattern without NUL"
        ));
    }
    let mut builder = GitignoreBuilder::new("");
    builder
        .add_line(None, &gitignore_pattern(pattern))
        .map_err(|error| {
            format!("ignore value {pattern:?} is not a valid gitignore pattern: {error}")
        })?;
    builder.build().map_err(|error| {
        format!("ignore value {pattern:?} is not a valid gitignore pattern: {error}")
    })?;
    Ok(())
}

pub(crate) fn gitignore_pattern(pattern: &str) -> String {
    let (negation, body) = pattern
        .strip_prefix('!')
        .map_or(("", pattern), |body| ("!", body));
    if body.starts_with('/') || !body.contains('/') || pattern.starts_with('#') {
        pattern.to_owned()
    } else {
        format!("{negation}**/{body}")
    }
}

pub fn expand_path(value: &str, environment: &Environment) -> Result<PathBuf, ConfigurationError> {
    let mut expanded = if value == "~" {
        environment.home().to_string_lossy().into_owned()
    } else if let Some(rest) = value.strip_prefix("~/") {
        environment.home().join(rest).to_string_lossy().into_owned()
    } else {
        value.to_owned()
    };
    let mut cursor = 0;
    while let Some(relative) = expanded[cursor..].find('$') {
        let start = cursor + relative;
        let (name_start, name_end, replace_end) = if expanded.as_bytes().get(start + 1)
            == Some(&b'{')
        {
            let end = expanded[start + 2..]
                .find('}')
                .map(|offset| start + 2 + offset)
                .ok_or_else(|| {
                    ConfigurationError::MissingVariable(expanded[start + 2..].to_owned())
                })?;
            (start + 2, end, end + 1)
        } else {
            let end = expanded[start + 1..]
                .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
                .map(|offset| start + 1 + offset)
                .unwrap_or(expanded.len());
            (start + 1, end, end)
        };
        let name = expanded[name_start..name_end].to_owned();
        if name.is_empty() {
            cursor = start + 1;
            continue;
        }
        let replacement = environment
            .variable(&name)
            .ok_or_else(|| ConfigurationError::MissingVariable(name.clone()))?
            .to_string_lossy()
            .into_owned();
        expanded.replace_range(start..replace_end, &replacement);
        cursor = start + replacement.len();
    }
    Ok(PathBuf::from(expanded))
}

pub fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn require_absolute(field: &'static str, path: &Path) -> Result<(), ConfigurationError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(ConfigurationError::RelativePath {
            field,
            path: path.display().to_string(),
        })
    }
}

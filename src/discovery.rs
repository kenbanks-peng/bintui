use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

use crate::configuration::{gitignore_pattern, normalize};
use crate::model::{SearchWarning, WarningKind};

pub struct Discovery {
    pub targets: Vec<PathBuf>,
    pub warnings: Vec<SearchWarning>,
}

pub fn discover(
    root: &Path,
    bin_dir: &Path,
    ignored_patterns: &[String],
    ignored_paths: &BTreeSet<PathBuf>,
) -> std::io::Result<Discovery> {
    let metadata = fs::metadata(root)?;
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("search root is not a directory: {}", root.display()),
        ));
    }
    let ignored = ignore_matcher(root, ignored_patterns)?;
    let mut discovery = Discovery {
        targets: Vec::new(),
        warnings: Vec::new(),
    };
    visit(root, bin_dir, &ignored, ignored_paths, &mut discovery);
    discovery.targets.sort();
    Ok(discovery)
}

fn ignore_matcher(root: &Path, patterns: &[String]) -> std::io::Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    for pattern in patterns {
        builder
            .add_line(None, &gitignore_pattern(pattern))
            .map_err(invalid_ignore_pattern)?;
    }
    builder.build().map_err(invalid_ignore_pattern)
}

fn invalid_ignore_pattern(error: ignore::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, error)
}

fn visit(
    path: &Path,
    bin_dir: &Path,
    ignored: &Gitignore,
    ignored_paths: &BTreeSet<PathBuf>,
    discovery: &mut Discovery,
) {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) => {
            discovery.warnings.push(SearchWarning {
                kind: WarningKind::UnreadableDirectory,
                path: path.to_path_buf(),
                message: error.to_string(),
            });
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                discovery.warnings.push(SearchWarning {
                    kind: WarningKind::UnreadablePath,
                    path: path.to_path_buf(),
                    message: error.to_string(),
                });
                continue;
            }
        };
        let child = normalize(&entry.path());
        if ignored_paths.contains(&child) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&child) {
            Ok(metadata) => metadata,
            Err(error) => {
                discovery.warnings.push(SearchWarning {
                    kind: WarningKind::UnreadablePath,
                    path: child,
                    message: error.to_string(),
                });
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            if let Ok(target_metadata) = fs::metadata(&child) {
                if target_metadata.is_file() && executable(&target_metadata) {
                    discovery.targets.push(child);
                }
            }
        } else if metadata.is_dir() {
            if !ignored.matched(&child, true).is_ignore() && child != bin_dir {
                visit(&child, bin_dir, ignored, ignored_paths, discovery);
            }
        } else if metadata.is_file() && executable(&metadata) {
            discovery.targets.push(child);
        }
    }
}

fn executable(metadata: &fs::Metadata) -> bool {
    metadata.permissions().mode() & 0o111 != 0
}

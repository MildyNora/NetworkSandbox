use std::fs::{self, FileType, Metadata};
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use walkdir::WalkDir;

use crate::model::{Change, ChangeKind, Environment, Origin};

const MAX_TEXT_DIFF_BYTES: u64 = 2 * 1024 * 1024;

pub fn scan_changes(environment: &mut Environment, upper: &Path) -> Result<Vec<Change>> {
    let mut changes = Vec::new();
    if !upper.exists() {
        return Ok(changes);
    }

    for entry in WalkDir::new(upper).follow_links(false).into_iter() {
        let entry = entry?;
        if entry.path() == upper {
            continue;
        }
        let relative = entry.path().strip_prefix(upper)?;
        validate_relative_path(relative)?;
        if is_ignored(environment, relative) {
            continue;
        }

        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_dir() {
            continue;
        }

        let base_path = environment.base_root.join(relative);
        let origin = environment
            .origins
            .entry(relative.to_path_buf())
            .or_insert_with(|| {
                inspect_origin(&base_path).unwrap_or(Origin {
                    existed: false,
                    kind: None,
                    digest: None,
                })
            })
            .clone();

        let whiteout = is_whiteout(&metadata);
        let sandbox_kind = if whiteout {
            None
        } else {
            Some(file_kind(metadata.file_type()).to_owned())
        };
        let kind = if whiteout {
            ChangeKind::Deleted
        } else if !origin.existed {
            ChangeKind::Added
        } else if origin.kind.as_deref() != sandbox_kind.as_deref() {
            ChangeKind::TypeChanged
        } else {
            ChangeKind::Modified
        };
        let sandbox_digest = if whiteout {
            None
        } else {
            Some(digest_path(entry.path())?)
        };
        let binary = if metadata.file_type().is_file() {
            !is_probably_text(entry.path()).unwrap_or(false)
        } else {
            false
        };

        changes.push(Change {
            path: relative.to_path_buf(),
            kind,
            original_digest: origin.digest,
            sandbox_digest,
            binary,
        });
    }

    for relative in environment.deleted_paths.clone() {
        validate_relative_path(&relative)?;
        if is_ignored(environment, &relative)
            || changes.iter().any(|change| change.path == relative)
        {
            continue;
        }
        let base_path = environment.base_root.join(&relative);
        let origin = environment
            .origins
            .entry(relative.clone())
            .or_insert(inspect_origin(&base_path)?)
            .clone();
        if origin.existed {
            changes.push(Change {
                path: relative,
                kind: ChangeKind::Deleted,
                original_digest: origin.digest,
                sandbox_digest: None,
                binary: false,
            });
        }
    }

    changes.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(changes)
}

pub fn conflicts(environment: &Environment, changes: &[Change]) -> Vec<PathBuf> {
    changes
        .iter()
        .filter_map(|change| {
            let current = inspect_origin(&environment.base_root.join(&change.path)).ok()?;
            let recorded = environment.origins.get(&change.path)?;
            if current.existed != recorded.existed
                || current.kind != recorded.kind
                || current.digest != recorded.digest
            {
                Some(change.path.clone())
            } else {
                None
            }
        })
        .collect()
}

pub fn render_change_diff(
    environment: &Environment,
    upper: &Path,
    change: &Change,
) -> Result<String> {
    let header = format!("{} {}\n", change.kind, change.path.display());
    if change.binary || change.kind == ChangeKind::Deleted && change.original_digest.is_none() {
        return Ok(header);
    }
    let base_path = environment.base_root.join(&change.path);
    let sandbox_path = upper.join(&change.path);
    let old = if base_path.is_file() {
        read_text(&base_path)?
    } else {
        String::new()
    };
    let new = if sandbox_path.is_file() && change.kind != ChangeKind::Deleted {
        read_text(&sandbox_path)?
    } else {
        String::new()
    };
    if old == new {
        return Ok(header);
    }
    let unified = TextDiff::from_lines(&old, &new)
        .unified_diff()
        .header(
            &format!("host/{}", change.path.display()),
            &format!("sandbox/{}", change.path.display()),
        )
        .to_string();
    Ok(format!("{header}{unified}"))
}

pub fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("unsafe relative path '{}'", path.display());
    }
    Ok(())
}

pub fn digest_path(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    let mut hasher = Sha256::new();
    hasher.update(metadata.mode().to_le_bytes());
    hasher.update(metadata.uid().to_le_bytes());
    hasher.update(metadata.gid().to_le_bytes());
    if metadata.file_type().is_symlink() {
        hasher.update(b"symlink\0");
        hasher.update(fs::read_link(path)?.as_os_str().as_encoded_bytes());
    } else if metadata.file_type().is_file() {
        hasher.update(b"file\0");
        let mut file = fs::File::open(path)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
    } else {
        hasher.update(file_kind(metadata.file_type()).as_bytes());
    }
    let mut attributes = xattr::list(path)?.collect::<Vec<_>>();
    attributes.sort();
    for name in attributes {
        hasher.update(name.as_encoded_bytes());
        hasher.update([0]);
        if let Some(value) = xattr::get(path, &name)? {
            hasher.update(value);
        }
        hasher.update([0]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn inspect_origin(path: &Path) -> Result<Origin> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Origin {
            existed: true,
            kind: Some(file_kind(metadata.file_type()).to_owned()),
            digest: Some(digest_path(path)?),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Origin {
            existed: false,
            kind: None,
            digest: None,
        }),
        Err(error) => Err(error.into()),
    }
}

fn is_ignored(environment: &Environment, path: &Path) -> bool {
    environment
        .ignored_paths
        .iter()
        .any(|ignored| path.starts_with(ignored))
}

fn file_kind(file_type: FileType) -> &'static str {
    if file_type.is_file() {
        "file"
    } else if file_type.is_symlink() {
        "symlink"
    } else if file_type.is_dir() {
        "directory"
    } else if file_type.is_char_device() {
        "char_device"
    } else if file_type.is_block_device() {
        "block_device"
    } else if file_type.is_fifo() {
        "fifo"
    } else if file_type.is_socket() {
        "socket"
    } else {
        "unknown"
    }
}

fn is_whiteout(metadata: &Metadata) -> bool {
    metadata.file_type().is_char_device() && metadata.rdev() == 0
}

fn is_probably_text(path: &Path) -> Result<bool> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_TEXT_DIFF_BYTES {
        return Ok(false);
    }
    let bytes = fs::read(path)?;
    Ok(!bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok())
}

fn read_text(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_TEXT_DIFF_BYTES {
        bail!("{} is too large for a text diff", path.display());
    }
    fs::read_to_string(path).with_context(|| format!("read {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::model::EnvironmentPolicy;

    #[test]
    fn finds_added_and_modified_files() {
        let root = tempdir().unwrap();
        let upper = tempdir().unwrap();
        fs::write(root.path().join("existing"), "old\n").unwrap();
        fs::write(upper.path().join("existing"), "new\n").unwrap();
        fs::write(upper.path().join("added"), "hello\n").unwrap();
        let mut environment = Environment::new(
            "test".into(),
            None,
            root.path().into(),
            EnvironmentPolicy::default(),
            Vec::new(),
        );

        let changes = scan_changes(&mut environment, upper.path()).unwrap();

        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].kind, ChangeKind::Added);
        assert_eq!(changes[1].kind, ChangeKind::Modified);
    }

    #[test]
    fn rejects_parent_traversal() {
        assert!(validate_relative_path(Path::new("../etc/passwd")).is_err());
    }
}

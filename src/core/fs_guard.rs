//! Platform-aware filesystem safety guards and path normalization.
//!
//! All symlink / Windows reparse-point checks, path overlap checks, and
//! non-following file-open flags live here so higher-level scan/copy logic
//! stays platform-agnostic.

use super::PlannedFile;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

pub(crate) fn unsupported(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

// On Windows FILE_ATTRIBUTE_REPARSE_POINT includes junctions, not just symlinks.
pub(crate) fn is_reparse(metadata: &Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

// Canonicalize the existing prefix, retaining a not-yet-created suffix. This
// resolves drive aliases/junction aliases before comparing directory ancestry.
pub(crate) fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut suffix = Vec::<OsString>::new();
    loop {
        match fs::canonicalize(existing) {
            Ok(mut resolved) => {
                for part in suffix.iter().rev() {
                    if part == OsStr::new("..") {
                        if !resolved.pop() {
                            return Err(unsupported("路径超出根目录"));
                        }
                    } else if part != OsStr::new(".") {
                        resolved.push(part);
                    }
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Components, unlike file_name(), preserve a trailing '..'.
                let component = existing
                    .components()
                    .next_back()
                    .ok_or_else(|| unsupported("无法解析路径"))?;
                match component {
                    Component::Normal(name) => suffix.push(name.to_os_string()),
                    Component::ParentDir => suffix.push(OsString::from("..")),
                    Component::CurDir => suffix.push(OsString::from(".")),
                    _ => return Err(error),
                }
                existing = existing.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        // Canonical paths already have normalized separators and verbatim drive
        // prefixes. Windows paths are compared case-insensitively here.
        PathBuf::from(path.as_os_str().to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

pub(crate) fn overlap(a: &Path, b: &Path) -> bool {
    let a = path_key(a);
    let b = path_key(b);
    a.starts_with(&b) || b.starts_with(&a)
}

pub(crate) fn nearest_existing_directory(path: &Path) -> io::Result<PathBuf> {
    let mut cursor = path;
    loop {
        match fs::symlink_metadata(cursor) {
            Ok(meta) if meta.is_dir() && !is_reparse(&meta) => {
                return Ok(cursor.to_path_buf());
            }
            Ok(_) => return Err(unsupported("目标路径包含非目录或重解析点")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                cursor = cursor.parent().ok_or(error)?;
            }
            Err(error) => return Err(error),
        }
    }
}

// Reject reparse points in all currently existing path components. The canonical
// root selected during scan is allowed; newly introduced indirections are not.
pub(crate) fn check_directory_chain(path: &Path) -> io::Result<()> {
    let mut cumulative = PathBuf::new();
    for component in path.components() {
        cumulative.push(component.as_os_str());
        // A bare Windows verbatim prefix (\\?\C:) is not a valid absolute
        // directory until RootDir has been appended. Never stat that prefix.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&cumulative) {
            Ok(meta) if meta.is_dir() && !is_reparse(&meta) => (),
            Ok(meta) if is_reparse(&meta) => {
                return Err(unsupported(format!(
                    "拒绝重解析点: {}",
                    cumulative.display()
                )))
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::NotADirectory,
                    format!("目标或源父路径不是目录: {}", cumulative.display()),
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

pub(crate) fn probe_writable(destination: &Path) -> io::Result<()> {
    check_directory_chain(destination)?;
    let parent = nearest_existing_directory(destination)?;
    let mut probe = tempfile::Builder::new()
        .prefix(".picsync-probe-")
        .tempfile_in(parent)?;
    let tested = probe
        .write_all(b"PicSync write probe\n")
        .and_then(|_| probe.flush())
        .and_then(|_| probe.as_file().sync_all());
    // Explicit close makes cleanup errors visible even when the write failed.
    let cleaned = probe.close();
    match (tested, cleaned) {
        (Err(write), Err(clean)) => Err(io::Error::new(
            write.kind(),
            format!("写入探针失败: {write}; 探针清理也失败: {clean}"),
        )),
        (Err(error), _) | (_, Err(error)) => Err(error),
        _ => Ok(()),
    }
}

pub(crate) fn metadata_matches(metadata: &Metadata, planned: &PlannedFile) -> bool {
    metadata.is_file()
        && !is_reparse(metadata)
        && metadata.len() == planned.size
        && metadata.modified().ok() == Some(planned.modified)
}

pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        check_directory_chain(parent)?;
    }
    let before = fs::symlink_metadata(path)?;
    if !before.is_file() || is_reparse(&before) {
        return Err(unsupported("拒绝符号链接、重解析点或非普通文件"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself, never its target. Also deny write and
        // delete sharing while copying/hashing (FILE_SHARE_READ only).
        options.custom_flags(0x0020_0000).share_mode(0x0000_0001);
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0002_0000); // O_NOFOLLOW
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    let after = fs::symlink_metadata(path)?;
    if !opened.is_file() || is_reparse(&opened) || !after.is_file() || is_reparse(&after) {
        return Err(unsupported("打开过程中检测到文件链接替换"));
    }
    if opened.len() != after.len() || opened.modified().ok() != after.modified().ok() {
        return Err(unsupported("打开过程中源文件已变更"));
    }
    Ok(file)
}

pub(crate) fn numbered_path(base: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return base.to_path_buf();
    }
    let mut name = base
        .file_stem()
        .unwrap_or_else(|| OsStr::new("image"))
        .to_os_string();
    name.push(format!(" ({index})"));
    if let Some(extension) = base.extension() {
        name.push(".");
        name.push(extension);
    }
    base.with_file_name(name)
}

// Only objective capacity/device/path failures stop the entire run. Individual
// permissions and sharing violations are reported and the next item is tried.
pub(crate) fn globally_unavailable(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory | io::ErrorKind::InvalidInput
    ) {
        return true;
    }
    #[cfg(windows)]
    {
        matches!(
            error.raw_os_error(),
            Some(3 | 15 | 21 | 39 | 53 | 55 | 64 | 67 | 112 | 1167)
        )
    }
    #[cfg(not(windows))]
    {
        matches!(error.raw_os_error(), Some(5 | 19 | 28 | 30 | 116 | 122))
    }
}

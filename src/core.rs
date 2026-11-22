//! Date-based image planning and conservative, no-clobber copying.
//!
//! Dates are local calendar dates. Archives use `YYYY-MM-DD/name.ext` and
//! collisions use `name (1).ext`, `name (2).ext`, ... within that date only.
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use filetime::FileTime;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use tempfile::NamedTempFile;
use walkdir::WalkDir;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DateBasis {
    TakenFirst,
    Modified,
    Created,
}

impl DateBasis {
    pub fn label(self) -> &'static str {
        match self {
            Self::TakenFirst => "拍摄日期优先",
            Self::Modified => "修改日期",
            Self::Created => "创建日期",
        }
    }
}

#[derive(Clone)]
pub struct ScanOptions {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub recursive: bool,
    pub basis: DateBasis,
}

#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub source: PathBuf,
    pub target: PathBuf,
    pub date: NaiveDate,
    pub basis_label: String,
    pub size: u64,
    pub modified: SystemTime,
    pub action: String,
}

#[derive(Debug, Default, Clone)]
pub struct ScanPlan {
    pub files: Vec<PlannedFile>,
    pub warnings: Vec<String>,
    pub total_bytes: u64,
}

#[derive(Debug, Default, Clone)]
pub struct CopyReport {
    pub copied: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone)]
pub enum Progress {
    Scan {
        visited: usize,
        matched: usize,
    },
    Copy {
        done: usize,
        total: usize,
        current: String,
        copied: usize,
        skipped: usize,
        failed: usize,
    },
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

fn unsupported(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

// On Windows FILE_ATTRIBUTE_REPARSE_POINT includes junctions, not just symlinks.
fn is_reparse(metadata: &Metadata) -> bool {
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

fn image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .map(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "tif" | "tiff" | "webp" | "bmp" | "gif" | "heic" | "heif"
            )
        })
        .unwrap_or(false)
}

// Canonicalize the existing prefix, retaining a not-yet-created suffix. This
// resolves drive aliases/junction aliases before comparing directory ancestry.
fn resolve_path(path: &Path) -> io::Result<PathBuf> {
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

fn path_key(path: &Path) -> PathBuf {
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

fn overlap(a: &Path, b: &Path) -> bool {
    let a = path_key(a);
    let b = path_key(b);
    a.starts_with(&b) || b.starts_with(&a)
}

fn nearest_existing_directory(path: &Path) -> io::Result<PathBuf> {
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
fn check_directory_chain(path: &Path) -> io::Result<()> {
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

fn probe_writable(destination: &Path) -> io::Result<()> {
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

fn metadata_matches(metadata: &Metadata, planned: &PlannedFile) -> bool {
    metadata.is_file()
        && !is_reparse(metadata)
        && metadata.len() == planned.size
        && metadata.modified().ok() == Some(planned.modified)
}

fn open_regular(path: &Path) -> io::Result<File> {
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

fn exif_date(file: &File) -> Option<NaiveDate> {
    let mut reader = BufReader::new(file);
    let exif = exif::Reader::new().read_from_container(&mut reader).ok()?;
    let field = exif.get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)?;
    if let exif::Value::Ascii(values) = &field.value {
        let value = std::str::from_utf8(values.first()?).ok()?;
        parse_exif_date(value)
    } else {
        None
    }
}

fn parse_exif_date(value: &str) -> Option<NaiveDate> {
    let text = value.trim_matches(|c: char| c == '\0' || c.is_ascii_whitespace());
    // Check the entire date/time, not only its date portion (bad times are bad EXIF).
    NaiveDateTime::parse_from_str(text, "%Y:%m:%d %H:%M:%S")
        .ok()
        .map(|d| d.date())
}

fn local_date(time: SystemTime) -> NaiveDate {
    DateTime::<Local>::from(time).date_naive()
}

fn select_date(
    path: &Path,
    metadata: &Metadata,
    basis: DateBasis,
) -> io::Result<(NaiveDate, String)> {
    match basis {
        DateBasis::TakenFirst => {
            let mut file = open_regular(path)?;
            if let Some(date) = exif_date(&file) {
                return Ok((date, "拍摄日期（EXIF DateTimeOriginal）".into()));
            }
            // The handle can have been advanced by the EXIF reader; no subsequent
            // file reads are needed, but reset it for clarity.
            let _ = file.seek(SeekFrom::Start(0));
            Ok((
                local_date(metadata.modified()?),
                "修改日期（拍摄日期缺失或无效，回退）".into(),
            ))
        }
        DateBasis::Modified => Ok((local_date(metadata.modified()?), "修改日期".into())),
        DateBasis::Created => Ok((local_date(metadata.created()?), "创建日期".into())),
    }
}

fn numbered_path(base: &Path, index: usize) -> PathBuf {
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

/// Scan without creating the archive directory tree or hashing entire files.
/// A short-lived write probe is created and explicitly removed in the nearest
/// existing destination ancestor. Cancellation returns a partial plan + warning.
pub fn scan(
    options: &ScanOptions,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> Result<ScanPlan, String> {
    if options.start > options.end {
        return Err("开始日期不能晚于结束日期".into());
    }
    let source = fs::canonicalize(&options.source).map_err(|e| format!("源目录无效: {e}"))?;
    if !source.is_dir() {
        return Err("源路径不是目录".into());
    }
    let destination =
        resolve_path(&options.destination).map_err(|e| format!("目标路径无效: {e}"))?;
    if overlap(&source, &destination) {
        return Err("源目录和目标目录不能相同，也不能互为父子目录（含路径别名）".into());
    }
    if cancelled(cancel) {
        return Ok(ScanPlan {
            warnings: vec!["扫描已取消".into()],
            ..ScanPlan::default()
        });
    }
    probe_writable(&destination).map_err(|e| format!("目标不可写或探针清理失败: {e}"))?;
    let mut plan = ScanPlan::default();
    let mut visited = 0;
    let mut reserved = HashSet::new();
    let mut walker = WalkDir::new(&source)
        .follow_links(false)
        .sort_by_file_name();
    if !options.recursive {
        walker = walker.max_depth(1);
    }
    let entries = walker.into_iter().filter_entry(|entry| {
        if entry.depth() == 0 {
            return true;
        }
        // symlink_metadata is intentional: metadata() would follow junctions.
        fs::symlink_metadata(entry.path())
            .map(|m| !is_reparse(&m))
            .unwrap_or(true)
    });
    for entry in entries {
        if cancelled(cancel) {
            plan.warnings.push("扫描已取消，当前仅为部分计划".into());
            break;
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                plan.warnings.push(format!("目录读取失败: {error}"));
                continue;
            }
        };
        if entry.depth() == 0 {
            continue;
        }
        visited += 1;
        if image_extension(entry.path()) {
            let candidate = (|| -> io::Result<Option<PlannedFile>> {
                let metadata = fs::symlink_metadata(entry.path())?;
                if !metadata.is_file() || is_reparse(&metadata) {
                    return Ok(None);
                }
                let modified = metadata.modified()?;
                let (date, basis_label) = select_date(entry.path(), &metadata, options.basis)?;
                let after = fs::symlink_metadata(entry.path())?;
                if !after.is_file()
                    || is_reparse(&after)
                    || metadata.len() != after.len()
                    || after.modified()? != modified
                {
                    return Err(unsupported("扫描读取期间文件发生变化"));
                }
                if date < options.start || date > options.end {
                    return Ok(None);
                }
                let base = destination
                    .join(date.format("%Y-%m-%d").to_string())
                    .join(entry.file_name());
                let mut index = 0usize;
                let target = loop {
                    let target = numbered_path(&base, index);
                    if !reserved.contains(&path_key(&target)) {
                        match fs::symlink_metadata(&target) {
                            Err(e) if e.kind() == io::ErrorKind::NotFound => break target,
                            Err(e) => return Err(e),
                            Ok(_) => (),
                        }
                    }
                    index = index
                        .checked_add(1)
                        .ok_or_else(|| unsupported("重名编号溢出"))?;
                };
                reserved.insert(path_key(&target));
                let action = if index == 0 {
                    "待校验：复制时按 SHA-256 校验同日同名及编号文件；相同跳过，冲突编号".into()
                } else {
                    format!("待校验：预计重名编号 ({index})；复制时 SHA-256 相同则跳过，否则重新分配编号")
                };
                Ok(Some(PlannedFile {
                    source: entry.path().to_path_buf(),
                    target,
                    date,
                    basis_label,
                    size: metadata.len(),
                    modified,
                    action,
                }))
            })();
            match candidate {
                Ok(Some(file)) => {
                    plan.total_bytes = plan.total_bytes.saturating_add(file.size);
                    plan.files.push(file);
                }
                Ok(None) => (),
                Err(error) => plan
                    .warnings
                    .push(format!("跳过 {}: {error}", entry.path().display())),
            }
        }
        progress(Progress::Scan {
            visited,
            matched: plan.files.len(),
        });
    }
    Ok(plan)
}

#[derive(Debug)]
enum CopyError {
    Cancelled,
    File(io::Error),
    Target(io::Error),
}

impl From<io::Error> for CopyError {
    fn from(error: io::Error) -> Self {
        Self::File(error)
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), CopyError> {
    if cancelled(cancel) {
        Err(CopyError::Cancelled)
    } else {
        Ok(())
    }
}

fn check_source(file: &File, planned: &PlannedFile) -> Result<(), CopyError> {
    let path_metadata = fs::symlink_metadata(&planned.source)?;
    if !metadata_matches(&file.metadata()?, planned) || !metadata_matches(&path_metadata, planned) {
        return Err(CopyError::File(unsupported(
            "源文件大小、修改时间或文件类型与扫描计划不符",
        )));
    }
    if let Some(parent) = planned.source.parent() {
        check_directory_chain(parent)?;
    }
    Ok(())
}

fn digest_reader(reader: &mut File, cancel: &AtomicBool) -> Result<[u8; 32], CopyError> {
    reader.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        check_cancel(cancel)?;
        let length = reader.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        hash.update(&buffer[..length]);
    }
    Ok(hash.finalize().into())
}

fn same_contents(
    source: &mut File,
    planned: &PlannedFile,
    candidate: &Path,
    source_hash: &mut Option<[u8; 32]>,
    cancel: &AtomicBool,
) -> Result<bool, CopyError> {
    let metadata = fs::symlink_metadata(candidate).map_err(CopyError::Target)?;
    if !metadata.is_file() || is_reparse(&metadata) || metadata.len() != planned.size {
        return Ok(false);
    }
    let mut existing = open_regular(candidate).map_err(CopyError::Target)?;
    if source_hash.is_none() {
        *source_hash = Some(digest_reader(source, cancel)?);
    }
    let target_hash = digest_reader(&mut existing, cancel).map_err(|error| match error {
        CopyError::File(e) => CopyError::Target(e),
        other => other,
    })?;
    check_source(source, planned)?;
    let after = existing.metadata().map_err(CopyError::Target)?;
    let path_after = fs::symlink_metadata(candidate).map_err(CopyError::Target)?;
    if !after.is_file()
        || is_reparse(&after)
        || !path_after.is_file()
        || is_reparse(&path_after)
        || after.len() != metadata.len()
        || after.modified().ok() != metadata.modified().ok()
        || path_after.len() != metadata.len()
        || path_after.modified().ok() != metadata.modified().ok()
    {
        return Err(CopyError::Target(unsupported("校验期间目标文件发生变化")));
    }
    Ok(source_hash.as_ref() == Some(&target_hash))
}

// Enumerate the complete collision family even if there are numbering gaps.
// This is crucial for idempotence when e.g. name.ext was deleted but (3) remains.
fn collision_family(directory: &Path, base: &Path) -> io::Result<Vec<PathBuf>> {
    let stem = base.file_stem().unwrap_or_else(|| OsStr::new("image"));
    let extension = base.extension();
    let mut family = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        if path_key(&path) == path_key(base) {
            family.push((0usize, path));
            continue;
        }
        // File names need not be UTF-8. Match the exact OS-string prefix/suffix
        // by constructing each recognized decimal-number candidate, not lossy
        // converting the original name for equality.
        if let Some(candidate_stem) = path.file_stem().and_then(OsStr::to_str) {
            if let Some(number) = candidate_stem
                .rsplit_once(" (")
                .and_then(|(_, suffix)| suffix.strip_suffix(')'))
            {
                if let Ok(index) = number.parse::<usize>() {
                    if index > 0
                        && number == index.to_string()
                        && path_key(&path) == path_key(&numbered_path(base, index))
                    {
                        family.push((index, path));
                    }
                }
            }
        } else {
            // A non-Unicode source name can still be copied safely. Numbered
            // non-Unicode collision names are handled by OS-string byte/unit
            // matching below.
            if let Some(index) = non_unicode_number(&path, stem, extension) {
                if path_key(&path) == path_key(&numbered_path(base, index)) {
                    family.push((index, path));
                }
            }
        }
    }
    family.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Ok(family.into_iter().map(|(_, path)| path).collect())
}

fn non_unicode_number(path: &Path, stem: &OsStr, extension: Option<&OsStr>) -> Option<usize> {
    let candidate = path.file_stem()?.as_encoded_bytes();
    let prefix = stem.as_encoded_bytes();
    if path.extension() != extension || !candidate.starts_with(prefix) {
        return None;
    }
    let tail = std::str::from_utf8(&candidate[prefix.len()..]).ok()?;
    let number = tail.strip_prefix(" (")?.strip_suffix(')')?;
    let index = number.parse::<usize>().ok()?;
    (index > 0 && number == index.to_string()).then_some(index)
}

fn close_temp(temp: NamedTempFile, report: &mut CopyReport) {
    if let Err(error) = temp.close() {
        report.messages.push(format!(
            "临时文件清理失败（请手动检查 .picsync-*）: {error}"
        ));
    }
}

// Only objective capacity/device/path failures stop the entire run. Individual
// permissions and sharing violations are reported and the next item is tried.
fn globally_unavailable(error: &io::Error) -> bool {
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

// Small injectable I/O boundary: capacity/read failures and mid-stream
// cancellation can be tested without filling or disrupting a real disk.
fn copy_stream(
    source: &mut dyn Read,
    target: &mut dyn Write,
    expected: u64,
    cancel: &AtomicBool,
) -> Result<(), CopyError> {
    let mut buffer = [0u8; 128 * 1024];
    let mut written = 0u64;
    loop {
        check_cancel(cancel)?;
        let length = source.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        written = written
            .checked_add(length as u64)
            .ok_or_else(|| unsupported("文件大小溢出"))?;
        if written > expected {
            return Err(unsupported("复制期间源文件增大").into());
        }
        target
            .write_all(&buffer[..length])
            .map_err(CopyError::Target)?;
    }
    if written != expected {
        return Err(unsupported("复制期间源文件缩短").into());
    }
    Ok(())
}

#[derive(Debug)]
enum Outcome {
    Copied(PathBuf),
    Skipped(PathBuf),
}

fn copy_one(
    planned: &PlannedFile,
    cancel: &AtomicBool,
    report: &mut CopyReport,
) -> Result<Outcome, CopyError> {
    check_cancel(cancel)?;
    let before = fs::symlink_metadata(&planned.source)?;
    if !metadata_matches(&before, planned) {
        return Err(unsupported("源文件已变更或被替换为链接").into());
    }
    let mut source = open_regular(&planned.source)?;
    check_source(&source, planned)?;
    // Access time is captured BEFORE reading the source, not after hashing.
    let atime = FileTime::from_last_access_time(&before);
    let mtime = FileTime::from_last_modification_time(&before);
    let directory = planned
        .target
        .parent()
        .ok_or_else(|| unsupported("目标没有父目录"))?;
    check_directory_chain(directory).map_err(CopyError::Target)?;
    fs::create_dir_all(directory).map_err(CopyError::Target)?;
    check_directory_chain(directory).map_err(CopyError::Target)?;
    let name = planned
        .source
        .file_name()
        .ok_or_else(|| unsupported("源文件没有文件名"))?;
    let base = directory.join(name);
    let mut source_hash = None;
    for candidate in collision_family(directory, &base).map_err(CopyError::Target)? {
        check_cancel(cancel)?;
        match same_contents(&mut source, planned, &candidate, &mut source_hash, cancel) {
            Ok(true) => return Ok(Outcome::Skipped(candidate)),
            // A concurrent removal is benign: enumerate remaining members.
            Err(CopyError::Target(e))
                if e.kind() == io::ErrorKind::NotFound && directory.is_dir() => {}
            Err(error) => return Err(error),
            Ok(false) => (),
        }
    }
    check_source(&source, planned)?;
    source.seek(SeekFrom::Start(0))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".picsync-")
        .tempfile_in(directory)
        .map_err(CopyError::Target)?;
    let write_result = (|| -> Result<(), CopyError> {
        copy_stream(&mut source, &mut temp, planned.size, cancel)?;
        check_source(&source, planned)?;
        check_cancel(cancel)?;
        temp.flush().map_err(CopyError::Target)?;
        filetime::set_file_handle_times(temp.as_file(), Some(atime), Some(mtime))
            .map_err(CopyError::Target)?;
        temp.as_file().sync_all().map_err(CopyError::Target)?;
        Ok(())
    })();
    if let Err(error) = write_result {
        close_temp(temp, report);
        return Err(error);
    }
    let mut index = 0usize;
    loop {
        if let Err(error) = check_cancel(cancel).and_then(|_| check_source(&source, planned)) {
            close_temp(temp, report);
            return Err(error);
        }
        if let Err(error) = check_directory_chain(directory) {
            close_temp(temp, report);
            return Err(CopyError::Target(error));
        }
        let candidate = numbered_path(&base, index);
        match fs::symlink_metadata(&candidate) {
            Ok(_) => {
                match same_contents(&mut source, planned, &candidate, &mut source_hash, cancel) {
                    Ok(true) => {
                        close_temp(temp, report);
                        return Ok(Outcome::Skipped(candidate));
                    }
                    Ok(false) => (),
                    Err(CopyError::Target(e))
                        if e.kind() == io::ErrorKind::NotFound && directory.is_dir() =>
                    {
                        continue
                    }
                    Err(error) => {
                        close_temp(temp, report);
                        return Err(error);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // Verified against tempfile 3.27.0 src/file/imp/windows.rs:
                // persist(overwrite=false) calls MoveFileExW(flags=0), i.e.
                // without MOVEFILE_REPLACE_EXISTING.
                // The library, not an existence-check + overwriting rename,
                // is the authority for atomic non-replacement here.
                match temp.persist_noclobber(&candidate) {
                    Ok(file) => {
                        drop(file);
                        return Ok(Outcome::Copied(candidate));
                    }
                    Err(error) => {
                        let tempfile::PersistError { error, file } = error;
                        temp = file;
                        if error.kind() == io::ErrorKind::AlreadyExists
                            || fs::symlink_metadata(&candidate).is_ok()
                        {
                            // Re-check THIS index: the winner may have identical content.
                            continue;
                        }
                        close_temp(temp, report);
                        return Err(CopyError::Target(error));
                    }
                }
            }
            Err(error) => {
                close_temp(temp, report);
                return Err(CopyError::Target(error));
            }
        }
        index = match index.checked_add(1) {
            Some(index) => index,
            None => {
                close_temp(temp, report);
                return Err(unsupported("重名编号溢出").into());
            }
        };
    }
}

/// Execute a plan. Existing files are never overwritten. Each permission/file
/// error is isolated; target loss, device unavailability and full disks stop work.
/// Cancellation removes this invocation's temporary file, never foreign files.
pub fn copy_plan(
    plan: &ScanPlan,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Progress),
) -> CopyReport {
    let mut report = CopyReport::default();
    for (index, planned) in plan.files.iter().enumerate() {
        if cancelled(cancel) {
            report.cancelled = true;
            break;
        }
        progress(Progress::Copy {
            done: index,
            total: plan.files.len(),
            current: planned.source.display().to_string(),
            copied: report.copied,
            skipped: report.skipped,
            failed: report.failed,
        });
        let mut stop = false;
        match copy_one(planned, cancel, &mut report) {
            Ok(Outcome::Copied(path)) => {
                report.copied += 1;
                report.messages.push(format!("已复制: {}", path.display()));
            }
            Ok(Outcome::Skipped(path)) => {
                report.skipped += 1;
                report
                    .messages
                    .push(format!("内容相同，已跳过: {}", path.display()));
            }
            Err(CopyError::Cancelled) => {
                report.cancelled = true;
            }
            Err(CopyError::File(error)) => {
                report.failed += 1;
                report
                    .messages
                    .push(format!("源文件失败 {}: {error}", planned.source.display()));
            }
            Err(CopyError::Target(error)) => {
                report.failed += 1;
                report.messages.push(format!(
                    "目标操作失败 {}: {error}",
                    planned.target.display()
                ));
                if globally_unavailable(&error) {
                    stop = true;
                    report.messages.push(format!(
                        "目标整体不可用，已停止剩余复制；尚未处理 {} 个文件",
                        plan.files.len().saturating_sub(index + 1)
                    ));
                }
            }
        }
        progress(Progress::Copy {
            done: index + usize::from(!report.cancelled),
            total: plan.files.len(),
            current: planned.source.display().to_string(),
            copied: report.copied,
            skipped: report.skipped,
            failed: report.failed,
        });
        if report.cancelled || stop {
            break;
        }
    }
    if cancelled(cancel) {
        report.cancelled = true;
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};
    use tempfile::TempDir;

    fn fixture() -> (TempDir, ScanOptions) {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::create_dir(&source).unwrap();
        let options = ScanOptions {
            source,
            destination: temp.path().join("destination"),
            start: NaiveDate::from_ymd_opt(2000, 1, 1).unwrap(),
            end: NaiveDate::from_ymd_opt(2100, 1, 1).unwrap(),
            recursive: true,
            basis: DateBasis::Modified,
        };
        (temp, options)
    }
    fn image(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
        filetime::set_file_mtime(
            path,
            FileTime::from_system_time(UNIX_EPOCH + Duration::from_secs(1_735_732_800)),
        )
        .unwrap();
    }
    fn run_scan(options: &ScanOptions) -> ScanPlan {
        scan(options, &AtomicBool::new(false), &mut |_| {}).unwrap()
    }
    fn run_copy(plan: &ScanPlan) -> CopyReport {
        copy_plan(plan, &AtomicBool::new(false), &mut |_| {})
    }
    fn no_temps(path: &Path) -> bool {
        WalkDir::new(path)
            .into_iter()
            .filter_map(Result::ok)
            .all(|e| !e.file_name().to_string_lossy().starts_with(".picsync-"))
    }

    #[test]
    fn injected_read_failure_is_source_error() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("read interrupted"))
            }
        }
        assert!(matches!(
            copy_stream(&mut Broken, &mut Vec::new(), 5, &AtomicBool::new(false)),
            Err(CopyError::File(_))
        ));
    }

    #[test]
    fn injected_disk_full_is_fatal_target_error() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::from_raw_os_error(if cfg!(windows) {
                    112
                } else {
                    28
                }))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let error =
            copy_stream(&mut &b"image"[..], &mut Full, 5, &AtomicBool::new(false)).unwrap_err();
        match error {
            CopyError::Target(e) => assert!(globally_unavailable(&e)),
            _ => panic!("wrong error category"),
        }
    }

    #[test]
    fn mid_stream_cancellation_removes_temp_and_preserves_completed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("completed.jpg"), b"keep").unwrap();
        let cancel = AtomicBool::new(false);
        struct CancellingWriter<'a> {
            temp: &'a mut NamedTempFile,
            cancel: &'a AtomicBool,
        }
        impl Write for CancellingWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let n = self.temp.write(bytes)?;
                self.cancel.store(true, Ordering::Relaxed);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.temp.flush()
            }
        }
        let mut temp = tempfile::Builder::new()
            .prefix(".picsync-")
            .tempfile_in(dir.path())
            .unwrap();
        let bytes = vec![7u8; 400_000];
        let result = copy_stream(
            &mut &bytes[..],
            &mut CancellingWriter {
                temp: &mut temp,
                cancel: &cancel,
            },
            bytes.len() as u64,
            &cancel,
        );
        assert!(matches!(result, Err(CopyError::Cancelled)));
        let mut report = CopyReport::default();
        close_temp(temp, &mut report);
        assert!(report.messages.is_empty());
        assert!(no_temps(dir.path()));
        assert_eq!(fs::read(dir.path().join("completed.jpg")).unwrap(), b"keep");
    }

    #[test]
    fn stream_rejects_growth_and_truncation() {
        for expected in [2, 8] {
            assert!(matches!(
                copy_stream(
                    &mut &b"image"[..],
                    &mut Vec::new(),
                    expected,
                    &AtomicBool::new(false)
                ),
                Err(CopyError::File(_))
            ));
        }
    }

    #[test]
    fn labels_and_supported_extensions() {
        assert_eq!(DateBasis::Modified.label(), "修改日期");
        assert_ne!(DateBasis::TakenFirst.label(), DateBasis::Created.label());
        for ext in [
            "JPG", "jpeg", "PNG", "tif", "tiff", "webp", "bmp", "gif", "heic", "heif",
        ] {
            assert!(image_extension(Path::new(&format!("image.{ext}"))));
        }
        assert!(!image_extension(Path::new("image.raw")));
        assert!(!image_extension(Path::new("image")));
    }
    #[test]
    fn exif_validation() {
        assert_eq!(
            parse_exif_date("2024:02:29 12:34:56\0"),
            NaiveDate::from_ymd_opt(2024, 2, 29)
        );
        for bad in [
            "2023:02:29 12:00:00",
            "2024:02:29 25:00:00",
            "0000:00:00 00:00:00",
            "2024:02:29",
            "garbage",
        ] {
            assert!(parse_exif_date(bad).is_none(), "{bad}");
        }
    }
    #[test]
    fn rejects_invalid_ranges_and_overlapping_paths() {
        let (_temp, mut options) = fixture();
        options.destination = options.source.clone();
        assert!(scan(&options, &AtomicBool::new(false), &mut |_| {}).is_err());
        options.destination = options.source.join("not-created/child");
        assert!(scan(&options, &AtomicBool::new(false), &mut |_| {}).is_err());
        options.destination = options.source.parent().unwrap().to_path_buf();
        assert!(scan(&options, &AtomicBool::new(false), &mut |_| {}).is_err());
        options.start = options.end.succ_opt().unwrap();
        assert!(scan(&options, &AtomicBool::new(false), &mut |_| {}).is_err());
    }
    #[test]
    fn normalized_missing_paths_are_checked() {
        let (_temp, mut options) = fixture();
        options.destination = options.source.join("missing/../archive");
        assert!(scan(&options, &AtomicBool::new(false), &mut |_| {}).is_err());
    }
    #[test]
    fn scan_filters_dates_extensions_and_depth_without_creating_archive() {
        let (temp, mut options) = fixture();
        image(&options.source.join("a.JPG"), b"abc");
        image(&options.source.join("nested/b.png"), b"12345");
        image(&options.source.join("ignored.txt"), b"ignore");
        let plan = run_scan(&options);
        assert_eq!(plan.files.len(), 2);
        assert_eq!(plan.total_bytes, 8);
        assert!(!options.destination.exists());
        assert!(no_temps(temp.path()));
        for file in &plan.files {
            assert_eq!(
                file.target
                    .parent()
                    .unwrap()
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap(),
                file.date.format("%Y-%m-%d").to_string()
            );
            assert!(file.action.contains("待校验"));
        }
        options.recursive = false;
        assert_eq!(run_scan(&options).files.len(), 1);
        options.start = plan.files[0].date;
        options.end = options.start;
        assert_eq!(run_scan(&options).files.len(), 1); // inclusive
        options.start = options.start.succ_opt().unwrap();
        options.end = options.start;
        assert!(run_scan(&options).files.is_empty());
    }
    #[test]
    fn valid_exif_drives_both_filter_and_archive_date() {
        let (_temp, mut options) = fixture();
        // Minimal little-endian TIFF: IFD0 -> ExifIFD -> DateTimeOriginal.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II\x2a\x00\x08\x00\x00\x00");
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x8769u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&26u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x9003u16.to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        tiff.extend_from_slice(&20u32.to_le_bytes());
        tiff.extend_from_slice(&44u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(b"2024:02:29 12:34:56\0");
        image(&options.source.join("exif.tiff"), &tiff);
        options.basis = DateBasis::TakenFirst;
        options.start = NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        options.end = options.start;
        let plan = run_scan(&options);
        assert_eq!(plan.files.len(), 1, "{:?}", plan.warnings);
        assert_eq!(plan.files[0].date, options.start);
        assert!(plan.files[0].basis_label.contains("EXIF"));
        assert_eq!(
            plan.files[0].target.parent().unwrap().file_name().unwrap(),
            "2024-02-29"
        );
        assert_eq!(run_copy(&plan).copied, 1);
    }
    #[test]
    fn created_uses_creation_metadata_without_modified_fallback() {
        let (_temp, mut options) = fixture();
        let path = options.source.join("a.jpg");
        image(&path, b"image");
        options.basis = DateBasis::Created;
        let plan = run_scan(&options);
        match fs::metadata(&path).unwrap().created() {
            Ok(created) => {
                assert_eq!(plan.files.len(), 1);
                assert_eq!(plan.files[0].date, local_date(created));
                assert_eq!(plan.files[0].basis_label, "创建日期");
            }
            Err(_) => {
                assert!(plan.files.is_empty());
                assert_eq!(plan.warnings.len(), 1);
            }
        }
    }
    #[test]
    fn taken_date_falls_back_for_invalid_container() {
        let (_temp, mut options) = fixture();
        image(&options.source.join("invalid.jpg"), b"not a jpeg");
        options.basis = DateBasis::TakenFirst;
        let plan = run_scan(&options);
        assert_eq!(plan.files.len(), 1);
        assert!(plan.files[0].basis_label.contains("回退"));
        assert_eq!(plan.files[0].date, local_date(plan.files[0].modified));
    }
    #[test]
    fn scan_cancel_and_progress() {
        let (_temp, options) = fixture();
        for index in 0..4 {
            image(&options.source.join(format!("{index}.png")), b"a");
        }
        let cancel = AtomicBool::new(false);
        let mut visits = Vec::new();
        let plan = scan(&options, &cancel, &mut |event| {
            if let Progress::Scan { visited, matched } = event {
                visits.push((visited, matched));
                cancel.store(true, Ordering::Relaxed);
            }
        })
        .unwrap();
        assert_eq!(plan.files.len(), 1);
        assert_eq!(visits, [(1, 1)]);
        assert!(!plan.warnings.is_empty());
    }
    #[test]
    fn copies_and_repeat_is_idempotent() {
        let (temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"payload");
        let plan = run_scan(&options);
        let first = run_copy(&plan);
        assert_eq!((first.copied, first.failed), (1, 0), "{:?}", first.messages);
        assert_eq!(fs::read(&plan.files[0].target).unwrap(), b"payload");
        let second = run_copy(&plan);
        assert_eq!((second.copied, second.skipped, second.failed), (0, 1, 0));
        assert!(no_temps(temp.path()));
    }
    #[test]
    fn same_name_different_content_numbers_and_gap_still_deduplicates() {
        let (_temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"new");
        let initial = run_scan(&options);
        let base = initial.files[0].target.clone();
        image(&base, b"old");
        image(&numbered_path(&base, 3), b"new");
        let plan = run_scan(&options);
        assert_eq!(plan.files[0].target, numbered_path(&base, 1));
        let report = run_copy(&plan);
        assert_eq!((report.copied, report.skipped, report.failed), (0, 1, 0));
        fs::remove_file(numbered_path(&base, 3)).unwrap();
        let report = run_copy(&plan);
        assert_eq!((report.copied, report.skipped, report.failed), (1, 0, 0));
        assert_eq!(fs::read(&base).unwrap(), b"old");
        assert_eq!(fs::read(numbered_path(&base, 1)).unwrap(), b"new");
        assert_eq!(run_copy(&plan).skipped, 1);
    }
    #[test]
    fn does_not_deduplicate_unrelated_names_or_dates() {
        let (_temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"same");
        image(&options.source.join("b.jpg"), b"same");
        let plan = run_scan(&options);
        let other_day = options.destination.join("1999-01-01/a.jpg");
        image(&other_day, b"same");
        assert_eq!(run_copy(&plan).copied, 2);
    }
    #[test]
    fn multiple_source_names_share_collision_family() {
        let (_temp, options) = fixture();
        image(&options.source.join("one/a.jpg"), b"first");
        image(&options.source.join("two/a.jpg"), b"second");
        let plan = run_scan(&options);
        assert_ne!(plan.files[0].target, plan.files[1].target);
        let report = run_copy(&plan);
        assert_eq!(
            (report.copied, report.failed),
            (2, 0),
            "{:?}",
            report.messages
        );
        assert_eq!(run_copy(&plan).skipped, 2);
    }
    #[test]
    fn changed_source_fails_but_later_file_continues() {
        let (_temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"first");
        image(&options.source.join("b.jpg"), b"second");
        let plan = run_scan(&options);
        fs::write(&plan.files[0].source, b"changed size").unwrap();
        let report = run_copy(&plan);
        assert_eq!((report.failed, report.copied), (1, 1));
    }
    #[test]
    fn modification_time_change_is_rejected_even_with_same_size() {
        let (_temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"first");
        let plan = run_scan(&options);
        filetime::set_file_mtime(
            &plan.files[0].source,
            FileTime::from_system_time(plan.files[0].modified + Duration::from_secs(10)),
        )
        .unwrap();
        let report = run_copy(&plan);
        assert_eq!((report.failed, report.copied), (1, 0));
    }
    #[test]
    fn preserves_mtime_and_pre_read_atime() {
        let (_temp, options) = fixture();
        let source = options.source.join("a.jpg");
        image(&source, b"first");
        let plan = run_scan(&options);
        let atime = FileTime::from_unix_time(1_700_000_000, 0);
        let mtime = FileTime::from_system_time(plan.files[0].modified);
        filetime::set_file_times(&source, atime, mtime).unwrap();
        let report = run_copy(&plan);
        assert_eq!(report.copied, 1, "{:?}", report.messages);
        let destination = fs::metadata(&plan.files[0].target).unwrap();
        assert_eq!(FileTime::from_last_modification_time(&destination), mtime);
        assert_eq!(FileTime::from_last_access_time(&destination), atime);
    }
    #[test]
    fn cancellation_between_files_emits_progress_and_leaves_no_temps() {
        let (temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"first");
        image(&options.source.join("b.jpg"), b"second");
        let plan = run_scan(&options);
        let cancel = AtomicBool::new(false);
        let mut updates = 0;
        let report = copy_plan(&plan, &cancel, &mut |event| {
            if let Progress::Copy {
                done,
                total,
                copied,
                ..
            } = event
            {
                assert_eq!(total, 2);
                updates += 1;
                if done == 1 {
                    assert_eq!(copied, 1);
                    cancel.store(true, Ordering::Relaxed);
                } else {
                    assert_eq!((done, copied), (0, 0));
                }
            }
        });
        assert!(report.cancelled);
        assert_eq!(report.copied, 1);
        assert_eq!(updates, 2);
        assert!(no_temps(temp.path()));
    }
    #[test]
    fn pre_cancel_does_nothing() {
        let (_temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"first");
        let plan = run_scan(&options);
        let report = copy_plan(&plan, &AtomicBool::new(true), &mut |_| {
            panic!("no progress")
        });
        assert!(report.cancelled);
        assert_eq!(report.copied, 0);
        assert!(!options.destination.exists());
    }
    #[test]
    fn directory_name_conflict_is_numbered_not_overwritten() {
        let (_temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"first");
        let plan = run_scan(&options);
        fs::create_dir_all(&plan.files[0].target).unwrap();
        let report = run_copy(&plan);
        assert_eq!((report.copied, report.failed), (1, 0));
        assert!(plan.files[0].target.is_dir());
        assert!(numbered_path(&plan.files[0].target, 1).is_file());
    }
    #[test]
    fn unavailable_target_parent_stops_run() {
        let (_temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"first");
        image(&options.source.join("b.jpg"), b"second");
        let plan = run_scan(&options);
        fs::write(&options.destination, b"not a directory").unwrap();
        let report = run_copy(&plan);
        assert_eq!(report.failed, 1);
        assert!(report.messages.iter().any(|m| m.contains("尚未处理 1")));
        assert_eq!(report.copied, 0);
        assert_eq!(fs::read(&options.destination).unwrap(), b"not a directory");
    }
    #[test]
    fn tempfile_noclobber_never_overwrites_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("existing.jpg");
        fs::write(&target, b"original").unwrap();
        let mut staged = NamedTempFile::new_in(temp.path()).unwrap();
        staged.write_all(b"replacement").unwrap();
        let error = staged.persist_noclobber(&target).unwrap_err();
        assert_eq!(fs::read(&target).unwrap(), b"original");
        error.file.close().unwrap();
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
    #[test]
    fn pre_cancelled_hash_checks_before_reading() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data");
        fs::write(&path, b"test").unwrap();
        let mut file = File::open(path).unwrap();
        assert!(matches!(
            digest_reader(&mut file, &AtomicBool::new(true)),
            Err(CopyError::Cancelled)
        ));
    }
    #[cfg(windows)]
    #[test]
    fn windows_canonical_verbatim_and_long_paths_work() {
        let (_temp, mut options) = fixture();
        let mut deep = options.source.clone();
        for _ in 0..15 {
            deep.push("long-name-123456789");
        }
        // Add the verbatim prefix before creating paths longer than MAX_PATH.
        let root = fs::canonicalize(&options.source).unwrap();
        let relative = deep.strip_prefix(&options.source).unwrap();
        let deep = root.join(relative);
        image(&deep.join("image.jpg"), b"long path image");
        options.source = root;
        let plan = run_scan(&options);
        assert_eq!(plan.files.len(), 1, "{:?}", plan.warnings);
        let report = run_copy(&plan);
        assert_eq!(
            (report.copied, report.failed),
            (1, 0),
            "{:?}",
            report.messages
        );
    }
    #[cfg(windows)]
    #[test]
    fn windows_locked_source_fails_individually_and_continues() {
        use std::os::windows::fs::OpenOptionsExt;
        let (_temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"locked");
        image(&options.source.join("b.jpg"), b"second");
        let plan = run_scan(&options);
        let _lock = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&plan.files[0].source)
            .unwrap();
        let report = run_copy(&plan);
        assert_eq!(
            (report.failed, report.copied),
            (1, 1),
            "{:?}",
            report.messages
        );
    }
    #[cfg(windows)]
    #[test]
    fn windows_reparse_links_are_skipped_and_replacements_rejected() {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        let (temp, options) = fixture();
        image(&options.source.join("real.jpg"), b"original");
        let outside = temp.path().join("outside");
        image(&outside.join("x.jpg"), b"external");
        if let Err(error) = symlink_dir(&outside, options.source.join("linked")) {
            eprintln!("symlink test unavailable without Developer Mode/privilege: {error}");
            return;
        }
        symlink_file(outside.join("x.jpg"), options.source.join("link.jpg")).unwrap();
        let plan = run_scan(&options);
        assert_eq!(plan.files.len(), 1);
        fs::remove_file(&plan.files[0].source).unwrap();
        symlink_file(outside.join("x.jpg"), &plan.files[0].source).unwrap();
        let report = run_copy(&plan);
        assert_eq!((report.failed, report.copied), (1, 0));
        // A target junction/symlink introduced AFTER scanning is not followed.
        fs::remove_file(&plan.files[0].source).unwrap();
        image(&plan.files[0].source, b"original");
        symlink_dir(&outside, &options.destination).unwrap();
        let report = run_copy(&plan);
        assert_eq!((report.failed, report.copied), (1, 0));
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    }
    #[cfg(windows)]
    #[test]
    fn windows_case_insensitive_paths_overlap() {
        let (_temp, mut options) = fixture();
        options.destination = PathBuf::from(options.source.to_string_lossy().to_uppercase());
        assert!(scan(&options, &AtomicBool::new(false), &mut |_| {}).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn skips_symlink_tree_and_rejects_replaced_source() {
        use std::os::unix::fs::symlink;
        let (temp, options) = fixture();
        image(&options.source.join("real.jpg"), b"original");
        let outside = temp.path().join("outside");
        image(&outside.join("x.jpg"), b"external");
        symlink(&outside, options.source.join("linked")).unwrap();
        symlink(outside.join("x.jpg"), options.source.join("link.jpg")).unwrap();
        let plan = run_scan(&options);
        assert_eq!(plan.files.len(), 1);
        fs::remove_file(&plan.files[0].source).unwrap();
        symlink(outside.join("x.jpg"), &plan.files[0].source).unwrap();
        let report = run_copy(&plan);
        assert_eq!((report.failed, report.copied), (1, 0));
    }
    #[cfg(unix)]
    #[test]
    fn canonical_alias_overlap_is_rejected() {
        use std::os::unix::fs::symlink;
        let (temp, mut options) = fixture();
        let alias = temp.path().join("alias");
        symlink(&options.source, &alias).unwrap();
        options.destination = alias.join("new");
        assert!(scan(&options, &AtomicBool::new(false), &mut |_| {}).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn destination_symlink_replacement_is_not_followed() {
        use std::os::unix::fs::symlink;
        let (temp, options) = fixture();
        image(&options.source.join("a.jpg"), b"first");
        let plan = run_scan(&options);
        let outside = temp.path().join("outside");
        fs::create_dir(&outside).unwrap();
        symlink(&outside, &options.destination).unwrap();
        let report = run_copy(&plan);
        assert_eq!((report.failed, report.copied), (1, 0));
        assert_eq!(fs::read_dir(outside).unwrap().count(), 0);
    }
}

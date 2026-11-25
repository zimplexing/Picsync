//! Conservative, atomic, no-clobber file copying with SHA-256 deduplication.

use super::fs_guard::{
    check_directory_chain, globally_unavailable, is_reparse, metadata_matches, numbered_path,
    open_regular, path_key, unsupported,
};
use super::{cancelled, CopyEvent, CopyReport, PlannedFile, Progress, ScanPlan};
use filetime::FileTime;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;
use tempfile::NamedTempFile;

/// Emit mid-file byte progress only when a single file streams past this stride,
/// keeping small-file runs quiet while large files stay responsive.
const STREAM_PROGRESS_STRIDE: u64 = 1024 * 1024;

#[derive(Debug)]
pub(crate) enum CopyError {
    Cancelled,
    File(io::Error),
    Target(io::Error),
}

impl From<io::Error> for CopyError {
    fn from(error: io::Error) -> Self {
        Self::File(error)
    }
}

type TargetHashCache = HashMap<PathBuf, (u64, Option<SystemTime>, [u8; 32])>;

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

pub(crate) fn digest_reader(reader: &mut File, cancel: &AtomicBool) -> Result<[u8; 32], CopyError> {
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
    target_hashes: &mut TargetHashCache,
    cancel: &AtomicBool,
) -> Result<bool, CopyError> {
    let metadata = fs::symlink_metadata(candidate).map_err(CopyError::Target)?;
    if !metadata.is_file() || is_reparse(&metadata) || metadata.len() != planned.size {
        return Ok(false);
    }
    let key = path_key(candidate);
    let mtime = metadata.modified().ok();
    if let Some((cached_len, cached_mtime, cached_hash)) = target_hashes.get(&key) {
        if *cached_len == metadata.len() && *cached_mtime == mtime {
            if source_hash.is_none() {
                *source_hash = Some(digest_reader(source, cancel)?);
            }
            check_source(source, planned)?;
            return Ok(source_hash.as_ref() == Some(cached_hash));
        }
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
        || after.modified().ok() != mtime
        || path_after.len() != metadata.len()
        || path_after.modified().ok() != mtime
    {
        return Err(CopyError::Target(unsupported("校验期间目标文件发生变化")));
    }
    target_hashes.insert(key, (metadata.len(), mtime, target_hash));
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

pub(crate) fn close_temp(temp: NamedTempFile, report: &mut CopyReport) {
    if let Err(error) = temp.close() {
        report.events.push(CopyEvent::TempCleanupFailed {
            error: error.to_string(),
        });
    }
}

// Small injectable I/O boundary: capacity/read failures and mid-stream
// cancellation can be tested without filling or disrupting a real disk.
pub(crate) fn copy_stream(
    source: &mut dyn Read,
    target: &mut dyn Write,
    expected: u64,
    cancel: &AtomicBool,
    on_bytes: &mut dyn FnMut(u64),
) -> Result<(), CopyError> {
    let mut buffer = [0u8; 128 * 1024];
    let mut written = 0u64;
    let mut next_report = STREAM_PROGRESS_STRIDE;
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
        if written >= next_report && written < expected {
            on_bytes(written);
            next_report = written.saturating_add(STREAM_PROGRESS_STRIDE);
        }
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
    on_bytes: &mut dyn FnMut(u64),
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
    let mut target_hashes = TargetHashCache::new();
    for candidate in collision_family(directory, &base).map_err(CopyError::Target)? {
        check_cancel(cancel)?;
        match same_contents(
            &mut source,
            planned,
            &candidate,
            &mut source_hash,
            &mut target_hashes,
            cancel,
        ) {
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
        copy_stream(&mut source, &mut temp, planned.size, cancel, on_bytes)?;
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
                match same_contents(
                    &mut source,
                    planned,
                    &candidate,
                    &mut source_hash,
                    &mut target_hashes,
                    cancel,
                ) {
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
    let bytes_total = plan.total_bytes;
    let mut bytes_done = 0u64;
    for (index, planned) in plan.files.iter().enumerate() {
        if cancelled(cancel) {
            report.cancelled = true;
            break;
        }
        let current_display = planned.source.display().to_string();
        progress(Progress::Copy {
            done: index,
            total: plan.files.len(),
            bytes_done,
            bytes_total,
            current: current_display.clone(),
            copied: report.copied,
            skipped: report.skipped,
            failed: report.failed,
        });
        let copied_before = report.copied;
        let skipped_before = report.skipped;
        let failed_before = report.failed;
        let mut stop = false;
        let outcome = copy_one(planned, cancel, &mut report, &mut |streamed| {
            progress(Progress::Copy {
                done: index,
                total: plan.files.len(),
                bytes_done: bytes_done.saturating_add(streamed),
                bytes_total,
                current: current_display.clone(),
                copied: copied_before,
                skipped: skipped_before,
                failed: failed_before,
            });
        });
        match outcome {
            Ok(Outcome::Copied(path)) => {
                report.copied += 1;
                report.events.push(CopyEvent::Copied {
                    source: planned.source.clone(),
                    target: path,
                });
            }
            Ok(Outcome::Skipped(path)) => {
                report.skipped += 1;
                report.events.push(CopyEvent::SkippedDuplicate {
                    source: planned.source.clone(),
                    existing: path,
                });
            }
            Err(CopyError::Cancelled) => {
                report.cancelled = true;
            }
            Err(CopyError::File(error)) => {
                report.failed += 1;
                report.events.push(CopyEvent::SourceFailed {
                    source: planned.source.clone(),
                    error: error.to_string(),
                });
            }
            Err(CopyError::Target(error)) => {
                report.failed += 1;
                let fatal = globally_unavailable(&error);
                report.events.push(CopyEvent::TargetFailed {
                    target: planned.target.clone(),
                    error: error.to_string(),
                    fatal,
                });
                if fatal {
                    stop = true;
                    report.events.push(CopyEvent::TargetUnavailableStopped {
                        remaining: plan.files.len().saturating_sub(index + 1),
                    });
                }
            }
        }
        if !report.cancelled {
            bytes_done = bytes_done.saturating_add(planned.size);
        }
        progress(Progress::Copy {
            done: index + usize::from(!report.cancelled),
            total: plan.files.len(),
            bytes_done,
            bytes_total,
            current: current_display,
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

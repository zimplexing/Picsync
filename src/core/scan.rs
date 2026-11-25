//! Read-only directory scanning and archive plan generation.

use super::date::{image_extension, select_date};
use super::fs_guard::{
    is_reparse, numbered_path, overlap, path_key, probe_writable, resolve_path, unsupported,
};
use super::{cancelled, PlannedAction, PlannedFile, Progress, ScanOptions, ScanPlan};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::sync::atomic::AtomicBool;
use walkdir::WalkDir;

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
                let (date, basis) = select_date(entry.path(), &metadata, options.basis)?;
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
                    PlannedAction::Copy
                } else {
                    PlannedAction::NumberedCollision(index)
                };
                Ok(Some(PlannedFile {
                    source: entry.path().to_path_buf(),
                    target,
                    date,
                    basis,
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

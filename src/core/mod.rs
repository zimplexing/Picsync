//! Date-based image planning and conservative, no-clobber copying.
//!
//! Dates are local calendar dates. Archives use `YYYY-MM-DD/name.ext` and
//! collisions use `name (1).ext`, `name (2).ext`, ... within that date only.

mod copy;
mod date;
mod fs_guard;
mod scan;

pub use copy::copy_plan;
pub use scan::scan;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

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

/// The actual date source resolved for a scanned image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolvedBasis {
    ExifOriginal,
    ModifiedFallback,
    Modified,
    Created,
}

impl ResolvedBasis {
    pub fn label(self) -> &'static str {
        match self {
            Self::ExifOriginal => "拍摄日期（EXIF DateTimeOriginal）",
            Self::ModifiedFallback => "修改日期（拍摄日期缺失或无效，回退）",
            Self::Modified => "修改日期",
            Self::Created => "创建日期",
        }
    }
}

/// Planned copy action determined during scan preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlannedAction {
    Copy,
    NumberedCollision(usize),
}

impl PlannedAction {
    pub fn is_collision(self) -> bool {
        matches!(self, Self::NumberedCollision(_))
    }

    pub fn detail(self) -> String {
        match self {
            Self::Copy => {
                "待校验：复制时按 SHA-256 校验同日同名及编号文件；相同跳过，冲突编号".into()
            }
            Self::NumberedCollision(index) => format!(
                "待校验：预计重名编号 ({index})；复制时 SHA-256 相同则跳过，否则重新分配编号"
            ),
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
    pub basis: ResolvedBasis,
    pub size: u64,
    pub modified: SystemTime,
    pub action: PlannedAction,
}

#[derive(Debug, Default, Clone)]
pub struct ScanPlan {
    pub files: Vec<PlannedFile>,
    pub warnings: Vec<String>,
    pub total_bytes: u64,
}

/// Structured per-item outcome recorded during copy execution.
#[derive(Debug, Clone)]
pub enum CopyEvent {
    Copied {
        source: PathBuf,
        target: PathBuf,
    },
    SkippedDuplicate {
        source: PathBuf,
        existing: PathBuf,
    },
    SourceFailed {
        source: PathBuf,
        error: String,
    },
    TargetFailed {
        target: PathBuf,
        error: String,
        fatal: bool,
    },
    TargetUnavailableStopped {
        remaining: usize,
    },
    TempCleanupFailed {
        error: String,
    },
}

impl fmt::Display for CopyEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Copied { target, .. } => write!(f, "已复制: {}", target.display()),
            Self::SkippedDuplicate { existing, .. } => {
                write!(f, "内容相同，已跳过: {}", existing.display())
            }
            Self::SourceFailed { source, error } => {
                write!(f, "源文件失败 {}: {error}", source.display())
            }
            Self::TargetFailed { target, error, .. } => {
                write!(f, "目标操作失败 {}: {error}", target.display())
            }
            Self::TargetUnavailableStopped { remaining } => {
                write!(
                    f,
                    "目标整体不可用，已停止剩余复制；尚未处理 {remaining} 个文件"
                )
            }
            Self::TempCleanupFailed { error } => {
                write!(f, "临时文件清理失败（请手动检查 .picsync-*）: {error}")
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct CopyReport {
    pub copied: usize,
    pub skipped: usize,
    pub failed: usize,
    pub cancelled: bool,
    pub events: Vec<CopyEvent>,
}

impl CopyReport {
    pub fn messages(&self) -> Vec<String> {
        self.events.iter().map(ToString::to_string).collect()
    }
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
        bytes_done: u64,
        bytes_total: u64,
        current: String,
        copied: usize,
        skipped: usize,
        failed: usize,
    },
}

pub(crate) fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::copy::{close_temp, copy_stream, digest_reader, CopyError};
    use super::date::{image_extension, local_date, parse_exif_date};
    use super::fs_guard::{globally_unavailable, numbered_path};
    use super::*;
    use filetime::FileTime;
    use std::fs::{self, File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::path::Path;
    use std::time::{Duration, UNIX_EPOCH};
    use tempfile::{NamedTempFile, TempDir};
    use walkdir::WalkDir;

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
            copy_stream(
                &mut Broken,
                &mut Vec::new(),
                5,
                &AtomicBool::new(false),
                &mut |_| {}
            ),
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
        let error = copy_stream(
            &mut &b"image"[..],
            &mut Full,
            5,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap_err();
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
            &mut |_| {},
        );
        assert!(matches!(result, Err(CopyError::Cancelled)));
        let mut report = CopyReport::default();
        close_temp(temp, &mut report);
        assert!(report.events.is_empty());
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
                    &AtomicBool::new(false),
                    &mut |_| {}
                ),
                Err(CopyError::File(_))
            ));
        }
    }

    #[test]
    fn large_stream_emits_intermediate_byte_progress() {
        let bytes = vec![42u8; 2_500_000];
        let mut reported = Vec::new();
        copy_stream(
            &mut &bytes[..],
            &mut Vec::new(),
            bytes.len() as u64,
            &AtomicBool::new(false),
            &mut |n| reported.push(n),
        )
        .unwrap();
        assert!(!reported.is_empty());
        assert!(reported.windows(2).all(|w| w[0] < w[1]));
        assert!(*reported.last().unwrap() < bytes.len() as u64);
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
            assert_eq!(file.action, PlannedAction::Copy);
            assert!(file.action.detail().contains("待校验"));
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
        assert_eq!(plan.files[0].basis, ResolvedBasis::ExifOriginal);
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
                assert_eq!(plan.files[0].basis, ResolvedBasis::Created);
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
        assert_eq!(plan.files[0].basis, ResolvedBasis::ModifiedFallback);
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
        assert_eq!((first.copied, first.failed), (1, 0), "{:?}", first.events);
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
        assert_eq!(plan.files[0].action, PlannedAction::NumberedCollision(1));
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
        assert_eq!(plan.files[0].action, PlannedAction::Copy);
        assert_eq!(plan.files[1].action, PlannedAction::NumberedCollision(1));
        let report = run_copy(&plan);
        assert_eq!(
            (report.copied, report.failed),
            (2, 0),
            "{:?}",
            report.events
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
        assert_eq!(report.copied, 1, "{:?}", report.events);
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
                bytes_done,
                bytes_total,
                ..
            } = event
            {
                assert_eq!(total, 2);
                assert_eq!(bytes_total, 11);
                updates += 1;
                if done == 1 {
                    assert_eq!(copied, 1);
                    assert_eq!(bytes_done, 5);
                    cancel.store(true, Ordering::Relaxed);
                } else {
                    assert_eq!((done, copied, bytes_done), (0, 0, 0));
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
        assert!(report
            .events
            .iter()
            .any(|e| matches!(e, CopyEvent::TargetUnavailableStopped { remaining: 1 })));
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
            report.events
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
            report.events
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

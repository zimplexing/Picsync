//! Application state machine, job orchestration, and `eframe::App` entry.

mod path_probe;
mod views;
mod worker;

use crate::theme;
use chrono::{Local, NaiveDate};
use eframe::egui;
use path_probe::{PathProbe, PathState};
use picsync::{core, settings};
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use views::{display_path, format_size};
use worker::{Job, WorkerMessage};

pub struct PicsyncApp {
    settings: settings::Settings,
    data_dir: Option<PathBuf>,
    source_text: String,
    destination_text: String,
    range: bool,
    start: NaiveDate,
    end: NaiveDate,
    start_text: String,
    end_text: String,
    plan: Option<Arc<core::ScanPlan>>,
    job: Option<Job>,
    status: String,
    error: Option<String>,
    settings_warning: Option<String>,
    notices: Vec<String>,
    last_log: Option<PathBuf>,
    dirty_since: Option<Instant>,
    fraction: f32,
    current: String,
    close_prompt: bool,
    exit_when_done: bool,
    smoke_started: Option<Instant>,
    logo: Option<egui::TextureHandle>,
    source_state: PathState,
    destination_state: PathState,
    path_probe: Option<PathProbe>,
    paths_dirty: bool,
    last_probe_at: Option<Instant>,
}

impl PicsyncApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::install(&cc.egui_ctx);
        let directory = settings::data_directory();
        let (settings, error) = match &directory {
            Ok(path) => match settings::load(&path.join("settings.json")) {
                Ok(s) => (s, None),
                Err(e) => (settings::Settings::default(), Some(e)),
            },
            Err(e) => (settings::Settings::default(), Some(e.clone())),
        };
        let mut app = Self::from_loaded(settings, directory.ok(), error);
        app.logo = Some(theme::logo_texture(&cc.egui_ctx));
        app.path_probe = Some(PathProbe::new(cc.egui_ctx.clone()));
        app
    }

    fn from_loaded(
        settings: settings::Settings,
        data_dir: Option<PathBuf>,
        error: Option<String>,
    ) -> Self {
        let today = Local::now().date_naive();
        Self {
            source_text: settings.source.to_string_lossy().into_owned(),
            destination_text: settings.destination.to_string_lossy().into_owned(),
            settings,
            data_dir,
            range: false,
            start: today,
            end: today,
            start_text: today.to_string(),
            end_text: today.to_string(),
            plan: None,
            job: None,
            status: "选择文件夹和日期，先扫描预览，再开始复制。".into(),
            error: None,
            settings_warning: error,
            notices: Vec::new(),
            last_log: None,
            dirty_since: None,
            fraction: 0.0,
            current: String::new(),
            close_prompt: false,
            exit_when_done: false,
            smoke_started: std::env::args()
                .any(|a| a == "--smoke-test")
                .then(Instant::now),
            logo: None,
            source_state: PathState::EMPTY,
            destination_state: PathState::EMPTY,
            path_probe: None,
            paths_dirty: true,
            last_probe_at: None,
        }
    }

    pub(super) fn invalidate(&mut self, settings_changed: bool) {
        self.plan = None;
        self.current.clear();
        self.fraction = 0.0;
        self.status = "条件已更新，请重新扫描预览。".into();
        if settings_changed {
            self.dirty_since = Some(Instant::now());
            self.paths_dirty = true;
        }
    }

    fn update_path_probe(&mut self, ctx: &egui::Context) {
        let probe = self
            .path_probe
            .get_or_insert_with(|| PathProbe::new(ctx.clone()));
        if let Some(result) = probe.poll() {
            self.source_state = result.source;
            self.destination_state = result.destination;
        }
        let should_probe = self.paths_dirty
            || self
                .last_probe_at
                .is_none_or(|checked| checked.elapsed() > Duration::from_secs(3));
        if should_probe {
            probe.request(
                self.settings.source.clone(),
                self.settings.destination.clone(),
            );
            self.paths_dirty = false;
            self.last_probe_at = Some(Instant::now());
        }
    }

    pub(super) fn save_settings(&mut self) {
        // Do not overwrite an unreadable/newer config merely by opening/closing.
        if self.dirty_since.is_none() {
            return;
        }
        if let Some(dir) = &self.data_dir {
            match settings::save(&dir.join("settings.json"), &self.settings) {
                Ok(backup) => {
                    self.dirty_since = None;
                    self.settings_warning =
                        backup.map(|p| format!("原配置已备份：{}", p.display()));
                }
                Err(e) => {
                    self.settings_warning = Some(e);
                    self.dirty_since = Some(Instant::now());
                }
            }
        } else {
            self.settings_warning = Some("无法确定配置目录；本次选择无法记住。".into());
            self.dirty_since = Some(Instant::now());
        }
    }

    pub(super) fn options(&self) -> Result<core::ScanOptions, String> {
        let start = parse_date(&self.start_text)?;
        let end = if self.range {
            parse_date(&self.end_text)?
        } else {
            start
        };
        if start > end {
            return Err("开始日期不能晚于结束日期。".into());
        }
        if self.settings.source.as_os_str().is_empty()
            || self.settings.destination.as_os_str().is_empty()
        {
            return Err("请先选择来源文件夹 A 和目标文件夹 B。".into());
        }
        Ok(core::ScanOptions {
            source: self.settings.source.clone(),
            destination: self.settings.destination.clone(),
            start,
            end,
            recursive: self.settings.recursive,
            basis: self.settings.basis,
        })
    }

    pub(super) fn start_scan(&mut self, ctx: &egui::Context) {
        let options = match self.options() {
            Ok(o) => o,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.save_settings();
        self.error = None;
        self.plan = None;
        self.notices.clear();
        self.current.clear();
        self.fraction = 0.0;
        self.status = "正在扫描图片及拍摄日期…".into();
        self.job = Some(Job::spawn_scan(options, self.data_dir.clone(), ctx.clone()));
    }

    pub(super) fn start_copy(&mut self, ctx: &egui::Context) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        self.error = None;
        self.notices.clear();
        self.current.clear();
        self.fraction = 0.0;
        self.status = "正在复制；原始图片不会被移动或删除。".into();
        self.job = Some(Job::spawn_copy(plan, self.data_dir.clone(), ctx.clone()));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let mut finished = false;
        let mut disconnected = false;
        // Drain at most the bounded queue length per frame.
        for _ in 0..17 {
            let Some(job) = self.job.as_ref() else {
                break;
            };
            let message = match job.try_recv() {
                Ok(m) => m,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            };
            match message {
                WorkerMessage::Progress(core::Progress::Scan { visited, matched }) => {
                    self.current = format!("已检查 {visited} 项 · 匹配 {matched} 张图片");
                }
                WorkerMessage::Progress(core::Progress::Copy {
                    done,
                    total,
                    bytes_done,
                    bytes_total,
                    current,
                    copied,
                    skipped,
                    failed,
                }) => {
                    self.fraction = if bytes_total > 0 {
                        (bytes_done as f64 / bytes_total as f64).clamp(0.0, 1.0) as f32
                    } else if total == 0 {
                        0.0
                    } else {
                        done as f32 / total as f32
                    };
                    self.current = format!(
                        "{done}/{total} · {}/{} · 成功 {copied} · 跳过 {skipped} · 失败 {failed} · {}",
                        format_size(bytes_done),
                        format_size(bytes_total),
                        display_path(Path::new(&current))
                    );
                }
                WorkerMessage::Scanned(result, log, warning) => {
                    self.last_log = log;
                    if let Some(w) = warning {
                        self.error = Some(w);
                    }
                    let scan_cancelled = self.job.as_ref().is_some_and(|j| j.is_cancelling());
                    match result {
                        Ok(_) if scan_cancelled => {
                            self.status = "扫描已取消；部分结果未作为可执行计划保留。".into();
                            self.plan = None;
                        }
                        Ok(plan) => {
                            self.status = format!(
                                "扫描完成：{} 张图片，共 {}。确认预览后开始复制。",
                                plan.files.len(),
                                format_size(plan.total_bytes)
                            );
                            self.notices = plan.warnings.iter().take(200).cloned().collect();
                            if plan.warnings.len() > 200 {
                                self.notices.push("更多警告请查看完整日志。".into());
                            }
                            self.plan = Some(Arc::new(plan));
                        }
                        Err(e) => {
                            self.status = "扫描未完成；未开始复制。".into();
                            self.error = Some(e);
                        }
                    }
                    self.current.clear();
                    finished = true;
                }
                WorkerMessage::Copied(report, log, warning) => {
                    self.last_log = log;
                    let total = self.plan.as_ref().map_or(0, |p| p.files.len());
                    let processed = report.copied + report.skipped + report.failed;
                    let title = if report.cancelled {
                        "已取消（已完成图片保留）"
                    } else if processed < total {
                        "任务提前停止，请查看失败原因"
                    } else {
                        "任务结束"
                    };
                    self.status = format!(
                        "{title}：成功 {} 张 · 相同内容跳过 {} 张 · 失败 {} 张 · 未处理 {} 张",
                        report.copied,
                        report.skipped,
                        report.failed,
                        total.saturating_sub(processed)
                    );
                    self.fraction = if total == 0 {
                        0.0
                    } else {
                        processed as f32 / total as f32
                    };
                    self.notices = report.messages().into_iter().take(200).collect();
                    self.notices
                        .push("再次执行前请重新扫描。完整逐项结果保存在本次日志中。".into());
                    if let Some(w) = warning {
                        self.error = Some(w);
                    }
                    self.plan = None;
                    self.current.clear();
                    self.paths_dirty = true;
                    finished = true;
                }
                WorkerMessage::Failed(e) => {
                    self.error = Some(e);
                    self.plan = None;
                    self.status = "任务已停止。".into();
                    finished = true;
                }
            }
            if finished {
                break;
            }
        }
        if disconnected && !finished {
            self.error = Some("后台任务连接中断，请重新扫描。".into());
            self.plan = None;
            finished = true;
        }
        if finished {
            if let Some(job) = self.job.take() {
                job.join();
            }
            if self.exit_when_done {
                self.save_settings();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

impl eframe::App for PicsyncApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if self.logo.is_none() {
            self.logo = Some(theme::logo_texture(ctx));
        }
        self.update_path_probe(ctx);
        let save_delay = if self.settings_warning.is_some() {
            Duration::from_secs(5)
        } else {
            Duration::from_millis(600)
        };
        if self.dirty_since.is_some_and(|t| t.elapsed() >= save_delay) {
            self.save_settings();
        }
        if self.job.is_some() || self.dirty_since.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            if self.job.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.close_prompt = true;
            } else {
                self.save_settings();
            }
        }

        self.header(ctx);
        self.footer(ctx);
        self.content(ctx);
        self.close_dialog(ctx);

        if self
            .smoke_started
            .is_some_and(|t| t.elapsed() > Duration::from_secs(2))
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if self.smoke_started.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}

pub(super) fn parse_date(text: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .filter(|date| date.to_string() == text && text.len() == 10)
        .ok_or_else(|| format!("日期“{text}”无效，请输入 YYYY-MM-DD，例如 2026-11-22。"))
}

#[cfg(test)]
mod tests {
    use super::path_probe::path_state;
    use super::*;

    fn app_at(dir: &Path) -> PicsyncApp {
        PicsyncApp::from_loaded(settings::Settings::default(), Some(dir.to_path_buf()), None)
    }

    #[test]
    fn dates_require_exact_iso_and_valid_calendar() {
        for bad in ["2026-2-01", "2026-02-30", "", "2026-01-01 ", "2026/01/01"] {
            assert!(parse_date(bad).is_err());
        }
        assert!(parse_date("2024-02-29").is_ok());
    }

    #[test]
    fn unreadable_config_is_not_rewritten_until_the_user_changes_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "broken").unwrap();
        let error = settings::load(&path).err();
        let mut app = PicsyncApp::from_loaded(
            settings::Settings::default(),
            Some(dir.path().to_path_buf()),
            error,
        );
        assert!(app.settings_warning.is_some());
        app.save_settings();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "broken");
        app.settings.source = PathBuf::from("F:\\图片");
        app.invalidate(true);
        assert!(app.dirty_since.is_some());
        app.save_settings();
        assert_eq!(
            settings::load(&path).unwrap().source,
            PathBuf::from("F:\\图片")
        );
        assert!(std::fs::read_dir(dir.path()).unwrap().any(|e| {
            e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("settings-recovery-")
        }));
    }

    #[test]
    fn options_reject_reversed_or_invalid_dates_and_empty_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_at(dir.path());
        app.settings.source = dir.path().join("source");
        app.settings.destination = dir.path().join("destination");
        std::fs::create_dir(&app.settings.source).unwrap();
        app.start_text = "2026-11-22".into();
        app.range = true;
        app.end_text = "2026-11-21".into();
        assert!(app.options().is_err());
        app.end_text = "2026-11-22".into();
        assert_eq!(app.options().unwrap().start, app.options().unwrap().end);
        app.start_text = "22-11-2026".into();
        assert!(app.options().is_err());
        app.start_text = "2026-11-22".into();
        app.settings.destination = PathBuf::new();
        assert!(matches!(app.options(), Err(e) if e.contains("目标文件夹")));
    }

    #[test]
    fn new_app_starts_on_today_with_no_plan() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_at(dir.path());
        assert_eq!(app.start, Local::now().date_naive());
        assert_eq!(app.start, app.end);
        assert!(app.plan.is_none() && app.job.is_none());
        assert!(app.settings_warning.is_none());
    }

    #[test]
    fn editing_conditions_invalidates_an_existing_plan() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = app_at(dir.path());
        app.plan = Some(Arc::new(core::ScanPlan::default()));
        app.invalidate(true);
        assert!(app.plan.is_none());
        assert!(app.status.contains("重新扫描"));
    }

    #[test]
    fn path_state_explains_missing_and_creatable_folders() {
        let dir = tempfile::tempdir().unwrap();
        let existing = path_state(dir.path(), false);
        assert_eq!(existing.hint, "文件夹存在");

        let creatable = path_state(&dir.path().join("new/archive"), true);
        assert_eq!(creatable.hint, "文件夹尚不存在，复制时会自动创建");

        let missing = path_state(&dir.path().join("new/archive"), false);
        assert_eq!(missing.hint, "文件夹不存在或无法访问");

        let empty = path_state(Path::new(""), true);
        assert_eq!(empty.hint, "尚未选择文件夹");
    }
}

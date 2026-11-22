#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod theme;

use chrono::{Local, NaiveDate};
use eframe::egui::{self, Color32, RichText};
use picsync::{core, icon, settings};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 866.0])
            .with_min_inner_size([900.0, 700.0])
            .with_icon(std::sync::Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Picsync · 图片时光归档",
        options,
        Box::new(|cc| Ok(Box::new(PicsyncApp::new(cc)))),
    )
}

/// The window icon comes from the same renderer as the executable icon, so the
/// title bar, taskbar and Explorer all show the identical mark.
fn app_icon() -> egui::IconData {
    let size = 128u32;
    egui::IconData {
        rgba: icon::render(size),
        width: size,
        height: size,
    }
}

enum Message {
    Progress(core::Progress),
    Scanned(
        Result<core::ScanPlan, String>,
        Option<PathBuf>,
        Option<String>,
    ),
    Copied(core::CopyReport, Option<PathBuf>, Option<String>),
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum JobKind {
    Scan,
    Copy,
}

struct Job {
    receiver: mpsc::Receiver<Message>,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<()>,
    kind: JobKind,
}

/// Whether a path field points at a usable folder, with a short explanation.
#[derive(Clone, Copy)]
struct PathState {
    color: Color32,
    hint: &'static str,
}

struct PicsyncApp {
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
    paths_checked_at: Option<Instant>,
}

impl PicsyncApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
            source_state: PathState {
                color: theme::INK_FAINT,
                hint: "尚未选择文件夹",
            },
            destination_state: PathState {
                color: theme::INK_FAINT,
                hint: "尚未选择文件夹",
            },
            paths_checked_at: None,
        }
    }

    fn invalidate(&mut self, settings_changed: bool) {
        self.plan = None;
        self.current.clear();
        self.fraction = 0.0;
        self.status = "条件已更新，请重新扫描预览。".into();
        if settings_changed {
            self.dirty_since = Some(Instant::now());
        }
        self.refresh_path_states();
    }

    /// Folder stat calls are cached briefly so a slow network path cannot stall
    /// every frame.
    fn refresh_path_states(&mut self) {
        self.source_state = path_state(&self.settings.source, false);
        self.destination_state = path_state(&self.settings.destination, true);
        self.paths_checked_at = Some(Instant::now());
    }

    fn save_settings(&mut self) {
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

    fn options(&self) -> Result<core::ScanOptions, String> {
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

    fn start_scan(&mut self, ctx: &egui::Context) {
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
        let (sender, receiver) = mpsc::sync_channel(16);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let ctx = ctx.clone();
        let log_dir = self.data_dir.clone();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                core::scan(&options, &worker_cancel, &mut |p| {
                    let _ = sender.try_send(Message::Progress(p));
                    ctx.request_repaint();
                })
            }));
            let message = match result {
                Ok(result) => {
                    let title = match &result {
                        Ok(p) => format!(
                            "扫描完成：匹配 {} 张，{} 字节；A={}；B={}",
                            p.files.len(),
                            p.total_bytes,
                            options.source.display(),
                            options.destination.display()
                        ),
                        Err(e) => format!("扫描未完成：{e}"),
                    };
                    let lines = result
                        .as_ref()
                        .map(|p| p.warnings.as_slice())
                        .unwrap_or(&[]);
                    let (log, warning) = save_log(log_dir, &title, lines);
                    Message::Scanned(result, log, warning)
                }
                Err(_) => {
                    Message::Failed("扫描线程意外终止，未开始复制。请检查日志并重新扫描。".into())
                }
            };
            let _ = sender.send(message);
            ctx.request_repaint();
        });
        self.job = Some(Job {
            receiver,
            cancel,
            handle,
            kind: JobKind::Scan,
        });
    }

    fn start_copy(&mut self, ctx: &egui::Context) {
        let Some(plan) = self.plan.clone() else {
            return;
        };
        self.error = None;
        self.notices.clear();
        self.current.clear();
        self.fraction = 0.0;
        self.status = "正在复制；原始图片不会被移动或删除。".into();
        let (sender, receiver) = mpsc::sync_channel(16);
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let ctx = ctx.clone();
        let log_dir = self.data_dir.clone();
        let handle = std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                core::copy_plan(&plan, &worker_cancel, &mut |p| {
                    let _ = sender.try_send(Message::Progress(p));
                    ctx.request_repaint();
                })
            }));
            let message = match result {
                Ok(report) => {
                    let title = format!(
                        "复制结束：成功 {}，跳过 {}，失败 {}，取消 {}",
                        report.copied, report.skipped, report.failed, report.cancelled
                    );
                    let (log, warning) = save_log(log_dir, &title, &report.messages);
                    Message::Copied(report, log, warning)
                }
                Err(_) => Message::Failed(
                    "复制线程意外终止。已完成图片保留；请检查目标文件夹后重新扫描。".into(),
                ),
            };
            let _ = sender.send(message);
            ctx.request_repaint();
        });
        self.job = Some(Job {
            receiver,
            cancel,
            handle,
            kind: JobKind::Copy,
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let mut finished = false;
        let mut disconnected = false;
        // Drain at most the bounded queue length per frame.
        for _ in 0..17 {
            let Some(job) = self.job.as_ref() else {
                break;
            };
            let message = match job.receiver.try_recv() {
                Ok(m) => m,
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            };
            match message {
                Message::Progress(core::Progress::Scan { visited, matched }) => {
                    self.current = format!("已检查 {visited} 项 · 匹配 {matched} 张图片");
                }
                Message::Progress(core::Progress::Copy {
                    done,
                    total,
                    current,
                    copied,
                    skipped,
                    failed,
                }) => {
                    self.fraction = if total == 0 {
                        0.0
                    } else {
                        done as f32 / total as f32
                    };
                    self.current = format!(
                        "{done}/{total} · 成功 {copied} · 跳过 {skipped} · 失败 {failed} · {}",
                        display_path(Path::new(&current))
                    );
                }
                Message::Scanned(result, log, warning) => {
                    self.last_log = log;
                    if let Some(w) = warning {
                        self.error = Some(w);
                    }
                    let scan_cancelled = self
                        .job
                        .as_ref()
                        .is_some_and(|j| j.cancel.load(Ordering::Relaxed));
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
                Message::Copied(report, log, warning) => {
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
                    self.notices = report.messages.into_iter().take(200).collect();
                    self.notices
                        .push("再次执行前请重新扫描。完整逐项结果保存在本次日志中。".into());
                    if let Some(w) = warning {
                        self.error = Some(w);
                    }
                    self.plan = None;
                    self.current.clear();
                    finished = true;
                }
                Message::Failed(e) => {
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
                let _ = job.handle.join();
            }
            if self.exit_when_done {
                self.save_settings();
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn header(&self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(theme::ACCENT_DEEP)
                    .inner_margin(egui::Margin::symmetric(20, 15)),
            )
            .show(ctx, |ui| {
                theme::header_gradient(ui);
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        egui::Frame::new()
                            .fill(Color32::WHITE)
                            .corner_radius(egui::CornerRadius::same(12))
                            .inner_margin(egui::Margin::same(4))
                            .shadow(egui::Shadow {
                                offset: [0, 2],
                                blur: 8,
                                spread: 0,
                                color: Color32::from_black_alpha(40),
                            })
                            .show(ui, |ui| {
                                ui.add(egui::Image::new((logo.id(), egui::vec2(34.0, 34.0))));
                            });
                        ui.add_space(4.0);
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Picsync")
                                .size(23.0)
                                .strong()
                                .color(Color32::WHITE),
                        );
                        ui.label(
                            RichText::new("图片时光归档 · 按拍摄日期整理照片")
                                .size(12.0)
                                .color(Color32::from_rgb(194, 226, 219)),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        header_badge(ui, "本地处理 · 不上传 · 不改原图");
                    });
                });
            });
    }

    fn footer(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("footer")
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .inner_margin(egui::Margin::symmetric(14, 12)),
            )
            .show(ctx, |ui| {
                // The footer stacks several short rows; a tighter rhythm keeps
                // it from eating the preview's height while a job runs.
                ui.spacing_mut().item_spacing.y = 4.0;
                let panel = ui.clip_rect();
                ui.painter().line_segment(
                    [panel.left_top(), panel.right_top()],
                    egui::Stroke::new(1.0, theme::LINE),
                );
                self.status_row(ui);
                if self
                    .job
                    .as_ref()
                    .is_some_and(|job| job.kind == JobKind::Copy)
                {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        let width = (ui.available_width() - 118.0).max(120.0);
                        ui.add(
                            egui::ProgressBar::new(self.fraction)
                                .desired_width(width)
                                .desired_height(8.0)
                                .fill(theme::ACCENT)
                                .corner_radius(egui::CornerRadius::same(4))
                                .animate(true),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new(format!("{:.0}%", self.fraction * 100.0))
                                    .size(12.0)
                                    .strong()
                                    .color(theme::ACCENT_DEEP),
                            );
                        });
                    });
                }
                if !self.current.is_empty() {
                    ui.add_space(1.0);
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.current)
                                .size(11.5)
                                .color(theme::INK_FAINT),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&self.current);
                }
                if let Some(error) = &self.error {
                    ui.add_space(4.0);
                    theme::alert(ui, theme::Tone::Error, error);
                }
                if let Some(warning) = &self.settings_warning {
                    ui.add_space(4.0);
                    theme::alert(ui, theme::Tone::Warning, warning);
                }
                if !self.notices.is_empty() {
                    ui.add_space(2.0);
                    egui::CollapsingHeader::new(
                        RichText::new(format!("结果与提示 · {} 条", self.notices.len()))
                            .size(12.5)
                            .color(theme::INK_SOFT),
                    )
                    .id_salt("notices")
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(110.0)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                for line in &self.notices {
                                    ui.label(RichText::new(line).size(12.0).color(theme::INK_SOFT));
                                }
                            });
                    });
                }
                // While a job runs the footer reports live progress, so the
                // previous run's log path is left out to keep the panel short.
                if self.job.is_none() {
                    if let Some(log) = &self.last_log {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("本次日志：{}", display_path(log)))
                                    .size(11.0)
                                    .color(theme::INK_FAINT),
                            )
                            .truncate(),
                        )
                        .on_hover_text(display_path(log));
                    }
                }
                ui.add_space(4.0);
                let divider_y = ui.cursor().top();
                ui.painter().line_segment(
                    [
                        egui::pos2(panel.left(), divider_y),
                        egui::pos2(panel.right(), divider_y),
                    ],
                    egui::Stroke::new(1.0, theme::LINE_SOFT),
                );
                ui.add_space(6.0);
                self.actions(ui, ctx);
            });
    }

    fn status_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            let dot = if self.error.is_some() {
                theme::ERROR
            } else if self.job.is_some() {
                theme::WARN
            } else {
                theme::ACCENT
            };
            ui.painter().circle_filled(rect.center(), 3.5, dot);
            let width = (ui.available_width() - 176.0).max(140.0);
            ui.allocate_ui_with_layout(
                egui::vec2(width, 20.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&self.status)
                                .size(13.5)
                                .strong()
                                .color(theme::INK),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&self.status);
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.job.is_some() {
                    let kind = self.job.as_ref().map(|job| job.kind);
                    let text = if kind == Some(JobKind::Scan) {
                        "扫描中"
                    } else {
                        "复制中"
                    };
                    theme::status_pill(ui, text, theme::Tone::Info);
                } else if let Some(plan) = &self.plan {
                    theme::pill(
                        ui,
                        &format!(
                            "{} 张 · {}",
                            plan.files.len(),
                            format_size(plan.total_bytes)
                        ),
                        theme::ACCENT_DEEP,
                        theme::ACCENT_TINT,
                    );
                }
            });
        });
    }

    fn actions(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            let idle = self.job.is_none();
            if theme::button(
                ui,
                "扫描预览",
                theme::Kind::Secondary,
                idle,
                egui::vec2(120.0, 40.0),
            )
            .clicked()
            {
                self.start_scan(ctx);
            }
            let can_copy = idle
                && self
                    .plan
                    .as_ref()
                    .is_some_and(|plan| !plan.files.is_empty());
            if theme::button(
                ui,
                "开始复制",
                theme::Kind::Primary,
                can_copy,
                egui::vec2(132.0, 40.0),
            )
            .clicked()
            {
                self.start_copy(ctx);
            }
            if let Some(job) = &self.job {
                if theme::button(
                    ui,
                    "取消任务",
                    theme::Kind::Ghost,
                    !job.cancel.load(Ordering::Relaxed),
                    egui::vec2(96.0, 40.0),
                )
                .clicked()
                {
                    job.cancel.store(true, Ordering::Relaxed);
                    self.status = "正在取消，请等待当前读取停止并清理临时文件…".into();
                }
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if theme::button(
                    ui,
                    "打开目标",
                    theme::Kind::Ghost,
                    true,
                    egui::vec2(88.0, 34.0),
                )
                .clicked()
                {
                    if self.settings.destination.is_dir() {
                        if let Err(e) = std::process::Command::new("explorer.exe")
                            .arg(&self.settings.destination)
                            .spawn()
                        {
                            self.error = Some(format!("无法打开目标：{e}"));
                        }
                    } else {
                        self.error = Some("目标文件夹尚不存在。".into());
                    }
                }
                if theme::button(
                    ui,
                    "日志目录",
                    theme::Kind::Ghost,
                    true,
                    egui::vec2(88.0, 34.0),
                )
                .clicked()
                {
                    if let Some(dir) = &self.data_dir {
                        match std::fs::create_dir_all(dir).and_then(|_| {
                            std::process::Command::new("explorer.exe")
                                .arg(dir)
                                .spawn()
                                .map(|_| ())
                        }) {
                            Ok(()) => {}
                            Err(e) => self.error = Some(format!("无法打开日志目录：{e}")),
                        }
                    }
                }
            });
        });
    }

    fn content(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(theme::CANVAS)
                    .inner_margin(egui::Margin::symmetric(20, 16)),
            )
            .show(ctx, |ui| {
                // The preview card takes whatever height the folder and date
                // cards leave over. On a small window that is not enough for a
                // readable table, so the whole column scrolls instead.
                let viewport = ui.available_height();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let top = ui.cursor().top();
                        self.configuration(ui);
                        ui.add_space(10.0);
                        let used = ui.cursor().top() - top;
                        // Never claim more than the viewport offers: the outer
                        // scroll area handles the rare case of a tiny window.
                        let preview_height = (viewport - used - 14.0).max(120.0);

                        self.preview(ui, preview_height);
                    });
            });
    }

    fn configuration(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let mut settings_changed = false;
        theme::card(ui, None, |ui| {
            theme::section_header(ui, "1", "选择文件夹", |ui| {
                ui.label(
                    RichText::new("路径会自动记住")
                        .size(11.5)
                        .color(theme::INK_FAINT),
                );
            });
            ui.add_space(8.0);

            let source = path_row(
                ui,
                "来源 A",
                &mut self.source_text,
                "待整理图片所在的文件夹",
                self.source_state,
                "选择来源文件夹 A",
                &self.settings.source,
            );
            if source.edited {
                self.settings.source = PathBuf::from(&self.source_text);
                changed = true;
                settings_changed = true;
            }
            if let Some(path) = source.picked {
                self.source_text = path.to_string_lossy().into_owned();
                self.settings.source = path;
                changed = true;
                settings_changed = true;
            }

            ui.add_space(2.0);
            let destination = path_row(
                ui,
                "目标 B",
                &mut self.destination_text,
                "归档后的图片保存位置",
                self.destination_state,
                "选择目标文件夹 B",
                &self.settings.destination,
            );
            if destination.edited {
                self.settings.destination = PathBuf::from(&self.destination_text);
                changed = true;
                settings_changed = true;
            }
            if let Some(path) = destination.picked {
                self.destination_text = path.to_string_lossy().into_owned();
                self.settings.destination = path;
                changed = true;
                settings_changed = true;
            }

            theme::hairline(ui);
            theme::section_header(ui, "2", "设置日期", |ui| {
                ui.label(
                    RichText::new("范围包含首尾两天")
                        .size(11.5)
                        .color(theme::INK_FAINT),
                );
            });
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                theme::segment_group(ui, |ui| {
                    if theme::segment(ui, "某一天", !self.range).clicked() {
                        self.range = false;
                        changed = true;
                    }
                    if theme::segment(ui, "日期范围", self.range).clicked() {
                        self.range = true;
                        changed = true;
                    }
                });
                ui.add_space(8.0);
                ui.label(
                    RichText::new(if self.range { "开始" } else { "日期" })
                        .size(13.0)
                        .color(theme::INK_SOFT),
                );
                changed |= date_control(ui, "start", &mut self.start, &mut self.start_text);
                if self.range {
                    ui.label(RichText::new("至").size(13.0).color(theme::INK_SOFT));
                    changed |= date_control(ui, "end", &mut self.end, &mut self.end_text);
                }
            });

            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("日期依据").size(13.0).color(theme::INK_SOFT));
                let before = self.settings.basis;
                egui::ComboBox::from_id_salt("basis")
                    .selected_text(
                        RichText::new(self.settings.basis.label())
                            .size(13.0)
                            .color(theme::INK),
                    )
                    .width(190.0)
                    .show_ui(ui, |ui| {
                        for basis in [
                            core::DateBasis::TakenFirst,
                            core::DateBasis::Modified,
                            core::DateBasis::Created,
                        ] {
                            ui.selectable_value(
                                &mut self.settings.basis,
                                basis,
                                RichText::new(basis.label()).size(13.0),
                            );
                        }
                    });
                if before != self.settings.basis {
                    changed = true;
                    settings_changed = true;
                }
                ui.add_space(12.0);
                let checkbox = ui
                    .scope(|ui| {
                        let widgets = &mut ui.style_mut().visuals.widgets;
                        widgets.inactive.fg_stroke = egui::Stroke::new(1.0, theme::ACCENT);
                        widgets.hovered.fg_stroke = egui::Stroke::new(1.0, theme::ACCENT_DEEP);
                        widgets.active.fg_stroke = egui::Stroke::new(1.0, theme::ACCENT_DEEP);
                        ui.checkbox(
                            &mut self.settings.recursive,
                            RichText::new("包含子文件夹").size(13.0).color(theme::INK),
                        )
                    })
                    .inner;
                if checkbox.changed() {
                    changed = true;
                    settings_changed = true;
                }
            });

            let rule = match self.settings.basis {
                core::DateBasis::TakenFirst => {
                    "没有有效拍摄日期时使用修改日期；按同一日期筛选和归档。"
                }
                core::DateBasis::Modified => {
                    "按文件修改日期筛选与归档，不读取拍摄日期。范围包含首尾两天。"
                }
                core::DateBasis::Created => {
                    "按文件创建日期筛选与归档；无法读取创建日期时报告失败，不回退。"
                }
            };
            ui.add_space(3.0);
            ui.label(RichText::new(rule).size(11.5).color(theme::INK_FAINT));
        });
        if changed {
            self.invalidate(settings_changed);
        }
    }

    fn preview(&self, ui: &mut egui::Ui, height: f32) {
        let content_height = theme::card_inner_height(height);
        theme::card(ui, Some(content_height), |ui| {
            theme::section_header(ui, "3", "预览与复制", |ui| {
                if let Some(plan) = &self.plan {
                    theme::pill(
                        ui,
                        &format!(
                            "{} 张 · {}",
                            plan.files.len(),
                            format_size(plan.total_bytes)
                        ),
                        theme::ACCENT_DEEP,
                        theme::ACCENT_TINT,
                    );
                } else {
                    ui.label(
                        RichText::new("确认无误后再开始复制")
                            .size(11.5)
                            .color(theme::INK_FAINT),
                    );
                }
            });
            ui.add_space(2.0);
            let available = ui.available_height();
            match &self.plan {
                Some(plan) if plan.files.is_empty() => {
                    centered_note(
                        ui,
                        (available - 2.0).max(56.0),
                        "所选日期内没有匹配图片",
                        "请尝试其他日期，或把“日期依据”换成修改日期。",
                    );
                }
                Some(plan) => self.preview_table(ui, plan),
                None => self.empty_state(ui, (available - 2.0).max(56.0)),
            }
        });
    }

    fn empty_state(&self, ui: &mut egui::Ui, height: f32) {
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::top_down(egui::Align::Center),
            |ui| {
                // Keep the empty state centred as one block, and drop the
                // optional lines when the card is too short to hold them.
                // Gaps below are explicit, so the default item spacing is off.
                ui.spacing_mut().item_spacing.y = 0.0;
                let compact = height < 150.0;
                let block = if compact { 86.0 } else { 149.0 };
                ui.add_space(((height - block) * 0.5).max(0.0));
                if let Some(logo) = &self.logo {
                    let side = if compact { 38.0 } else { 44.0 };
                    ui.add(egui::Image::new((logo.id(), egui::vec2(side, side))));
                    ui.add_space(if compact { 4.0 } else { 6.0 });
                }
                ui.label(
                    RichText::new("让每一天的照片，各归其位。")
                        .size(if compact { 17.0 } else { 19.0 })
                        .strong()
                        .color(theme::INK),
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new("选择文件夹和日期，扫描后在这里预览匹配到的图片。")
                        .size(12.5)
                        .color(theme::INK_SOFT),
                );
                if !compact {
                    ui.add_space(8.0);
                    theme::pill(
                        ui,
                        "支持 JPG · PNG · TIFF · WebP · BMP · GIF · HEIC",
                        theme::INK_FAINT,
                        Color32::from_rgb(243, 247, 245),
                    );
                    ui.add_space(5.0);
                    ui.label(
                        RichText::new("归档示例：目标文件夹 / 2026-11-22 / IMG_0001.jpg")
                            .size(11.0)
                            .color(theme::INK_FAINT),
                    );
                }
            },
        );
    }

    fn preview_table(&self, ui: &mut egui::Ui, plan: &core::ScanPlan) {
        const HEADER: f32 = 32.0;
        // The card pins this ui's max rect, so the real space left for the
        // scrolling body is exact: no assumptions about spacing in between.
        let body_height = (ui.max_rect().bottom() - ui.cursor().top() - HEADER - 16.0).max(40.0);
        // Rows carry their own 30 px height; without this the table adds an item
        // gap between them and a partially visible row peeks out of the card.
        let spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = ui.available_width();
        let band = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(width, HEADER));
        ui.painter().rect_filled(
            band,
            egui::CornerRadius::same(8),
            Color32::from_rgb(241, 246, 244),
        );
        egui_extras::TableBuilder::new(ui)
            .id_salt("preview")
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            // The default minimum scroll height (200 px) would make a short
            // table taller than the card that hosts it.
            .min_scrolled_height(0.0)
            .max_scroll_height(body_height)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(egui_extras::Column::initial(168.0).at_least(110.0))
            .column(egui_extras::Column::initial(105.0).at_least(85.0))
            .column(egui_extras::Column::initial(120.0).at_least(80.0))
            .column(egui_extras::Column::initial(78.0).at_least(60.0))
            .column(egui_extras::Column::remainder().at_least(150.0))
            .column(egui_extras::Column::initial(132.0).at_least(96.0))
            .header(HEADER, |mut header| {
                for text in [
                    "图片 / 来源",
                    "归档日期",
                    "日期依据",
                    "大小",
                    "预计目标",
                    "计划操作",
                ] {
                    header.col(|ui| {
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(text)
                                .size(11.5)
                                .strong()
                                .color(theme::INK_SOFT),
                        );
                    });
                }
            })
            .body(|body| {
                body.rows(28.0, plan.files.len(), |mut row| {
                    let file = &plan.files[row.index()];
                    row.col(|ui| {
                        let name = file
                            .source
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        ui.add(
                            egui::Label::new(RichText::new(name).size(13.0).color(theme::INK))
                                .truncate(),
                        )
                        .on_hover_text(display_path(&file.source));
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(file.date.to_string())
                                .size(12.0)
                                .color(theme::INK_SOFT),
                        );
                    });
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(&file.basis_label)
                                    .size(12.0)
                                    .color(theme::INK_SOFT),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&file.basis_label);
                    });
                    row.col(|ui| {
                        ui.label(
                            RichText::new(format_size(file.size))
                                .size(12.0)
                                .color(theme::INK_SOFT),
                        );
                    });
                    row.col(|ui| {
                        let target = display_path(&file.target);
                        ui.add(
                            egui::Label::new(
                                RichText::new(target.clone())
                                    .monospace()
                                    .size(11.5)
                                    .color(theme::INK_SOFT),
                            )
                            .truncate(),
                        )
                        .on_hover_text(target);
                    });
                    row.col(|ui| {
                        if file.action.contains("预计") {
                            theme::pill(ui, "重名待校验", theme::WARN, theme::WARN_TINT)
                                .on_hover_text(&file.action);
                        } else {
                            theme::pill(ui, "待复制", theme::ACCENT_DEEP, theme::ACCENT_TINT)
                                .on_hover_text(&file.action);
                        }
                    });
                });
            });
        ui.spacing_mut().item_spacing = spacing;
    }

    fn close_dialog(&mut self, ctx: &egui::Context) {
        if !self.close_prompt {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("close_prompt"))
            .frame(
                egui::Frame::new()
                    .fill(theme::SURFACE)
                    .corner_radius(egui::CornerRadius::same(16))
                    .inner_margin(egui::Margin::same(22))
                    .shadow(egui::Shadow {
                        offset: [0, 12],
                        blur: 36,
                        spread: 0,
                        color: Color32::from_black_alpha(48),
                    }),
            )
            .show(ctx, |ui| {
                ui.set_width(360.0);
                ui.label(
                    RichText::new("任务仍在运行")
                        .size(17.0)
                        .strong()
                        .color(theme::INK),
                );
                ui.add_space(6.0);
                ui.label(
                    RichText::new("现在退出会取消任务；已经复制完成的图片会保留。")
                        .size(13.0)
                        .color(theme::INK_SOFT),
                );
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if theme::button(
                        ui,
                        "留在应用中",
                        theme::Kind::Secondary,
                        true,
                        egui::vec2(112.0, 36.0),
                    )
                    .clicked()
                    {
                        self.close_prompt = false;
                        self.exit_when_done = false;
                    }
                    if self
                        .job
                        .as_ref()
                        .is_some_and(|job| job.cancel.load(Ordering::Relaxed))
                    {
                        ui.label(
                            RichText::new("任务正在取消…")
                                .size(11.5)
                                .color(theme::INK_FAINT),
                        );
                    }
                    if theme::button(
                        ui,
                        "取消并退出",
                        theme::Kind::Primary,
                        true,
                        egui::vec2(112.0, 36.0),
                    )
                    .clicked()
                    {
                        if let Some(job) = &self.job {
                            job.cancel.store(true, Ordering::Relaxed);
                        }
                        self.exit_when_done = true;
                        self.close_prompt = false;
                        if self.job.is_none() {
                            self.save_settings();
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                });
            });
        if modal.should_close() {
            self.close_prompt = false;
            self.exit_when_done = false;
        }
    }
}

impl eframe::App for PicsyncApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        if self.logo.is_none() {
            self.logo = Some(theme::logo_texture(ctx));
        }
        if self
            .paths_checked_at
            .is_none_or(|checked| checked.elapsed() > Duration::from_secs(1))
        {
            self.refresh_path_states();
        }
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

fn header_badge(ui: &mut egui::Ui, text: &str) {
    let galley = ui.painter().layout(
        text.to_owned(),
        egui::FontId::new(11.5, egui::FontFamily::Proportional),
        Color32::from_rgb(226, 243, 238),
        f32::INFINITY,
    );
    let size = galley.size() + egui::vec2(18.0, 9.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect(
        rect,
        egui::CornerRadius::same(20),
        Color32::from_white_alpha(16),
        egui::Stroke::new(1.0, Color32::from_white_alpha(26)),
        egui::StrokeKind::Inside,
    );
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, theme::INK);
}

fn centered_note(ui: &mut egui::Ui, height: f32, title: &str, detail: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::top_down(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let block = 46.0;
            ui.add_space(((height - block) * 0.5).max(0.0));
            ui.label(
                RichText::new(title)
                    .size(16.0)
                    .strong()
                    .color(theme::INK_SOFT),
            );
            ui.add_space(3.0);
            ui.label(RichText::new(detail).size(12.5).color(theme::INK_FAINT));
        },
    );
}

struct PathRow {
    edited: bool,
    picked: Option<PathBuf>,
}

fn path_row(
    ui: &mut egui::Ui,
    label: &str,
    text: &mut String,
    hint: &str,
    state: PathState,
    dialog_title: &str,
    current: &Path,
) -> PathRow {
    let mut edited = false;
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(56.0, 34.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(RichText::new(label).size(13.0).color(theme::INK_SOFT));
            },
        );
        let button_width = 92.0;
        let field_width = (ui.available_width() - button_width - 26.0).max(200.0);
        let response = ui.add_sized(
            [field_width, 34.0],
            egui::TextEdit::singleline(text)
                .hint_text(hint)
                .font(egui::FontId::monospace(12.5))
                .margin(egui::Margin::symmetric(10, 8))
                .vertical_align(egui::Align::Center),
        );
        edited = response.changed();
        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.5, state.color);
        if theme::button(
            ui,
            "选择…",
            theme::Kind::Secondary,
            true,
            egui::vec2(button_width, 34.0),
        )
        .on_hover_text(state.hint)
        .clicked()
        {
            let mut dialog = rfd::FileDialog::new().set_title(dialog_title);
            if current.is_dir() {
                dialog = dialog.set_directory(current);
            }
            picked = dialog.pick_folder();
        }
    });
    PathRow { edited, picked }
}

fn path_state(path: &Path, may_create: bool) -> PathState {
    if path.as_os_str().is_empty() {
        return PathState {
            color: theme::INK_FAINT,
            hint: "尚未选择文件夹",
        };
    }
    if path.is_dir() {
        return PathState {
            color: theme::ACCENT,
            hint: "文件夹存在",
        };
    }
    if may_create
        && path
            .ancestors()
            .skip(1)
            .take(8)
            .any(|parent| parent.is_dir())
    {
        return PathState {
            color: theme::ACCENT,
            hint: "文件夹尚不存在，复制时会自动创建",
        };
    }
    PathState {
        color: theme::WARN,
        hint: "文件夹不存在或无法访问",
    }
}

fn date_control(ui: &mut egui::Ui, id: &str, value: &mut NaiveDate, text: &mut String) -> bool {
    let mut changed = ui
        .add(
            egui::TextEdit::singleline(text)
                .id_salt((id, "text"))
                .desired_width(104.0)
                .margin(egui::Margin::symmetric(9, 7))
                .font(egui::FontId::monospace(12.5))
                .vertical_align(egui::Align::Center)
                .hint_text("YYYY-MM-DD"),
        )
        .changed();
    if changed {
        if let Ok(date) = parse_date(text) {
            *value = date;
        }
    }
    let selected = ui
        .add(
            egui_extras::DatePickerButton::new(value)
                .id_salt(id)
                .format("选择日期")
                .show_icon(false)
                .calendar_week(false),
        )
        .changed();
    if selected {
        *text = value.to_string();
        changed = true;
    }
    changed
}

fn parse_date(text: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d")
        .ok()
        .filter(|date| date.to_string() == text && text.len() == 10)
        .ok_or_else(|| format!("日期“{text}”无效，请输入 YYYY-MM-DD，例如 2026-11-22。"))
}

/// Paths are kept in their verbatim form for Windows long-path safety, but the
/// `\\?\` prefix is noise in the interface.
fn display_path(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text
    }
}

fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} 字节")
    } else if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    }
}

fn save_log(
    directory: Option<PathBuf>,
    title: &str,
    lines: &[String],
) -> (Option<PathBuf>, Option<String>) {
    match directory {
        Some(dir) => match settings::write_log(&dir, title, lines) {
            Ok(path) => (Some(path), None),
            Err(e) => (None, Some(e)),
        },
        None => (None, Some("无法确定日志目录，本次日志未保存。".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_at(dir: &std::path::Path) -> PicsyncApp {
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

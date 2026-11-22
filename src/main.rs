#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use chrono::{Local, NaiveDate};
use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily, RichText};
use picsync::{core, settings};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const ACCENT: Color32 = Color32::from_rgb(15, 118, 110);
const MUTED: Color32 = Color32::from_rgb(95, 111, 126);

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 820.0])
            .with_min_inner_size([900.0, 720.0])
            .with_icon(std::sync::Arc::new(app_icon())),
        ..Default::default()
    };
    eframe::run_native(
        "Picsync · 图片时光归档",
        options,
        Box::new(|cc| Ok(Box::new(PicsyncApp::new(cc)))),
    )
}

/// A small self-contained icon: teal rounded tile with a white photo motif.
/// Drawn in code so the executable needs no extra asset at runtime.
fn app_icon() -> egui::IconData {
    let size = 64u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let inside_round_rect = |x: f32, y: f32| {
        let (left, top, right, bottom, radius) = (4.0f32, 4.0, 60.0, 60.0, 14.0);
        let cx = x.clamp(left + radius, right - radius);
        let cy = y.clamp(top + radius, bottom - radius);
        (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
    };
    let in_triangle = |x: f32, y: f32, a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
        let sign = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| {
            (p.0 - r.0) * (q.1 - r.1) - (q.0 - r.0) * (p.1 - r.1)
        };
        let d1 = sign((x, y), a, b);
        let d2 = sign((x, y), b, c);
        let d3 = sign((x, y), c, a);
        let negative = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let positive = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(negative && positive)
    };
    let motif = |x: f32, y: f32| {
        ((x - 25.0).powi(2) + (y - 25.0).powi(2) <= 16.0)
            || in_triangle(x, y, (13.0, 50.0), (29.0, 30.0), (41.0, 50.0))
            || in_triangle(x, y, (33.0, 50.0), (43.0, 37.0), (53.0, 50.0))
    };
    for y in 0..size {
        for x in 0..size {
            let mut tile: f32 = 0.0;
            let mut white: f32 = 0.0;
            for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                let px = x as f32 + dx;
                let py = y as f32 + dy;
                if inside_round_rect(px, py) {
                    tile += 0.25;
                    if motif(px, py) {
                        white += 0.25;
                    }
                }
            }
            let mix = |base: u8, top: u8| {
                (base as f32 * (1.0 - white) + top as f32 * white).round() as u8
            };
            rgba.extend_from_slice(&[
                mix(ACCENT.r(), 255),
                mix(ACCENT.g(), 255),
                mix(ACCENT.b(), 255),
                (tile * 255.0).round() as u8,
            ]);
        }
    }
    egui::IconData {
        rgba,
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
}

impl PicsyncApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = FontDefinitions::default();
        fonts.font_data.insert(
            "NotoSansSC".into(),
            Arc::new(FontData::from_static(include_bytes!(
                "../assets/NotoSansSC.ttf"
            ))),
        );
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "NotoSansSC".into());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push("NotoSansSC".into());
        cc.egui_ctx.set_fonts(fonts);
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.visuals.selection.bg_fill = ACCENT;
        style.visuals.widgets.active.bg_fill = ACCENT;
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));
        cc.egui_ctx.set_style(style);
        let directory = settings::data_directory();
        let (settings, error) = match &directory {
            Ok(path) => match settings::load(&path.join("settings.json")) {
                Ok(s) => (s, None),
                Err(e) => (settings::Settings::default(), Some(e)),
            },
            Err(e) => (settings::Settings::default(), Some(e.clone())),
        };
        Self::from_loaded(settings, directory.ok(), error)
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
                    self.current = format!("{done}/{total} · 成功 {copied} · 跳过 {skipped} · 失败 {failed} · {current}");
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

    fn configuration(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let mut settings_changed = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new("01  选择文件夹").strong().color(ACCENT));
            ui.label(RichText::new("路径会自动记住").small().color(MUTED));
        });
        let path_width = (ui.available_width() - 200.0).max(360.0);
        ui.horizontal(|ui| {
            ui.label("来源 A");
            if ui
                .add_sized(
                    [path_width, 30.0],
                    egui::TextEdit::singleline(&mut self.source_text)
                        .hint_text("待整理图片所在的文件夹"),
                )
                .changed()
            {
                self.settings.source = PathBuf::from(&self.source_text);
                changed = true;
                settings_changed = true;
            }
            if ui.button("选择来源…").clicked() {
                let mut dialog = rfd::FileDialog::new().set_title("选择来源文件夹 A");
                if self.settings.source.is_dir() {
                    dialog = dialog.set_directory(&self.settings.source);
                }
                if let Some(path) = dialog.pick_folder() {
                    self.source_text = path.to_string_lossy().into_owned();
                    self.settings.source = path;
                    changed = true;
                    settings_changed = true;
                }
            }
        });
        ui.horizontal(|ui| {
            ui.label("目标 B");
            if ui
                .add_sized(
                    [path_width, 30.0],
                    egui::TextEdit::singleline(&mut self.destination_text)
                        .hint_text("归档后的图片保存位置"),
                )
                .changed()
            {
                self.settings.destination = PathBuf::from(&self.destination_text);
                changed = true;
                settings_changed = true;
            }
            if ui.button("选择目标…").clicked() {
                let mut dialog = rfd::FileDialog::new().set_title("选择目标文件夹 B");
                if self.settings.destination.is_dir() {
                    dialog = dialog.set_directory(&self.settings.destination);
                }
                if let Some(path) = dialog.pick_folder() {
                    self.destination_text = path.to_string_lossy().into_owned();
                    self.settings.destination = path;
                    changed = true;
                    settings_changed = true;
                }
            }
        });
        ui.add_space(3.0);
        ui.separator();
        ui.label(RichText::new("02  设置日期").strong().color(ACCENT));
        ui.horizontal(|ui| {
            changed |= ui
                .selectable_value(&mut self.range, false, "某一天")
                .changed();
            changed |= ui
                .selectable_value(&mut self.range, true, "日期范围")
                .changed();
            ui.separator();
            ui.label(if self.range { "开始" } else { "日期" });
            changed |= date_control(ui, "start", &mut self.start, &mut self.start_text);
            if self.range {
                ui.label("至");
                changed |= date_control(ui, "end", &mut self.end, &mut self.end_text);
            }
        });
        ui.horizontal(|ui| {
            ui.label("日期依据");
            let before = self.settings.basis;
            egui::ComboBox::from_id_salt("basis")
                .selected_text(self.settings.basis.label())
                .width(230.0)
                .show_ui(ui, |ui| {
                    for basis in [
                        core::DateBasis::TakenFirst,
                        core::DateBasis::Modified,
                        core::DateBasis::Created,
                    ] {
                        ui.selectable_value(&mut self.settings.basis, basis, basis.label());
                    }
                });
            if before != self.settings.basis {
                changed = true;
                settings_changed = true;
            }
            if ui
                .checkbox(&mut self.settings.recursive, "包含子文件夹")
                .changed()
            {
                changed = true;
                settings_changed = true;
            }
        });
        let rule = match self.settings.basis {
            core::DateBasis::TakenFirst => {
                "没有有效拍摄日期时使用修改日期；按同一日期筛选和归档。范围包含首尾两天。"
            }
            core::DateBasis::Modified => {
                "按文件修改日期筛选与归档，不读取拍摄日期。范围包含首尾两天。"
            }
            core::DateBasis::Created => {
                "按文件创建日期筛选与归档；无法读取创建日期时报告失败，不回退。"
            }
        };
        ui.label(RichText::new(rule).small().color(MUTED));
        if changed {
            self.invalidate(settings_changed);
        }
    }

    fn preview(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("03  预览与复制").strong().color(ACCENT));
            if let Some(plan) = &self.plan {
                ui.label(format!(
                    "{} 张 · {}",
                    plan.files.len(),
                    format_size(plan.total_bytes)
                ));
            }
        });
        if let Some(plan) = &self.plan {
            if plan.files.is_empty() {
                ui.add_space(22.0);
                ui.centered_and_justified(|ui| {
                    ui.label("所选日期内没有匹配图片。请尝试其他日期或日期依据。");
                });
                return;
            }
            egui_extras::TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(egui_extras::Column::initial(145.0).at_least(90.0))
                .column(egui_extras::Column::initial(100.0))
                .column(egui_extras::Column::initial(130.0))
                .column(egui_extras::Column::initial(75.0))
                .column(egui_extras::Column::remainder().at_least(150.0))
                .column(egui_extras::Column::initial(140.0))
                .header(30.0, |mut header| {
                    for text in [
                        "图片 / 来源",
                        "归档日期",
                        "日期依据",
                        "大小",
                        "预计目标",
                        "计划操作",
                    ] {
                        header.col(|ui| {
                            ui.strong(text);
                        });
                    }
                })
                .body(|body| {
                    body.rows(32.0, plan.files.len(), |mut row| {
                        let f = &plan.files[row.index()];
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(
                                    f.source.file_name().unwrap_or_default().to_string_lossy(),
                                )
                                .truncate(),
                            )
                            .on_hover_text(display_path(&f.source));
                        });
                        row.col(|ui| {
                            ui.label(f.date.to_string());
                        });
                        row.col(|ui| {
                            ui.add(egui::Label::new(&f.basis_label).truncate())
                                .on_hover_text(&f.basis_label);
                        });
                        row.col(|ui| {
                            ui.label(format_size(f.size));
                        });
                        row.col(|ui| {
                            let target = display_path(&f.target);
                            ui.add(egui::Label::new(target.clone()).truncate())
                                .on_hover_text(target);
                        });
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(if f.action.contains("预计") {
                                    "重名，待内容校验"
                                } else {
                                    "待复制 / 内容校验"
                                })
                                .truncate(),
                            )
                            .on_hover_text(&f.action);
                        });
                    });
                });
        } else {
            ui.add_space(26.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("让每一天的照片，各归其位。")
                        .size(21.0)
                        .color(MUTED),
                );
                ui.add_space(10.0);
                ui.label("扫描后在这里查看匹配图片，确认无误再复制。");
                ui.label(
                    RichText::new("归档示例：目标文件夹 / 2026-11-22 / IMG_0001.jpg")
                        .small()
                        .color(MUTED),
                );
            });
        }
    }
}

impl eframe::App for PicsyncApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
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
        egui::TopBottomPanel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(236, 247, 244))
                    .inner_margin(20.0),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Picsync").size(30.0).strong().color(ACCENT));
                    ui.separator();
                    ui.label(RichText::new("图片时光归档").size(21.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("本地处理 · 不上传 · 不改原图")
                                .small()
                                .color(MUTED),
                        );
                    });
                });
            });
        let footer_fill = ctx.style().visuals.panel_fill;
        egui::TopBottomPanel::bottom("footer")
            .frame(
                egui::Frame::new()
                    .fill(footer_fill)
                    .inner_margin(14.0)
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(222, 230, 228))),
            )
            .show(ctx, |ui| {
                ui.label(RichText::new(&self.status).color(ACCENT));
                if self.job.is_some() && self.job.as_ref().is_some_and(|j| j.kind == JobKind::Copy)
                {
                    ui.add(egui::ProgressBar::new(self.fraction).show_percentage());
                }
                if !self.current.is_empty() {
                    ui.add(egui::Label::new(&self.current).truncate())
                        .on_hover_text(&self.current);
                }
                if let Some(error) = &self.error {
                    ui.colored_label(Color32::from_rgb(180, 45, 45), error);
                }
                if let Some(warning) = &self.settings_warning {
                    ui.colored_label(Color32::from_rgb(150, 95, 25), warning);
                }
                if !self.notices.is_empty() {
                    egui::CollapsingHeader::new(format!(
                        "结果与提示（{} 条显示）",
                        self.notices.len()
                    ))
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .max_height(120.0)
                            .show(ui, |ui| {
                                for line in &self.notices {
                                    ui.label(line);
                                }
                            });
                    });
                }
                if let Some(log) = &self.last_log {
                    ui.add(
                        egui::Label::new(
                            RichText::new(format!("本次日志：{}", display_path(log)))
                                .small()
                                .color(MUTED),
                        )
                        .truncate(),
                    )
                    .on_hover_text(display_path(log));
                }
                ui.separator();
                // The action row is last so that growing status text never
                // moves the buttons out from under the user's pointer.
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.job.is_none(),
                            egui::Button::new("扫描预览").min_size(egui::vec2(120.0, 38.0)),
                        )
                        .clicked()
                    {
                        self.start_scan(ctx);
                    }
                    let can_copy = self.job.is_none()
                        && self.plan.as_ref().is_some_and(|p| !p.files.is_empty());
                    let copy_button = if can_copy {
                        egui::Button::new(RichText::new("开始复制").color(Color32::WHITE))
                            .fill(ACCENT)
                    } else {
                        egui::Button::new("开始复制")
                    };
                    if ui
                        .add_enabled(can_copy, copy_button.min_size(egui::vec2(120.0, 38.0)))
                        .clicked()
                    {
                        self.start_copy(ctx);
                    }
                    if let Some(job) = &self.job {
                        if ui
                            .add_enabled(
                                !job.cancel.load(Ordering::Relaxed),
                                egui::Button::new("取消任务"),
                            )
                            .clicked()
                        {
                            job.cancel.store(true, Ordering::Relaxed);
                            self.status = "正在取消，请等待当前读取停止并清理临时文件…".into();
                        }
                        ui.spinner();
                        ui.label(if job.kind == JobKind::Scan {
                            "扫描中"
                        } else {
                            "复制中"
                        });
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("打开目标").clicked() {
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
                        if ui.button("日志目录").clicked() {
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
            });
        let panel_fill = ctx.style().visuals.panel_fill;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(20.0).fill(panel_fill))
            .show(ctx, |ui| {
                ui.add_enabled_ui(self.job.is_none(), |ui| self.configuration(ui));
                ui.add_space(8.0);
                ui.separator();
                self.preview(ui);
            });
        if self.close_prompt {
            egui::Window::new("任务仍在运行")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("取消任务后退出？已经复制完成的图片会保留。");
                    ui.horizontal(|ui| {
                        if ui.button("留在应用中").clicked() {
                            self.close_prompt = false;
                            self.exit_when_done = false;
                        }
                        if self
                            .job
                            .as_ref()
                            .is_some_and(|j| j.cancel.load(Ordering::Relaxed))
                        {
                            ui.label("任务仍会完成取消。");
                        }
                        if ui.button("取消并退出").clicked() {
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
        }
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

fn date_control(ui: &mut egui::Ui, id: &str, value: &mut NaiveDate, text: &mut String) -> bool {
    let mut changed = ui
        .add(
            egui::TextEdit::singleline(text)
                .id_salt((id, "text"))
                .desired_width(108.0)
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
}

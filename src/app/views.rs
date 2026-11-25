//! UI panels and view widgets for `PicsyncApp`.

use super::path_probe::PathState;
use super::worker::JobKind;
use super::{parse_date, PicsyncApp};
use crate::theme;
use chrono::NaiveDate;
use eframe::egui::{self, Color32, RichText};
use picsync::core;
use std::path::{Path, PathBuf};

impl PicsyncApp {
    pub(super) fn header(&self, ctx: &egui::Context) {
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

    pub(super) fn footer(&mut self, ctx: &egui::Context) {
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
                    .is_some_and(|job| job.kind() == JobKind::Copy)
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
                if let Some(job) = &self.job {
                    let text = if job.kind() == JobKind::Scan {
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
                    !job.is_cancelling(),
                    egui::vec2(96.0, 40.0),
                )
                .clicked()
                {
                    job.cancel();
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

    pub(super) fn content(&mut self, ctx: &egui::Context) {
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
                        let basis = file.basis.label();
                        ui.add(
                            egui::Label::new(
                                RichText::new(basis).size(12.0).color(theme::INK_SOFT),
                            )
                            .truncate(),
                        )
                        .on_hover_text(basis);
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
                        let detail = file.action.detail();
                        if file.action.is_collision() {
                            theme::pill(ui, "重名待校验", theme::WARN, theme::WARN_TINT)
                                .on_hover_text(detail);
                        } else {
                            theme::pill(ui, "待复制", theme::ACCENT_DEEP, theme::ACCENT_TINT)
                                .on_hover_text(detail);
                        }
                    });
                });
            });
        ui.spacing_mut().item_spacing = spacing;
    }

    pub(super) fn close_dialog(&mut self, ctx: &egui::Context) {
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
                    if self.job.as_ref().is_some_and(|job| job.is_cancelling()) {
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
                            job.cancel();
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

/// Paths are kept in their verbatim form for Windows long-path safety, but the
/// `\\?\` prefix is noise in the interface.
pub(super) fn display_path(path: &Path) -> String {
    let text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text
    }
}

pub(super) fn format_size(bytes: u64) -> String {
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

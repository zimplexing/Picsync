//! Visual language for Picsync: palette, typography, and the small set of
//! painted widgets the interface is built from.
//!
//! Everything here is presentation only. The screens in `main.rs` describe what
//! the application does; this module decides how it looks: a warm canvas, white
//! cards with hairline borders, a teal brand gradient and one primary action per
//! screen.

use eframe::egui::{
    self, Color32, CornerRadius, FontFamily, FontId, Margin, Response, RichText, Stroke, TextStyle,
    Vec2,
};
use std::sync::Arc;

/// Brand teal used for primary actions and emphasis.
pub const ACCENT: Color32 = Color32::from_rgb(15, 118, 110);
/// Darker teal for pressed states and dark text on tinted fills.
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(10, 76, 70);
/// Hover state for primary actions.
pub const ACCENT_HOVER: Color32 = Color32::from_rgb(12, 99, 92);
/// Very light teal wash for tinted surfaces.
pub const ACCENT_TINT: Color32 = Color32::from_rgb(235, 247, 244);
/// Slightly stronger teal wash for badges and selected segments.
pub const ACCENT_TINT_STRONG: Color32 = Color32::from_rgb(214, 238, 232);

pub const INK: Color32 = Color32::from_rgb(25, 37, 34);
pub const INK_SOFT: Color32 = Color32::from_rgb(93, 108, 102);
pub const INK_FAINT: Color32 = Color32::from_rgb(134, 147, 141);
pub const LINE: Color32 = Color32::from_rgb(222, 231, 227);
pub const LINE_SOFT: Color32 = Color32::from_rgb(238, 243, 240);
pub const CANVAS: Color32 = Color32::from_rgb(245, 247, 245);
pub const SURFACE: Color32 = Color32::from_rgb(255, 255, 255);
pub const WARN: Color32 = Color32::from_rgb(161, 78, 5);
pub const WARN_TINT: Color32 = Color32::from_rgb(253, 245, 232);
pub const ERROR: Color32 = Color32::from_rgb(176, 35, 25);
pub const ERROR_TINT: Color32 = Color32::from_rgb(254, 237, 235);
pub const DISABLED_FILL: Color32 = Color32::from_rgb(238, 242, 240);

pub const CARD_RADIUS: u8 = 14;
const CARD_MARGIN: i8 = 16;
const WIDGET_RADIUS: u8 = 9;

/// Install fonts, force the light theme and apply Picsync's style.
pub fn install(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some((name, bytes)) = load_system_chinese_font() {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(egui::FontData::from_owned(bytes)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name.to_owned());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(name.to_owned());
    }
    ctx.set_fonts(fonts);

    // The app is designed light-only: pin the theme so a dark Windows session
    // cannot swap the palette underneath the custom style.
    ctx.options_mut(|options| {
        options.theme_preference = egui::ThemePreference::Light;
        options.fallback_theme = egui::Theme::Light;
    });

    let mut style = (*ctx.style()).clone();
    style.visuals = light_visuals();
    style.spacing.item_spacing = egui::vec2(9.0, 9.0);
    style.spacing.button_padding = egui::vec2(13.0, 8.0);
    style.spacing.interact_size = egui::vec2(56.0, 30.0);
    style.spacing.icon_width = 14.0;
    style.spacing.icon_width_inner = 7.0;
    style.spacing.menu_margin = Margin::same(8);
    style.spacing.window_margin = Margin::same(14);
    style.spacing.indent = 16.0;
    style.spacing.scroll = egui::style::ScrollStyle::solid();
    style.spacing.scroll.bar_width = 9.0;
    style.spacing.scroll.bar_inner_margin = 3.0;
    style.spacing.scroll.bar_outer_margin = 2.0;
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(21.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(14.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(12.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
    ctx.set_style(style);
}

fn light_visuals() -> egui::Visuals {
    let mut visuals = egui::Visuals::light();
    visuals.dark_mode = false;
    visuals.override_text_color = Some(INK);
    visuals.panel_fill = CANVAS;
    visuals.window_fill = SURFACE;
    visuals.window_stroke = Stroke::new(1.0, LINE);
    visuals.window_corner_radius = CornerRadius::same(CARD_RADIUS);
    visuals.window_shadow = egui::Shadow {
        offset: [0, 10],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(38),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 6],
        blur: 18,
        spread: 0,
        color: Color32::from_black_alpha(28),
    };
    visuals.menu_corner_radius = CornerRadius::same(10);
    visuals.extreme_bg_color = Color32::from_rgb(250, 252, 251);
    visuals.faint_bg_color = Color32::from_rgb(248, 251, 249);
    visuals.code_bg_color = Color32::from_rgb(240, 246, 243);
    visuals.selection = egui::style::Selection {
        bg_fill: ACCENT_TINT_STRONG,
        stroke: Stroke::new(1.0, ACCENT),
    };
    visuals.hyperlink_color = ACCENT;
    visuals.warn_fg_color = WARN;
    visuals.error_fg_color = ERROR;

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.bg_fill = SURFACE;
    widgets.noninteractive.weak_bg_fill = SURFACE;
    widgets.noninteractive.bg_stroke = Stroke::new(1.0, LINE_SOFT);
    widgets.noninteractive.fg_stroke = Stroke::new(1.0, INK);
    widgets.noninteractive.corner_radius = CornerRadius::same(WIDGET_RADIUS);
    widgets.noninteractive.expansion = 0.0;

    widgets.inactive.bg_fill = Color32::from_rgb(243, 247, 245);
    widgets.inactive.weak_bg_fill = SURFACE;
    widgets.inactive.bg_stroke = Stroke::new(1.0, LINE);
    widgets.inactive.fg_stroke = Stroke::new(1.0, INK);
    widgets.inactive.corner_radius = CornerRadius::same(WIDGET_RADIUS);
    widgets.inactive.expansion = 0.0;

    widgets.hovered.bg_fill = ACCENT_TINT;
    widgets.hovered.weak_bg_fill = Color32::from_rgb(248, 252, 250);
    widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT_TINT_STRONG);
    widgets.hovered.fg_stroke = Stroke::new(1.0, ACCENT_DEEP);
    widgets.hovered.corner_radius = CornerRadius::same(WIDGET_RADIUS);
    widgets.hovered.expansion = 0.0;

    widgets.active.bg_fill = ACCENT_TINT_STRONG;
    widgets.active.weak_bg_fill = ACCENT_TINT;
    widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    widgets.active.fg_stroke = Stroke::new(1.0, ACCENT_DEEP);
    widgets.active.corner_radius = CornerRadius::same(WIDGET_RADIUS);
    widgets.active.expansion = 0.0;

    widgets.open.bg_fill = SURFACE;
    widgets.open.weak_bg_fill = SURFACE;
    widgets.open.bg_stroke = Stroke::new(1.0, LINE);
    widgets.open.fg_stroke = Stroke::new(1.0, INK);
    widgets.open.corner_radius = CornerRadius::same(WIDGET_RADIUS);
    widgets.open.expansion = 0.0;

    visuals
}

/// Load the brand mark as a texture for the header and empty state.
pub fn logo_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let pixels = picsync::icon::render(128);
    let image = egui::ColorImage::from_rgba_unmultiplied([128, 128], &pixels);
    ctx.load_texture("picsync-logo", image, egui::TextureOptions::LINEAR)
}

/// A white card with a hairline border and a soft shadow.
///
/// Passing `height` pins the card's content to an exact height, so callers can
/// fill the space a layout has left over and read the real remainder back with
/// `Ui::available_height`.
pub fn card<R>(ui: &mut egui::Ui, height: Option<f32>, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0, LINE))
        .corner_radius(CornerRadius::same(CARD_RADIUS))
        .inner_margin(Margin::same(CARD_MARGIN))
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(14),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(height) = height {
                ui.set_min_height(height);
                ui.set_max_height(height);
            }
            add(ui)
        })
        .inner
}

/// Content height of a card drawn into an outer height (margins and border).
pub fn card_inner_height(outer: f32) -> f32 {
    (outer - 2.0 * f32::from(CARD_MARGIN) - 2.0).max(48.0)
}

/// Numbered section heading with an optional right-aligned trailing widget.
pub fn section_header(
    ui: &mut egui::Ui,
    index: &str,
    title: &str,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    ui.horizontal(|ui| {
        let size = 22.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
        ui.painter()
            .circle_filled(rect.center(), size * 0.5, ACCENT_TINT_STRONG);
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            index,
            FontId::new(11.0, FontFamily::Proportional),
            ACCENT_DEEP,
        );
        ui.add_space(3.0);
        ui.label(RichText::new(title).size(16.5).strong().color(INK));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            trailing(ui);
        });
    });
}

/// A hairline divider that spans the current content width.
pub fn hairline(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    let y = ui.cursor().top();
    let left = ui.max_rect().left();
    let right = ui.max_rect().right();
    ui.painter().line_segment(
        [egui::pos2(left, y), egui::pos2(right, y)],
        Stroke::new(1.0, LINE_SOFT),
    );
    ui.add_space(9.0);
}

/// The brand gradient behind the window header.
pub fn header_gradient(ui: &egui::Ui) {
    let rect = ui.clip_rect();
    let mut mesh = egui::Mesh::default();
    let first = mesh.vertices.len() as u32;
    mesh.colored_vertex(rect.left_top(), ACCENT_DEEP);
    mesh.colored_vertex(rect.right_top(), ACCENT);
    mesh.colored_vertex(rect.right_bottom(), ACCENT);
    mesh.colored_vertex(rect.left_bottom(), ACCENT_DEEP);
    mesh.add_triangle(first, first + 1, first + 2);
    mesh.add_triangle(first, first + 2, first + 3);
    ui.painter().add(egui::Shape::mesh(mesh));

    let glow = egui::pos2(rect.right() - 54.0, rect.center().y - 26.0);
    ui.painter()
        .circle_filled(glow, 128.0, Color32::from_white_alpha(4));
    ui.painter()
        .circle_stroke(glow, 94.0, Stroke::new(1.0, Color32::from_white_alpha(11)));
}

/// A fully rounded label: status, counts, supported formats.
pub fn pill(ui: &mut egui::Ui, text: &str, fg: Color32, bg: Color32) -> Response {
    let font = FontId::new(11.5, FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout(text.to_owned(), font, fg, f32::INFINITY);
    let size = galley.size() + Vec2::new(16.0, 7.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(20), bg);
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, fg);
    response
}

/// Background treatment for the footer's status pills.
pub fn status_pill(ui: &mut egui::Ui, text: &str, tone: Tone) -> Response {
    let (fg, bg) = tone.colors();
    pill(ui, text, fg, bg)
}

/// Semantic tone for pills and inline alerts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warning,
    Error,
}

impl Tone {
    fn colors(self) -> (Color32, Color32) {
        match self {
            Self::Info => (ACCENT_DEEP, ACCENT_TINT),
            Self::Warning => (WARN, WARN_TINT),
            Self::Error => (ERROR, ERROR_TINT),
        }
    }
}

/// A tinted inline notice with a colored edge, for errors and warnings that
/// must be readable without opening a dialog.
pub fn alert(ui: &mut egui::Ui, tone: Tone, text: &str) {
    let (fg, bg) = tone.colors();
    let width = ui.available_width();
    let galley = ui.painter().layout(
        text.to_owned(),
        FontId::new(12.5, FontFamily::Proportional),
        fg,
        (width - 30.0).max(40.0),
    );
    let size = Vec2::new(width, galley.size().y + 13.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    let edge = egui::Rect::from_min_size(
        rect.min + Vec2::new(0.0, 6.0),
        Vec2::new(3.0, rect.height() - 12.0),
    );
    ui.painter().rect_filled(edge, CornerRadius::same(2), fg);
    ui.painter()
        .galley(rect.min + Vec2::new(13.0, 6.5), galley, fg);
}

/// Button emphasis.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Primary,
    Secondary,
    Ghost,
}

/// A button in one of the three app styles, with hover/pressed/disabled looks.
pub fn button(ui: &mut egui::Ui, label: &str, kind: Kind, enabled: bool, size: Vec2) -> Response {
    let (fill, hover, pressed, stroke, text) = match kind {
        Kind::Primary => (
            ACCENT,
            ACCENT_HOVER,
            ACCENT_DEEP,
            Stroke::NONE,
            Color32::WHITE,
        ),
        Kind::Secondary => (
            SURFACE,
            ACCENT_TINT,
            ACCENT_TINT_STRONG,
            Stroke::new(1.0, LINE),
            ACCENT_DEEP,
        ),
        Kind::Ghost => (
            Color32::TRANSPARENT,
            ACCENT_TINT,
            ACCENT_TINT_STRONG,
            Stroke::NONE,
            INK_SOFT,
        ),
    };
    ui.scope(|ui| {
        let widgets = &mut ui.style_mut().visuals.widgets;
        let radius = CornerRadius::same(WIDGET_RADIUS);
        widgets.inactive.weak_bg_fill = if enabled { fill } else { DISABLED_FILL };
        widgets.inactive.bg_stroke = if enabled { stroke } else { Stroke::NONE };
        widgets.inactive.fg_stroke = Stroke::new(1.0, text);
        widgets.inactive.corner_radius = radius;
        widgets.hovered.weak_bg_fill = if enabled { hover } else { DISABLED_FILL };
        widgets.hovered.bg_stroke = stroke;
        widgets.hovered.fg_stroke = Stroke::new(1.0, text);
        widgets.hovered.corner_radius = radius;
        widgets.active.weak_bg_fill = if enabled { pressed } else { DISABLED_FILL };
        widgets.active.bg_stroke = stroke;
        widgets.active.fg_stroke = Stroke::new(1.0, text);
        widgets.active.corner_radius = radius;
        widgets.noninteractive.weak_bg_fill = DISABLED_FILL;
        widgets.noninteractive.bg_stroke = Stroke::NONE;
        widgets.noninteractive.fg_stroke = Stroke::new(1.0, INK_FAINT);
        widgets.noninteractive.corner_radius = radius;

        let text = RichText::new(label).size(14.0);
        let text = if enabled { text } else { text.color(INK_FAINT) };
        ui.add_enabled(enabled, egui::Button::new(text).min_size(size).frame(true))
    })
    .inner
}

/// One choice inside a segmented control.
pub fn segment(ui: &mut egui::Ui, label: &str, selected: bool) -> Response {
    if selected {
        ui.add(
            egui::Button::new(
                RichText::new(label)
                    .size(13.5)
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(ACCENT)
            .stroke(Stroke::NONE)
            .corner_radius(CornerRadius::same(8))
            .min_size(Vec2::new(84.0, 27.0)),
        )
    } else {
        let response = button(ui, label, Kind::Ghost, true, Vec2::new(84.0, 27.0));
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    }
}

/// The sandboxed container that hosts a segmented control.
pub fn segment_group<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(Color32::from_rgb(239, 245, 243))
        .corner_radius(CornerRadius::same(11))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            ui.horizontal(add).inner
        })
        .inner
}

/// Load the default Windows Chinese UI font at runtime so the executable does
/// not need to embed a multi-megabyte CJK font file.
fn load_system_chinese_font() -> Option<(&'static str, Vec<u8>)> {
    #[cfg(windows)]
    {
        use std::path::PathBuf;

        let windir = std::env::var_os("WINDIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let fonts_dir = windir.join("Fonts");
        const CANDIDATES: [(&str, &str); 5] = [
            ("MicrosoftYaHei", "msyh.ttc"),
            ("MicrosoftYaHei", "msyh.ttf"),
            ("DengXian", "Deng.ttf"),
            ("SimHei", "simhei.ttf"),
            ("SimSun", "simsun.ttc"),
        ];
        for (name, file) in CANDIDATES {
            if let Ok(bytes) = std::fs::read(fonts_dir.join(file)) {
                return Some((name, bytes));
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn finds_default_windows_chinese_font() {
        let (name, bytes) = load_system_chinese_font().expect("Windows CJK font should exist");
        assert!(!name.is_empty());
        assert!(!bytes.is_empty());
    }
}

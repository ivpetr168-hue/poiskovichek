use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Fonts, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

use crate::settings::Theme;

/// Основные цвета интерфейса по умолчанию (тёмная тема, ТЗ раздел 4)
pub const BG_COLOR: egui::Color32 = egui::Color32::from_rgb(0x02, 0x05, 0x07);
pub const TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(0x22, 0xf2, 0xff);
pub const ACCENT_COLOR: egui::Color32 = egui::Color32::from_rgb(0xb6, 0xff, 0x3b);
pub const WARNING_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xb0, 0x00);
pub const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0x55, 0x55);

pub const CARD_BG: egui::Color32 = egui::Color32::from_rgb(0x06, 0x0d, 0x12);
pub const CARD_BORDER: egui::Color32 = egui::Color32::from_rgb(0x14, 0x93, 0xa1);
pub const SUBDUED_COLOR: egui::Color32 = egui::Color32::from_rgb(0x14, 0x93, 0xa1);

static CURRENT_THEME: AtomicU8 = AtomicU8::new(0);

fn theme_to_u8(theme: Theme) -> u8 {
    match theme {
        Theme::Dark => 0,
        Theme::Light => 1,
        Theme::Blue => 2,
        Theme::Brown => 3,
    }
}

fn u8_to_theme(val: u8) -> Theme {
    match val {
        1 => Theme::Light,
        2 => Theme::Blue,
        3 => Theme::Brown,
        _ => Theme::Dark,
    }
}

/// Возвращает текущую выбранную тему оформления
pub fn current_theme() -> Theme {
    u8_to_theme(CURRENT_THEME.load(Ordering::Relaxed))
}

/// Устанавливает активную тему оформления
pub fn set_theme(theme: Theme) {
    CURRENT_THEME.store(theme_to_u8(theme), Ordering::SeqCst);
}

/// Цветовая палитра темы оформления
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg: egui::Color32,
    pub text: egui::Color32,
    pub accent: egui::Color32,
    pub warning: egui::Color32,
    pub error: egui::Color32,
    pub card_bg: egui::Color32,
    pub card_border: egui::Color32,
    pub subdued: egui::Color32,
    pub title_bg: egui::Color32,
    pub extreme_bg: egui::Color32,
    pub selection_bg: egui::Color32,
    pub selection_text: egui::Color32,
    pub match_bg: egui::Color32,
    pub match_fg: egui::Color32,
    pub widget_inactive_bg: egui::Color32,
    pub widget_hovered_bg: egui::Color32,
    pub widget_active_bg: egui::Color32,
    pub widget_open_bg: egui::Color32,
    pub is_dark: bool,
}

impl Palette {
    pub fn for_theme(theme: Theme) -> Self {
        match theme {
            Theme::Dark => Self {
                bg: egui::Color32::from_rgb(0x02, 0x05, 0x07),
                text: egui::Color32::from_rgb(0x22, 0xf2, 0xff),
                accent: egui::Color32::from_rgb(0xb6, 0xff, 0x3b),
                warning: egui::Color32::from_rgb(0xff, 0xb0, 0x00),
                error: egui::Color32::from_rgb(0xff, 0x55, 0x55),
                card_bg: egui::Color32::from_rgb(0x06, 0x0d, 0x12),
                card_border: egui::Color32::from_rgb(0x14, 0x93, 0xa1),
                subdued: egui::Color32::from_rgb(0x14, 0x93, 0xa1),
                title_bg: egui::Color32::from_rgb(0x0a, 0x22, 0x2d),
                extreme_bg: egui::Color32::from_rgb(0x07, 0x10, 0x16),
                selection_bg: egui::Color32::from_rgb(0xb6, 0xff, 0x3b),
                selection_text: egui::Color32::from_rgb(0x02, 0x05, 0x07),
                match_bg: egui::Color32::from_rgb(0xb6, 0xff, 0x3b),
                match_fg: egui::Color32::from_rgb(0x02, 0x05, 0x07),
                widget_inactive_bg: egui::Color32::from_rgb(0x07, 0x16, 0x1e),
                widget_hovered_bg: egui::Color32::from_rgb(0x0b, 0x23, 0x30),
                widget_active_bg: egui::Color32::from_rgb(0x10, 0x32, 0x45),
                widget_open_bg: egui::Color32::from_rgb(0x0b, 0x23, 0x30),
                is_dark: true,
            },
            Theme::Light => Self {
                bg: egui::Color32::from_rgb(0xf0, 0xf2, 0xf5),
                text: egui::Color32::from_rgb(0x18, 0x22, 0x30),
                accent: egui::Color32::from_rgb(0x00, 0x4b, 0x9d),
                warning: egui::Color32::from_rgb(0x9c, 0x43, 0x07),
                error: egui::Color32::from_rgb(0xc5, 0x22, 0x1f),
                card_bg: egui::Color32::from_rgb(0xff, 0xff, 0xff),
                card_border: egui::Color32::from_rgb(0xc5, 0xd0, 0xdb),
                subdued: egui::Color32::from_rgb(0x50, 0x66, 0x80),
                title_bg: egui::Color32::from_rgb(0xda, 0xe2, 0xec),
                extreme_bg: egui::Color32::from_rgb(0xff, 0xff, 0xff),
                selection_bg: egui::Color32::from_rgb(0x00, 0x68, 0xaa),
                selection_text: egui::Color32::from_rgb(0xff, 0xff, 0xff),
                match_bg: egui::Color32::from_rgb(0xff, 0xe0, 0x66),
                match_fg: egui::Color32::from_rgb(0x18, 0x22, 0x30),
                widget_inactive_bg: egui::Color32::from_rgb(0xe2, 0xe8, 0xf0),
                widget_hovered_bg: egui::Color32::from_rgb(0xcc, 0xd9, 0xe8),
                widget_active_bg: egui::Color32::from_rgb(0xb8, 0xcb, 0xe0),
                widget_open_bg: egui::Color32::from_rgb(0xcc, 0xd9, 0xe8),
                is_dark: false,
            },
            Theme::Blue => Self {
                bg: egui::Color32::from_rgb(0x09, 0x11, 0x20),
                text: egui::Color32::from_rgb(0x90, 0xd4, 0xff),
                accent: egui::Color32::from_rgb(0x00, 0xf0, 0xc0),
                warning: egui::Color32::from_rgb(0xff, 0xb7, 0x1a),
                error: egui::Color32::from_rgb(0xff, 0x5c, 0x5c),
                card_bg: egui::Color32::from_rgb(0x0e, 0x1a, 0x30),
                card_border: egui::Color32::from_rgb(0x20, 0x55, 0x8f),
                subdued: egui::Color32::from_rgb(0x61, 0x9b, 0xd0),
                title_bg: egui::Color32::from_rgb(0x13, 0x24, 0x42),
                extreme_bg: egui::Color32::from_rgb(0x06, 0x0c, 0x17),
                selection_bg: egui::Color32::from_rgb(0x00, 0xf0, 0xc0),
                selection_text: egui::Color32::from_rgb(0x09, 0x11, 0x20),
                match_bg: egui::Color32::from_rgb(0x00, 0xf0, 0xc0),
                match_fg: egui::Color32::from_rgb(0x09, 0x11, 0x20),
                widget_inactive_bg: egui::Color32::from_rgb(0x12, 0x22, 0x3d),
                widget_hovered_bg: egui::Color32::from_rgb(0x19, 0x30, 0x56),
                widget_active_bg: egui::Color32::from_rgb(0x22, 0x40, 0x73),
                widget_open_bg: egui::Color32::from_rgb(0x19, 0x30, 0x56),
                is_dark: true,
            },
            Theme::Brown => Self {
                bg: egui::Color32::from_rgb(0x1a, 0x12, 0x0c),
                text: egui::Color32::from_rgb(0xfa, 0xde, 0xb8),
                accent: egui::Color32::from_rgb(0xff, 0xb0, 0x3b),
                warning: egui::Color32::from_rgb(0xff, 0xcc, 0x22),
                error: egui::Color32::from_rgb(0xff, 0x60, 0x4f),
                card_bg: egui::Color32::from_rgb(0x26, 0x1b, 0x14),
                card_border: egui::Color32::from_rgb(0x78, 0x4e, 0x35),
                subdued: egui::Color32::from_rgb(0xb6, 0x88, 0x6a),
                title_bg: egui::Color32::from_rgb(0x30, 0x20, 0x16),
                extreme_bg: egui::Color32::from_rgb(0x13, 0x0d, 0x09),
                selection_bg: egui::Color32::from_rgb(0xff, 0xb0, 0x3b),
                selection_text: egui::Color32::from_rgb(0x1a, 0x12, 0x0c),
                match_bg: egui::Color32::from_rgb(0xff, 0xb0, 0x3b),
                match_fg: egui::Color32::from_rgb(0x1a, 0x12, 0x0c),
                widget_inactive_bg: egui::Color32::from_rgb(0x2c, 0x1f, 0x17),
                widget_hovered_bg: egui::Color32::from_rgb(0x3d, 0x2a, 0x1f),
                widget_active_bg: egui::Color32::from_rgb(0x4e, 0x36, 0x28),
                widget_open_bg: egui::Color32::from_rgb(0x3d, 0x2a, 0x1f),
                is_dark: true,
            },
        }
    }
}

pub fn palette() -> Palette {
    Palette::for_theme(current_theme())
}

pub fn bg_color() -> egui::Color32 {
    palette().bg
}

pub fn text_color() -> egui::Color32 {
    palette().text
}

pub fn accent_color() -> egui::Color32 {
    palette().accent
}

pub fn warning_color() -> egui::Color32 {
    palette().warning
}

pub fn error_color() -> egui::Color32 {
    palette().error
}

pub fn subdued_color() -> egui::Color32 {
    palette().subdued
}

pub fn card_bg() -> egui::Color32 {
    palette().card_bg
}

pub fn card_border() -> egui::Color32 {
    palette().card_border
}

pub fn match_bg() -> egui::Color32 {
    palette().match_bg
}

pub fn match_fg() -> egui::Color32 {
    palette().match_fg
}

/// Определяет путь к системной папке шрифтов через Win32 `SHGetKnownFolderPath(FOLDERID_Fonts)`
pub fn get_system_fonts_dir() -> Option<PathBuf> {
    unsafe {
        let path_ptr = SHGetKnownFolderPath(&FOLDERID_Fonts, KF_FLAG_DEFAULT, None).ok()?;
        let path_str = path_ptr.to_string().ok()?;
        CoTaskMemFree(Some(path_ptr.as_ptr() as *const _));
        Some(PathBuf::from(path_str))
    }
}

/// Настраивает шрифты приложения: Consolas из системной папки, запасной — Segoe UI, иначе встроенный
pub fn configure_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    let fonts_dir = get_system_fonts_dir().unwrap_or_else(|| PathBuf::from(r"C:\Windows\Fonts"));

    let consolas = fonts_dir.join("consola.ttf");
    let segoe = fonts_dir.join("segoeui.ttf");

    if consolas.is_file() {
        if let Ok(bytes) = std::fs::read(&consolas) {
            fonts.font_data.insert(
                "SystemFont".to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "SystemFont".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .insert(0, "SystemFont".to_owned());
        }
    } else if segoe.is_file()
        && let Ok(bytes) = std::fs::read(&segoe)
    {
        fonts.font_data.insert(
            "SystemFont".to_owned(),
            Arc::new(egui::FontData::from_owned(bytes)),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "SystemFont".to_owned());
        fonts
            .families
            .entry(egui::FontFamily::Monospace)
            .or_default()
            .insert(0, "SystemFont".to_owned());
    }

    ctx.set_fonts(fonts);
}

/// Применяет выбранную тему оформления
pub fn apply_theme(ctx: &egui::Context, theme: Theme) {
    set_theme(theme);
    let p = Palette::for_theme(theme);

    let mut visuals = if p.is_dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };

    visuals.panel_fill = p.bg;
    visuals.window_fill = if p.is_dark { p.bg } else { egui::Color32::from_rgb(0xf8, 0xf9, 0xfa) };
    visuals.extreme_bg_color = p.extreme_bg;

    visuals.override_text_color = Some(p.text);
    visuals.selection.bg_fill = p.selection_bg;
    visuals.selection.stroke = egui::Stroke::new(1.0, if p.is_dark { p.bg } else { egui::Color32::WHITE });

    visuals.window_stroke = egui::Stroke::new(1.0, p.subdued);

    visuals.widgets.noninteractive.weak_bg_fill = p.card_bg;
    visuals.widgets.noninteractive.bg_fill = p.bg;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, p.subdued);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, p.text);

    visuals.widgets.inactive.weak_bg_fill = p.widget_inactive_bg;
    visuals.widgets.inactive.bg_fill = p.widget_inactive_bg;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, p.subdued);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, p.text);

    visuals.widgets.hovered.weak_bg_fill = p.widget_hovered_bg;
    visuals.widgets.hovered.bg_fill = p.widget_hovered_bg;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, p.accent);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, p.accent);

    visuals.widgets.active.weak_bg_fill = p.widget_active_bg;
    visuals.widgets.active.bg_fill = p.widget_active_bg;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, p.accent);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, p.accent);

    visuals.widgets.open.weak_bg_fill = p.widget_open_bg;
    visuals.widgets.open.bg_fill = p.widget_hovered_bg;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, p.accent);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, p.accent);

    let pref = if p.is_dark {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    };
    ctx.set_theme(pref);
    ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
    ctx.set_visuals_of(egui::Theme::Light, visuals.clone());
    ctx.set_visuals(visuals);
    ctx.request_repaint();
}

/// Рамка заголовка всплывающих окон в цветах темы
pub fn theme_title_frame() -> egui::Frame {
    let p = palette();
    egui::Frame::new()
        .inner_margin(egui::Margin::same(6))
        .fill(p.title_bg)
        .stroke(egui::Stroke::new(1.0, p.subdued))
}

/// Пункт выбора с читаемым текстом: у выбранного тёмные буквы на акцентном фоне
pub fn selectable_choice<V: PartialEq>(
    ui: &mut egui::Ui,
    current: &mut V,
    value: V,
    text: impl Into<String>,
) -> egui::Response {
    let selected = *current == value;
    let p = palette();
    let color = if selected { p.selection_text } else { p.text };
    let mut response = ui.selectable_label(selected, egui::RichText::new(text.into()).color(color));
    if response.clicked() && !selected {
        *current = value;
        response.mark_changed();
    }
    response
}

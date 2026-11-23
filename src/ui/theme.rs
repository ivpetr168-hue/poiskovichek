use std::path::PathBuf;
use std::sync::Arc;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{FOLDERID_Fonts, KF_FLAG_DEFAULT, SHGetKnownFolderPath};

/// Основные цвета интерфейса (ТЗ раздел 4)
pub const BG_COLOR: egui::Color32 = egui::Color32::from_rgb(0x02, 0x05, 0x07);
pub const TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(0x22, 0xf2, 0xff);
pub const ACCENT_COLOR: egui::Color32 = egui::Color32::from_rgb(0xb6, 0xff, 0x3b);
pub const WARNING_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0xb0, 0x00);
pub const ERROR_COLOR: egui::Color32 = egui::Color32::from_rgb(0xff, 0x55, 0x55);

pub const CARD_BG: egui::Color32 = egui::Color32::from_rgb(0x06, 0x0d, 0x12);
pub const CARD_BORDER: egui::Color32 = egui::Color32::from_rgb(0x14, 0x93, 0xa1);
pub const SUBDUED_COLOR: egui::Color32 = egui::Color32::from_rgb(0x14, 0x93, 0xa1);

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

/// Применяет тёмную терминальную тему оформления
pub fn apply_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = BG_COLOR;
    visuals.window_fill = BG_COLOR;
    visuals.extreme_bg_color = egui::Color32::from_rgb(0x07, 0x10, 0x16);

    visuals.override_text_color = Some(TEXT_COLOR);
    visuals.selection.bg_fill = ACCENT_COLOR;
    visuals.selection.stroke = egui::Stroke::new(1.0, BG_COLOR);

    visuals.window_stroke = egui::Stroke::new(1.0, SUBDUED_COLOR);

    visuals.widgets.noninteractive.weak_bg_fill = CARD_BG;
    visuals.widgets.noninteractive.bg_fill = BG_COLOR;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, SUBDUED_COLOR);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_COLOR);

    visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(0x07, 0x16, 0x1e);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(0x07, 0x16, 0x1e);
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, SUBDUED_COLOR);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0, TEXT_COLOR);

    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(0x0b, 0x23, 0x30);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(0x0b, 0x23, 0x30);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, ACCENT_COLOR);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.0, ACCENT_COLOR);

    visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(0x10, 0x32, 0x45);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(0x10, 0x32, 0x45);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0, ACCENT_COLOR);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(1.0, ACCENT_COLOR);

    visuals.widgets.open.weak_bg_fill = egui::Color32::from_rgb(0x0a, 0x22, 0x2d);
    visuals.widgets.open.bg_fill = egui::Color32::from_rgb(0x0b, 0x23, 0x30);
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0, ACCENT_COLOR);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0, ACCENT_COLOR);

    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.set_visuals_of(egui::Theme::Dark, visuals.clone());
    ctx.set_visuals_of(egui::Theme::Light, visuals.clone());
    ctx.set_visuals(visuals);
}

/// Рамка заголовка всплывающих окон в цветах темы
pub fn theme_title_frame() -> egui::Frame {
    egui::Frame::new()
        .inner_margin(egui::Margin::same(6))
        .fill(egui::Color32::from_rgb(0x0a, 0x22, 0x2d))
        .stroke(egui::Stroke::new(1.0, SUBDUED_COLOR))
}

/// Пункт выбора с читаемым текстом: у выбранного тёмные буквы на акцентном фоне
pub fn selectable_choice<V: PartialEq>(
    ui: &mut egui::Ui,
    current: &mut V,
    value: V,
    text: impl Into<String>,
) -> egui::Response {
    let selected = *current == value;
    let color = if selected { BG_COLOR } else { TEXT_COLOR };
    let mut response = ui.selectable_label(selected, egui::RichText::new(text.into()).color(color));
    if response.clicked() && !selected {
        *current = value;
        response.mark_changed();
    }
    response
}

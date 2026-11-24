use std::path::PathBuf;

use crate::drives::{DriveInfo, default_selection, describe, list_drives};
use crate::settings::{Source, SourceMode};
use crate::ui::dialogs::pick_folder;
use crate::ui::source_mode_title;
use crate::ui::theme::{
    accent_color, selectable_choice, subdued_color, text_color, theme_title_frame, warning_color,
};

/// Элемент диска в мастере первого запуска
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WizardDriveItem {
    pub info: DriveInfo,
    pub selected: bool,
    pub mode: SourceMode,
}

/// Дополнительно выбранная пользователем папка
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WizardFolderItem {
    pub path: PathBuf,
    pub mode: SourceMode,
}

/// Действие, возвращаемое мастером после обработки кадра
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WizardAction {
    None,
    /// Пользователь нажал «Пропустить» или закрыл мастер
    Skip,
    /// Пользователь нажал «Начать индексирование»
    Start(Vec<Source>),
}

/// Состояние мастера первого запуска (два шага)
pub struct WizardState {
    pub step: usize, // 0: «Что искать», 1: «Сводка»
    pub drives: Vec<WizardDriveItem>,
    pub custom_folders: Vec<WizardFolderItem>,
}

impl Default for WizardState {
    fn default() -> Self {
        Self::new()
    }
}

impl WizardState {
    pub fn new() -> Self {
        let drives = list_drives();
        let defaults = default_selection(&drives);

        let drive_items = drives
            .into_iter()
            .map(|d| {
                let selected = defaults.contains(&d.letter);
                let mode = if d.kind == crate::drives::DriveKind::Cloud {
                    SourceMode::NamesOnly
                } else {
                    SourceMode::Full
                };
                WizardDriveItem {
                    info: d,
                    selected,
                    mode,
                }
            })
            .collect();

        Self {
            step: 0,
            drives: drive_items,
            custom_folders: Vec::new(),
        }
    }

    /// Отрисовывает окно мастера первого запуска
    pub fn show(&mut self, ctx: &egui::Context, open: &mut bool) -> WizardAction {
        let mut is_open = *open;
        let mut action = WizardAction::None;

        egui::Window::new("Мастер первого запуска")
            .open(&mut is_open)
            .collapsible(false)
            .resizable(true)
            .default_width(640.0)
            .default_height(460.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .title_frame(theme_title_frame())
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;

                if self.step == 0 {
                    action = self.render_step1(ui);
                } else {
                    action = self.render_step2(ui);
                }
            });

        if !is_open {
            *open = false;
            return WizardAction::Skip;
        }

        action
    }

    fn render_step1(&mut self, ui: &mut egui::Ui) -> WizardAction {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Шаг 1 из 2: Что искать")
                .size(16.0)
                .strong(),
        );
        ui.label("Выберите диски и папки компьютера для включения в поиск документов:");
        ui.separator();

        egui::ScrollArea::vertical()
            .max_height(260.0)
            .show(ui, |ui| {
                ui.colored_label(text_color(), "Диски компьютера:");
                for drive_item in &mut self.drives {
                    ui.horizontal(|ui| {
                        let desc = describe(&drive_item.info);
                        ui.checkbox(&mut drive_item.selected, desc);

                        if drive_item.selected {
                            egui::ComboBox::from_id_salt(format!(
                                "drive_mode_{}",
                                drive_item.info.letter
                            ))
                            .selected_text(source_mode_title(drive_item.mode))
                            .show_ui(ui, |ui| {
                                selectable_choice(
                                    ui,
                                    &mut drive_item.mode,
                                    SourceMode::Full,
                                    "полностью",
                                );
                                selectable_choice(
                                    ui,
                                    &mut drive_item.mode,
                                    SourceMode::DocumentsOnly,
                                    "только документы",
                                );
                                selectable_choice(
                                    ui,
                                    &mut drive_item.mode,
                                    SourceMode::NamesOnly,
                                    "только имена",
                                );
                            });
                        }
                    });
                }

                if !self.custom_folders.is_empty() {
                    ui.add_space(8.0);
                    ui.colored_label(text_color(), "Дополнительные папки:");
                    let mut remove_idx = None;

                    for (idx, folder) in self.custom_folders.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(folder.path.to_string_lossy().to_string());
                            egui::ComboBox::from_id_salt(format!("folder_mode_{idx}"))
                                .selected_text(source_mode_title(folder.mode))
                                .show_ui(ui, |ui| {
                                    selectable_choice(
                                        ui,
                                        &mut folder.mode,
                                        SourceMode::Full,
                                        "полностью",
                                    );
                                    selectable_choice(
                                        ui,
                                        &mut folder.mode,
                                        SourceMode::DocumentsOnly,
                                        "только документы",
                                    );
                                    selectable_choice(
                                        ui,
                                        &mut folder.mode,
                                        SourceMode::NamesOnly,
                                        "только имена",
                                    );
                                });

                            if ui.button("Удалить").clicked() {
                                remove_idx = Some(idx);
                            }
                        });
                    }

                    if let Some(i) = remove_idx {
                        self.custom_folders.remove(i);
                    }
                }
            });

        ui.add_space(4.0);
        if ui.button("Добавить папку…").clicked()
            && let Some(folder) = pick_folder()
            && !self.custom_folders.iter().any(|f| f.path == folder)
        {
            let mode = if crate::drives::is_cloud_path(&folder) {
                SourceMode::NamesOnly
            } else {
                SourceMode::Full
            };
            self.custom_folders
                .push(WizardFolderItem { path: folder, mode });
        }

        ui.separator();

        let mut action = WizardAction::None;
        ui.horizontal(|ui| {
            if ui.button("Пропустить").clicked() {
                action = WizardAction::Skip;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let has_selection =
                    self.drives.iter().any(|d| d.selected) || !self.custom_folders.is_empty();
                if ui
                    .add_enabled(has_selection, egui::Button::new("Далее →"))
                    .clicked()
                {
                    self.step = 1;
                }
            });
        });

        action
    }

    fn render_step2(&mut self, ui: &mut egui::Ui) -> WizardAction {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Шаг 2 из 2: Сводка")
                .size(16.0)
                .strong(),
        );
        ui.label("Проверьте выбранные источники перед запуском первичного индексирования:");
        ui.separator();

        egui::ScrollArea::vertical()
            .max_height(240.0)
            .show(ui, |ui| {
                ui.colored_label(text_color(), "Будет проиндексировано:");
                for drive in self.drives.iter().filter(|d| d.selected) {
                    ui.label(format!(
                        "• Диск {}: ({}) — режим «{}»",
                        drive.info.letter,
                        drive.info.label,
                        source_mode_title(drive.mode)
                    ));
                }

                for folder in &self.custom_folders {
                    ui.label(format!(
                        "• Папка «{}» — режим «{}»",
                        folder.path.display(),
                        source_mode_title(folder.mode)
                    ));
                }

                ui.add_space(8.0);
                ui.colored_label(
                    subdued_color(),
                    "Исключения по умолчанию: системные папки Windows, Program Files, AppData, корзина; служебные папки .git, node_modules, target, venv; системные файлы pagefile.sys, hiberfil.sys.",
                );
                ui.add_space(4.0);
                ui.colored_label(
                    warning_color(),
                    "Примечание: время обхода зависит от объёма и типа накопителей; точное время заранее не прогнозируется.",
                );
            });

        ui.separator();

        let mut action = WizardAction::None;
        ui.horizontal(|ui| {
            if ui.button("← Назад").clicked() {
                self.step = 0;
            }
            if ui.button("Пропустить").clicked() {
                action = WizardAction::Skip;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(egui::RichText::new("Начать индексирование").strong())
                    .clicked()
                {
                    let mut sources = Vec::new();
                    for d in self.drives.iter().filter(|d| d.selected) {
                        sources.push(Source {
                            path: d.info.root.clone(),
                            mode: d.mode,
                            max_depth: 0,
                            enabled: true,
                            volume_serial: d.info.serial,
                            extra_excludes: Vec::new(),
                        });
                    }
                    for f in &self.custom_folders {
                        sources.push(Source {
                            path: f.path.clone(),
                            mode: f.mode,
                            max_depth: 0,
                            enabled: true,
                            volume_serial: None,
                            extra_excludes: Vec::new(),
                        });
                    }
                    action = WizardAction::Start(sources);
                }
            });
        });

        action
    }
}

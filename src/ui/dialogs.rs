use std::path::PathBuf;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog,
    IFileOpenDialog, ILCreateFromPathW, ILFree, IShellItem, SHOpenFolderAndSelectItems,
    SIGDN_FILESYSPATH, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

use crate::ui::theme::{ACCENT_COLOR, SUBDUED_COLOR, TEXT_COLOR, WARNING_COLOR};

/// Открывает файл во внешней программе по умолчанию через Win32 `ShellExecuteW`.
/// Если путь указывает на элемент внутри ZIP-архива (`...zip → member`),
/// открывается сам архив.
pub fn open_file_externally(path_str: &str) -> Result<(), String> {
    let target = if let Some((zip_path, _)) = path_str.split_once(" → ") {
        zip_path.trim()
    } else {
        path_str.trim()
    };

    let wide_file: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    let wide_op: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let res: HINSTANCE = ShellExecuteW(
            None,
            PCWSTR(wide_op.as_ptr()),
            PCWSTR(wide_file.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
        if (res.0 as usize) <= 32 {
            return Err(format!("ShellExecuteW failed (код {})", res.0 as usize));
        }
    }
    Ok(())
}

/// Открывает проводник Windows с выделением файла через Win32 `SHOpenFolderAndSelectItems`.
/// Если путь указывает на элемент внутри архива, выделяется файл архива.
pub fn show_in_folder(path_str: &str) -> Result<(), String> {
    let target = if let Some((zip_path, _)) = path_str.split_once(" → ") {
        zip_path.trim()
    } else {
        path_str.trim()
    };

    let wide: Vec<u16> = target.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let pidl = ILCreateFromPathW(PCWSTR(wide.as_ptr()));
        if pidl.is_null() {
            return Err("Не удалось получить PIDL для пути".to_string());
        }
        let res = SHOpenFolderAndSelectItems(pidl, None, 0);
        ILFree(Some(pidl));
        res.map_err(|e| format!("SHOpenFolderAndSelectItems failed: {e}"))
    }
}

/// Открывает штатный диалог Windows `IFileOpenDialog` для выбора одной папки.
pub fn pick_folder() -> Option<PathBuf> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).ok()?;

        let mut opts = dialog.GetOptions().ok()?;
        opts |= FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST;
        dialog.SetOptions(opts).ok()?;

        if dialog.Show(None).is_err() {
            return None;
        }

        let item: IShellItem = dialog.GetResult().ok()?;
        let display_name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path_str = display_name.to_string().ok()?;
        CoTaskMemFree(Some(display_name.as_ptr() as *const _));
        Some(PathBuf::from(path_str))
    }
}

/// Открывает штатный диалог Windows `IFileOpenDialog` для множественного выбора файлов.
pub fn pick_files() -> Option<Vec<PathBuf>> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL).ok()?;

        let mut opts = dialog.GetOptions().ok()?;
        opts |= FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM | FOS_FILEMUSTEXIST;
        dialog.SetOptions(opts).ok()?;

        if dialog.Show(None).is_err() {
            return None;
        }

        let items = dialog.GetResults().ok()?;
        let count = items.GetCount().ok()?;
        let mut result = Vec::new();

        for i in 0..count {
            if let Ok(item) = items.GetItemAt(i)
                && let Ok(display_name) = item.GetDisplayName(SIGDN_FILESYSPATH)
            {
                if let Ok(path_str) = display_name.to_string() {
                    result.push(PathBuf::from(path_str));
                }
                CoTaskMemFree(Some(display_name.as_ptr() as *const _));
            }
        }

        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }
}

/// Окно «О программе»
pub fn show_about_dialog(ctx: &egui::Context, open: &mut bool) {
    let mut is_open = *open;
    egui::Window::new("О программе")
        .open(&mut is_open)
        .collapsible(false)
        .resizable(false)
        .default_width(480.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .title_frame(crate::ui::theme::theme_title_frame())
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;

            ui.colored_label(
                ACCENT_COLOR,
                format!("ПоисКовичеК {}", env!("CARGO_PKG_VERSION")),
            );
            ui.label("Быстрый локальный поиск по именам, путям и содержимому документов.");
            ui.separator();

            ui.colored_label(TEXT_COLOR, "Лицензия: MIT");
            ui.colored_label(
                SUBDUED_COLOR,
                "Морфологический словарь: OpenCorpora / pymorphy3 (лицензия MIT)",
            );

            ui.separator();
            ui.colored_label(TEXT_COLOR, "Используемые библиотеки:");
            ui.label("• tantivy (Apache-2.0) — полнотекстовый индекс");
            ui.label("• rusqlite (MIT) — база метаданных SQLite");
            ui.label("• eframe / egui (MIT / Apache-2.0) — графический интерфейс");
            ui.label("• windows (MIT / Apache-2.0) — интеграция с Win32 API");
            ui.label("• pdf-extract / lopdf (MIT) — извлечение текста PDF");
            ui.label("• zstd (BSD 3-Clause) — сжатие текстовых блоков");
            ui.label("• zip (MIT) — чтение архивов и офисных документов");
            ui.label("• fst (MIT / Unlicense) — словарь словоформ");
            ui.label("• roxmltree (MIT / Apache-2.0) — разбор XML/FB2/DOCX");
            ui.label("• rayon (MIT / Apache-2.0) — параллельные вычисления");

            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Закрыть").clicked() {
                    *open = false;
                }
            });
        });

    if !is_open {
        *open = false;
    }
}

/// Окно подтверждения перестройки индекса из сохраненных текстов
pub fn show_confirm_reindex_dialog(
    ctx: &egui::Context,
    open: &mut bool,
    on_confirm: impl FnOnce(),
) {
    let mut is_open = *open;
    egui::Window::new("Подтверждение")
        .open(&mut is_open)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .title_frame(crate::ui::theme::theme_title_frame())
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 12.0;
            ui.colored_label(
                WARNING_COLOR,
                "Перестроить поиск из сохранённых текстов? Исходные файлы читаться не будут.",
            );
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Перестроить").clicked() {
                    on_confirm();
                    *open = false;
                }
                if ui.button("Отмена").clicked() {
                    *open = false;
                }
            });
        });

    if !is_open {
        *open = false;
    }
}

/// Окно подтверждения удаления записи из базы поиска
pub fn show_confirm_remove_record_dialog(
    ctx: &egui::Context,
    path_to_remove: &mut Option<String>,
    on_confirm: impl FnOnce(String),
) {
    let Some(ref path) = path_to_remove.clone() else {
        return;
    };

    let mut is_open = true;
    egui::Window::new("Удаление записи")
        .open(&mut is_open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .title_frame(crate::ui::theme::theme_title_frame())
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 10.0;
            ui.colored_label(
                WARNING_COLOR,
                "Убрать эту запись из поиска? Файл на диске останется.",
            );
            ui.colored_label(SUBDUED_COLOR, path);
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Убрать из поиска").clicked() {
                    on_confirm(path.clone());
                    *path_to_remove = None;
                }
                if ui.button("Отмена").clicked() {
                    *path_to_remove = None;
                }
            });
        });

    if !is_open {
        *path_to_remove = None;
    }
}

/// Диалог выбора источников для повторной индексации
pub fn show_reindex_sources_dialog(
    ctx: &egui::Context,
    open: &mut bool,
    sources: &[PathBuf],
    selected: &mut Vec<PathBuf>,
    on_confirm: impl FnOnce(Vec<PathBuf>),
) {
    let mut is_open = *open;
    egui::Window::new("Проиндексировать выбранные диски заново")
        .open(&mut is_open)
        .collapsible(false)
        .resizable(true)
        .default_width(500.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .title_frame(crate::ui::theme::theme_title_frame())
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label("Выберите источники для повторного индексирования:");

            egui::ScrollArea::vertical()
                .max_height(200.0)
                .show(ui, |ui| {
                    for src in sources {
                        let mut is_checked = selected.contains(src);
                        let label = src.to_string_lossy().to_string();
                        if ui.checkbox(&mut is_checked, label).changed() {
                            if is_checked {
                                if !selected.contains(src) {
                                    selected.push(src.clone());
                                }
                            } else {
                                selected.retain(|s| s != src);
                            }
                        }
                    }
                });

            ui.separator();
            ui.horizontal(|ui| {
                let can_run = !selected.is_empty();
                if ui
                    .add_enabled(can_run, egui::Button::new("Начать"))
                    .clicked()
                {
                    on_confirm(selected.clone());
                    *open = false;
                }
                if ui.button("Отмена").clicked() {
                    *open = false;
                }
            });
        });

    if !is_open {
        *open = false;
    }
}

/// Диалог выбора источника для удаления из поиска
pub fn show_remove_source_dialog(
    ctx: &egui::Context,
    open: &mut bool,
    sources: &[PathBuf],
    on_confirm: impl FnOnce(PathBuf),
) {
    let mut is_open = *open;
    let mut chosen_to_delete: Option<PathBuf> = None;

    egui::Window::new("Удалить диск или папку из поиска (файлы на диске останутся)")
        .open(&mut is_open)
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .title_frame(crate::ui::theme::theme_title_frame())
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.colored_label(
                WARNING_COLOR,
                "Файлы на диске останутся. Из базы поиска будут удалены все записи выбранного источника.",
            );
            ui.separator();

            if sources.is_empty() {
                ui.label("В поиске нет зарегистрированных источников.");
            } else {
                egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                    for src in sources {
                        ui.horizontal(|ui| {
                            ui.label(src.to_string_lossy().to_string());
                            if ui.button("Удалить").clicked() {
                                chosen_to_delete = Some(src.clone());
                            }
                        });
                    }
                });
            }

            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Закрыть").clicked() {
                    *open = false;
                }
            });
        });

    if let Some(src) = chosen_to_delete {
        on_confirm(src);
        *open = false;
    }

    if !is_open {
        *open = false;
    }
}

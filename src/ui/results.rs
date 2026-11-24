use crate::search::SearchItem;
use crate::snippet::Snippet;
use crate::ui::dialogs::{open_file_externally, show_in_folder};
use crate::ui::theme::{
    accent_color, card_bg, card_border, match_bg, match_fg, subdued_color, text_color,
};

/// Форматирует размер файла в человекочитаемом виде (МБ, КБ, Б)
pub fn format_size(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} МБ", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1024 {
        format!("{:.1} КБ", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} Б")
    }
}

/// Форматирует число расположений
pub fn format_locations(count: usize) -> String {
    if count == 1 {
        "1 расположение".to_string()
    } else {
        format!("{count} расположений")
    }
}

/// Состояние сниппета для карточки
#[derive(Clone, Debug)]
pub enum SnippetState {
    Pending,
    Ready(Snippet),
}

/// Отрисовывает виртуальный список карточек найденных файлов
pub fn render_results_list(
    ui: &mut egui::Ui,
    items: &[SearchItem],
    generation: u64,
    snippet_cache: &std::collections::HashMap<(u64, String), SnippetState>,
    request_snippet: &mut impl FnMut(String),
    confirm_remove_path: &mut Option<String>,
) {
    if items.is_empty() {
        return;
    }

    let approx_row_height = 130.0;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, approx_row_height, items.len(), |ui, row_range| {
            for idx in row_range {
                let item = &items[idx];
                let item_key = &item.item_key;

                // Запрос на расчет фрагмента, если еще не в кэше
                let cache_key = (generation, item_key.clone());
                if !snippet_cache.contains_key(&cache_key) {
                    request_snippet(item_key.clone());
                }

                render_card(ui, item, snippet_cache.get(&cache_key), confirm_remove_path);
                ui.add_space(8.0);
            }
        });
}

fn render_card(
    ui: &mut egui::Ui,
    item: &SearchItem,
    snippet_state: Option<&SnippetState>,
    confirm_remove_path: &mut Option<String>,
) {
    egui::Frame::canvas(ui.style())
        .fill(card_bg())
        .stroke(egui::Stroke::new(1.0, card_border()))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());

            // 1. Имя файла крупно и акцентом
            ui.horizontal(|ui| {
                ui.colored_label(
                    accent_color(),
                    egui::RichText::new(&item.name).size(16.0).strong(),
                );
                if item.is_duplicate {
                    ui.colored_label(subdued_color(), egui::RichText::new("[дубликат]").size(12.0));
                }
            });

            // 2. Детали: формат · категория · размер · N расположений [· текст не извлечён: причина]
            let mut details = format!(
                "{} · {} · {} · {}",
                item.extension,
                item.category,
                format_size(item.size),
                format_locations(item.path_count)
            );

            let display_status = item.status.clone().or_else(|| {
                crate::search::format_extraction_status(item.content_status.as_deref(), None)
            });
            if let Some(ref st) = display_status
                && !st.is_empty()
                && st != "ok"
            {
                details.push_str(" · ");
                details.push_str(st);
            }

            ui.colored_label(subdued_color(), egui::RichText::new(details).size(12.0));
            ui.add_space(4.0);

            // 3. Фрагмент с подсветкой
            match snippet_state {
                Some(SnippetState::Ready(snip)) if !snip.is_empty() => {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (seg, is_match) in &snip.segments {
                            if *is_match {
                                ui.colored_label(
                                    match_fg(),
                                    egui::RichText::new(seg)
                                        .background_color(match_bg())
                                        .size(13.0),
                                );
                            } else {
                                ui.colored_label(text_color(), egui::RichText::new(seg).size(13.0));
                            }
                        }
                    });
                }
                Some(SnippetState::Pending) => {
                    ui.colored_label(
                        subdued_color(),
                        egui::RichText::new("Фрагмент загружается…")
                            .size(12.0)
                            .italics(),
                    );
                }
                _ => {
                    // Текст отсутствует или сниппет пуст
                }
            }

            ui.add_space(4.0);

            // 4. Пути карточки
            for path in &item.paths {
                let response = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(path)
                                .color(text_color())
                                .size(12.0)
                                .underline(),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_cursor(egui::CursorIcon::PointingHand);

                // Двойной щелчок открывает файл
                if response.double_clicked() {
                    let _ = open_file_externally(path);
                }

                // Контекстное меню по правому клику
                response.context_menu(|ui| {
                    if ui.button("Открыть файл").clicked() {
                        let _ = open_file_externally(path);
                        ui.close();
                    }
                    if ui.button("Показать в папке").clicked() {
                        let _ = show_in_folder(path);
                        ui.close();
                    }
                    if ui.button("Копировать путь").clicked() {
                        ui.ctx().copy_text(path.clone());
                        ui.close();
                    }
                    ui.separator();
                    if ui
                        .button("Удалить запись из поиска (файл на диске останется)")
                        .clicked()
                    {
                        *confirm_remove_path = Some(path.clone());
                        ui.close();
                    }
                });
            }
        });
}

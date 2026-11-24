use std::path::PathBuf;

use crate::autostart;
use crate::relocate::RelocateMode;
use crate::settings::{
    MultiWordMode, PdfExtractorMode, Settings, Source, SourceMode, Theme, WordFormsMode,
};
use crate::ui::dialogs::pick_folder;
use crate::ui::theme::{
    accent_color, error_color, palette, selectable_choice, subdued_color, text_color,
    theme_title_frame, warning_color,
};
use crate::ui::{SORT_MODES, sort_mode_title, source_mode_title};

/// Вкладки окна настроек
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsTab {
    Sources,
    Exceptions,
    FormatsAndLimits,
    SensitiveData,
    Search,
    Update,
    DataAndPerformance,
    Interface,
}

impl SettingsTab {
    pub fn title(self) -> &'static str {
        match self {
            Self::Sources => "Источники",
            Self::Exceptions => "Исключения",
            Self::FormatsAndLimits => "Форматы и пределы",
            Self::SensitiveData => "Закрытые данные",
            Self::Search => "Поиск",
            Self::Update => "Обновление",
            Self::DataAndPerformance => "Данные и производительность",
            Self::Interface => "Интерфейс",
        }
    }
}

/// Результат сохранения настроек
#[derive(Clone, Debug)]
pub struct SettingsSaveResult {
    pub new_settings: Settings,
    pub old_settings: Settings,
    pub relocate: Option<(PathBuf, RelocateMode)>,
}

/// Действие, возвращаемое диалогом настроек
pub enum SettingsDialogAction {
    None,
    Cancel,
    Save(Box<SettingsSaveResult>),
}

/// Состояние диалога настроек
pub struct SettingsDialogState {
    pub draft: Settings,
    pub original: Settings,
    pub active_tab: SettingsTab,
    pub validation_errors: Vec<String>,

    // Вспомогательные строковые буферы для добавления элементов списков
    pub new_exclude_folder: String,
    pub new_docs_only_folder: String,
    pub new_exclude_path: String,
    pub new_exclude_file: String,
    pub new_secret_pattern: String,
    pub new_book_folder_word: String,

    // Состояние подтверждения удаления источника
    pub confirm_remove_source_idx: Option<usize>,

    // Состояние подтверждения сброса настроек
    pub confirm_reset_defaults: bool,

    // Состояние изменения папки данных
    pub selected_new_data_dir: Option<PathBuf>,
    pub relocate_mode: RelocateMode,
}

impl SettingsDialogState {
    pub fn new(current_settings: &Settings) -> Self {
        let mut draft = current_settings.clone();

        // Читаем реальное состояние автозапуска из реестра Windows
        if let Ok(reg_enabled) = autostart::is_autostart_enabled() {
            draft.autostart = reg_enabled;
        }

        let errors = draft.validate();

        Self {
            draft,
            original: current_settings.clone(),
            active_tab: SettingsTab::Sources,
            validation_errors: errors,
            new_exclude_folder: String::new(),
            new_docs_only_folder: String::new(),
            new_exclude_path: String::new(),
            new_exclude_file: String::new(),
            new_secret_pattern: String::new(),
            new_book_folder_word: String::new(),
            confirm_remove_source_idx: None,
            confirm_reset_defaults: false,
            selected_new_data_dir: None,
            relocate_mode: RelocateMode::Move,
        }
    }

    /// Отрисовывает окно диалога настроек
    pub fn show(&mut self, ctx: &egui::Context, open: &mut bool) -> SettingsDialogAction {
        let mut is_open = *open;
        let mut action = SettingsDialogAction::None;

        egui::Window::new("Настройки")
            .open(&mut is_open)
            .collapsible(false)
            .resizable(true)
            .default_width(780.0)
            .default_height(580.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .title_frame(theme_title_frame())
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = 8.0;

                // Переключатель вкладок
                ui.horizontal_wrapped(|ui| {
                    let tabs = [
                        SettingsTab::Sources,
                        SettingsTab::Exceptions,
                        SettingsTab::FormatsAndLimits,
                        SettingsTab::SensitiveData,
                        SettingsTab::Search,
                        SettingsTab::Update,
                        SettingsTab::DataAndPerformance,
                        SettingsTab::Interface,
                    ];
                    for tab in tabs {
                        let selected = self.active_tab == tab;
                        if ui
                            .selectable_label(
                                selected,
                                egui::RichText::new(tab.title()).color(if selected {
                                    palette().selection_text
                                } else {
                                    text_color()
                                }),
                            )
                            .clicked()
                        {
                            self.active_tab = tab;
                        }
                    }
                });

                ui.separator();

                // Содержимое активной вкладки
                egui::ScrollArea::vertical()
                    .max_height(420.0)
                    .show(ui, |ui| match self.active_tab {
                        SettingsTab::Sources => self.render_sources_tab(ui),
                        SettingsTab::Exceptions => self.render_exceptions_tab(ui),
                        SettingsTab::FormatsAndLimits => self.render_formats_tab(ui),
                        SettingsTab::SensitiveData => self.render_sensitive_data_tab(ui),
                        SettingsTab::Search => self.render_search_tab(ui),
                        SettingsTab::Update => self.render_update_tab(ui),
                        SettingsTab::DataAndPerformance => self.render_data_tab(ui),
                        SettingsTab::Interface => self.render_interface_tab(ui),
                    });

                ui.separator();

                // Нижняя панель с ошибками валидации и кнопками
                action = self.render_footer(ui, open);
            });

        if !is_open {
            *open = false;
            return SettingsDialogAction::Cancel;
        }

        action
    }

    // ========================================================================
    // 1. Вкладка «Источники»
    // ========================================================================
    fn render_sources_tab(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Источники поиска").size(15.0).strong(),
        );
        ui.colored_label(
            subdued_color(),
            "Диски и папки, по которым производится поиск, с индивидуальными режимами обхода.",
        );
        ui.add_space(4.0);

        if self.draft.sources.is_empty() {
            ui.label("Источники не добавлены. Добавьте диск или папку для индексирования.");
        } else {
            let mut remove_idx = None;

            egui::Grid::new("sources_grid")
                .striped(true)
                .spacing([12.0, 6.0])
                .show(ui, |ui| {
                    ui.colored_label(text_color(), "Вкл");
                    ui.colored_label(text_color(), "Путь");
                    ui.colored_label(text_color(), "Режим обхода");
                    ui.colored_label(text_color(), "Глубина (0 — вся)");
                    ui.colored_label(text_color(), "Действие");
                    ui.end_row();

                    for (idx, source) in self.draft.sources.iter_mut().enumerate() {
                        ui.checkbox(&mut source.enabled, "");
                        ui.label(source.path.to_string_lossy().to_string());

                        egui::ComboBox::from_id_salt(format!("src_mode_{idx}"))
                            .selected_text(source_mode_title(source.mode))
                            .show_ui(ui, |ui| {
                                selectable_choice(
                                    ui,
                                    &mut source.mode,
                                    SourceMode::Full,
                                    "полностью",
                                );
                                selectable_choice(
                                    ui,
                                    &mut source.mode,
                                    SourceMode::DocumentsOnly,
                                    "только документы",
                                );
                                selectable_choice(
                                    ui,
                                    &mut source.mode,
                                    SourceMode::NamesOnly,
                                    "только имена",
                                );
                            });

                        ui.add(egui::DragValue::new(&mut source.max_depth).range(0..=100));

                        if ui.button("Удалить").clicked() {
                            remove_idx = Some(idx);
                        }
                        ui.end_row();
                    }
                });

            if let Some(idx) = remove_idx {
                self.confirm_remove_source_idx = Some(idx);
            }
        }

        ui.add_space(8.0);
        if ui.button("Добавить папку…").clicked()
            && let Some(folder) = pick_folder()
            && !self.draft.sources.iter().any(|s| s.path == folder)
        {
            let mode = if crate::drives::is_cloud_path(&folder) {
                SourceMode::NamesOnly
            } else {
                SourceMode::Full
            };
            self.draft.sources.push(Source {
                path: folder,
                mode,
                max_depth: 0,
                enabled: true,
                volume_serial: None,
                extra_excludes: Vec::new(),
            });
        }

        // Модальное подтверждение удаления источника
        if let Some(idx) = self.confirm_remove_source_idx
            && let Some(src) = self.draft.sources.get(idx)
        {
            let src_path = src.path.display().to_string();
            let mut confirm_open = true;
            egui::Window::new("Подтверждение удаления")
                    .open(&mut confirm_open)
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .title_frame(theme_title_frame())
                    .show(ui.ctx(), |ui| {
                        ui.colored_label(
                            warning_color(),
                            "Записи удалятся из поиска, файлы на диске останутся. Удалить этот источник?",
                        );
                        ui.colored_label(subdued_color(), src_path);
                        ui.separator();
                        ui.horizontal(|ui| {
                            if ui.button("Удалить источник").clicked() {
                                self.draft.sources.remove(idx);
                                self.confirm_remove_source_idx = None;
                            }
                            if ui.button("Отмена").clicked() {
                                self.confirm_remove_source_idx = None;
                            }
                        });
                    });
            if !confirm_open {
                self.confirm_remove_source_idx = None;
            }
        }
    }

    // ========================================================================
    // 2. Вкладка «Исключения»
    // ========================================================================
    fn render_exceptions_tab(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Настройка исключений")
                .size(15.0)
                .strong(),
        );
        ui.colored_label(
            subdued_color(),
            "Исключения позволяют ускорить индексацию и избежать засорения поиска служебными файлами.",
        );
        ui.add_space(6.0);

        // Список 1: Имена папок
        ui.colored_label(
            text_color(),
            "1. Имена папок, исключаемые из обхода (шаблоны):",
        );
        ui.colored_label(
            subdued_color(),
            "Папки с этими именами полностью пропускаются при обходе (.git, node_modules, target и др.).",
        );
        render_string_list(
            ui,
            &mut self.draft.exclude_folder_names,
            &mut self.new_exclude_folder,
            "folder_names_list",
            "Имя папки...",
        );
        ui.separator();

        // Список 2: Папки «только документы»
        ui.colored_label(text_color(), "2. Папки «только документы» по имени:");
        ui.colored_label(
            subdued_color(),
            "В таких папках индексируются исключительно документы, без бинарных и временных файлов.",
        );
        render_string_list(
            ui,
            &mut self.draft.documents_only_folder_names,
            &mut self.new_docs_only_folder,
            "docs_only_folders_list",
            "Имя папки...",
        );
        ui.separator();

        // Список 3: Пути
        ui.colored_label(text_color(), "3. Системные и служебные пути:");
        ui.colored_label(
            subdued_color(),
            "Системные папки Windows и стандартные каталоги, исключаемые из обхода на всех томах.",
        );
        render_string_list(
            ui,
            &mut self.draft.exclude_paths,
            &mut self.new_exclude_path,
            "exclude_paths_list",
            "Путь или шаблон...",
        );
        ui.separator();

        // Список 4: Файлы
        ui.colored_label(text_color(), "4. Шаблоны исключаемых файлов:");
        ui.colored_label(
            subdued_color(),
            "Системные файлы и блокировки, пропускаемые при обходе (pagefile.sys, ~$* и др.).",
        );
        render_string_list(
            ui,
            &mut self.draft.exclude_files,
            &mut self.new_exclude_file,
            "exclude_files_list",
            "Имя или маска файла...",
        );
    }

    // ========================================================================
    // 3. Вкладка «Форматы и пределы»
    // ========================================================================
    fn render_formats_tab(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Форматы и пределы извлечения")
                .size(15.0)
                .strong(),
        );
        ui.colored_label(
            subdued_color(),
            "Управление извлечением текста по группам форматов и ограничениями на объём и время.",
        );
        ui.add_space(6.0);

        ui.colored_label(text_color(), "Извлекать текст по группам форматов:");
        ui.checkbox(
            &mut self.draft.extract_text_formats,
            "Текстовые форматы (txt, md, csv, json, xml, yaml, ini, log, исходный код)",
        );
        ui.colored_label(
            subdued_color(),
            "Обычные текстовые файлы и исходный код с автоопределением кодировки.",
        );

        ui.checkbox(
            &mut self.draft.extract_office_formats,
            "Офисные документы (docx, xlsx, pptx, odt, ods, odp)",
        );
        ui.colored_label(
            subdued_color(),
            "Документы Microsoft Office и OpenDocument с извлечением текста и таблиц.",
        );

        ui.checkbox(
            &mut self.draft.extract_book_formats,
            "Книги и разметка (html, htm, rtf, fb2, epub)",
        );
        ui.colored_label(
            subdued_color(),
            "Электронные книги по порядку глав и форматированный текст.",
        );

        ui.checkbox(&mut self.draft.extract_pdf_format, "Файлы PDF");
        ui.colored_label(
            subdued_color(),
            "Извлечение текстового слоя из документов в формате PDF.",
        );

        ui.separator();
        ui.colored_label(text_color(), "Пределы извлекаемых символов:");

        ui.horizontal(|ui| {
            ui.label("Книги и документы:");
            ui.add(
                egui::DragValue::new(&mut self.draft.limit_chars_books_documents)
                    .range(1_000..=100_000_000)
                    .speed(100_000),
            );
        });
        ui.colored_label(
            subdued_color(),
            "Максимальное количество символов из книг и офисных документов (умолчание: 20 000 000).",
        );

        ui.horizontal(|ui| {
            ui.label("Исходный код и служебные файлы:");
            ui.add(
                egui::DragValue::new(&mut self.draft.limit_chars_code_service)
                    .range(1_000..=10_000_000)
                    .speed(10_000),
            );
        });
        ui.colored_label(
            subdued_color(),
            "Предел символов для файлов исходного кода, конфигураций и логов (умолчание: 200 000).",
        );

        ui.horizontal(|ui| {
            ui.label("Прочие форматы:");
            ui.add(
                egui::DragValue::new(&mut self.draft.limit_chars_other)
                    .range(1_000..=5_000_000)
                    .speed(10_000),
            );
        });
        ui.colored_label(
            subdued_color(),
            "Предел символов для всех остальных текстовых форматов (умолчание: 100 000).",
        );

        ui.horizontal(|ui| {
            ui.label("Большой txt/md файл:");
            ui.add(
                egui::DragValue::new(&mut self.draft.limit_chars_large_txt)
                    .range(1_000..=10_000_000)
                    .speed(10_000),
            );
        });
        ui.colored_label(
            subdued_color(),
            "Предел символов для текстовых файлов, превысивших порог размера (умолчание: 200 000).",
        );

        ui.horizontal(|ui| {
            ui.label("Порог «большой txt/md», МБ:");
            ui.add(egui::DragValue::new(&mut self.draft.large_txt_threshold_mb).range(1..=100));
        });
        ui.colored_label(
            subdued_color(),
            "Размер файла в мегабайтах, начиная с которого txt/md считается большим (умолчание: 5 МБ).",
        );

        ui.separator();
        ui.colored_label(text_color(), "Архивы ZIP:");
        ui.checkbox(
            &mut self.draft.zip_scan,
            "Заходить внутрь ZIP-архивов для индексирования",
        );
        ui.colored_label(
            subdued_color(),
            "Индексация вложенных файлов архивов с указанием пути через разделитель « → ».",
        );

        ui.horizontal(|ui| {
            ui.label("Предел размера файла внутри ZIP, МБ:");
            ui.add(egui::DragValue::new(&mut self.draft.zip_max_file_size_mb).range(1..=500));
        });
        ui.colored_label(
            subdued_color(),
            "Файлы внутри архива больше этого предела пропускаются со статусом (умолчание: 20 МБ).",
        );

        ui.separator();
        ui.colored_label(text_color(), "Извлекатель PDF:");
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut self.draft.pdf_extractor,
                PdfExtractorMode::Builtin,
                "Встроенный (чистый Rust)",
            );
            ui.radio_value(
                &mut self.draft.pdf_extractor,
                PdfExtractorMode::External,
                "Внешний pdftotext",
            );
        });
        ui.colored_label(
            subdued_color(),
            "Встроенный извлекатель на Rust не требует внешних программ; внешний использует pdftotext.",
        );

        if self.draft.pdf_extractor == PdfExtractorMode::External {
            ui.horizontal(|ui| {
                ui.label("Путь к pdftotext:");
                let mut path_str = self.draft.pdf_external_path.to_string_lossy().to_string();
                if ui.text_edit_singleline(&mut path_str).changed() {
                    self.draft.pdf_external_path = PathBuf::from(path_str);
                }
            });
            ui.colored_label(
                subdued_color(),
                "Укажите полный путь к исполняемому файлу pdftotext.exe из пакета Poppler.",
            );
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Предел времени извлечения одного файла, с:");
            ui.add(egui::DragValue::new(&mut self.draft.extract_timeout_seconds).range(5..=600));
        });
        ui.colored_label(
            subdued_color(),
            "Защита от зависания рабочего процесса на повреждённых документах (умолчание: 120 с).",
        );
    }

    // ========================================================================
    // 4. Вкладка «Закрытые данные»
    // ========================================================================
    fn render_sensitive_data_tab(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Закрытые и конфиденциальные данные")
                .size(15.0)
                .strong(),
        );
        ui.colored_label(
            subdued_color(),
            "Файлы, подпадающие под правила закрытых данных, никогда не читаются для защиты тайн и паролей.",
        );
        ui.add_space(6.0);

        ui.colored_label(
            text_color(),
            "Встроенные правила безопасности (не выключаются):",
        );
        ui.colored_label(
            subdued_color(),
            "Эти шаблоны встроены в ядро программы и гарантируют сохранность личных данных:",
        );

        let builtin_rules = [
            ".env*",
            "*.env",
            ".ssh",
            ".gnupg",
            "credentials",
            "secrets",
            "auth.json",
            "logins.json",
            "private_key",
            "*password*",
            "*парол*",
        ];
        ui.add_space(4.0);
        for rule in builtin_rules {
            ui.colored_label(subdued_color(), format!("• {rule}"));
        }

        ui.separator();
        ui.colored_label(text_color(), "Дополнительные пользовательские шаблоны:");
        ui.colored_label(
            subdued_color(),
            "Добавьте свои имена или маски файлов, содержимое которых не должно индексироваться.",
        );

        render_string_list(
            ui,
            &mut self.draft.secret_extra_patterns,
            &mut self.new_secret_pattern,
            "secret_patterns_list",
            "Шаблон конфиденциального файла...",
        );
    }

    // ========================================================================
    // 5. Вкладка «Поиск»
    // ========================================================================
    fn render_search_tab(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Параметры поиска и выдачи")
                .size(15.0)
                .strong(),
        );
        ui.colored_label(
            subdued_color(),
            "Настройка морфологии, режима многословных запросов и отображения фрагментов.",
        );
        ui.add_space(6.0);

        ui.checkbox(
            &mut self.draft.enable_morphology,
            "Русские словоформы (морфологический словарь morph.fst)",
        );
        ui.colored_label(
            subdued_color(),
            "Позволяет находить слова во всех падежах, числах и склонениях (спешившую → спешившая).",
        );

        ui.separator();
        ui.colored_label(text_color(), "Режим сопоставления нескольких слов:");
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut self.draft.multi_word_mode,
                MultiWordMode::StrictPhrase,
                "строгая фраза",
            );
            ui.radio_value(
                &mut self.draft.multi_word_mode,
                MultiWordMode::AllWords,
                "все слова",
            );
        });
        ui.colored_label(
            subdued_color(),
            "«Строгая фраза» ищет слова строго подряд; «все слова» ищет в любом месте документа.",
        );

        ui.separator();
        ui.colored_label(text_color(), "Словоформы в запросе:");
        ui.horizontal(|ui| {
            ui.radio_value(
                &mut self.draft.word_forms_mode,
                WordFormsMode::AllForms,
                "все формы",
            );
            ui.radio_value(
                &mut self.draft.word_forms_mode,
                WordFormsMode::Exact,
                "точно как набрано",
            );
        });
        ui.colored_label(
            subdued_color(),
            "Тот же выбор есть в главном окне. «Точно как набрано»: «книга» не найдёт «книги».",
        );

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Число результатов в выдаче (1–500):");
            ui.add(egui::DragValue::new(&mut self.draft.max_results).range(1..=500));
        });
        ui.colored_label(
            subdued_color(),
            "Предельное количество групп результатов поиска на экране (умолчание: 200).",
        );

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Длина фрагмента (до / после):");
            ui.add(egui::DragValue::new(&mut self.draft.fragment_before).range(10..=1000));
            ui.label("символов до,");
            ui.add(egui::DragValue::new(&mut self.draft.fragment_after).range(10..=2000));
            ui.label("символов после.");
        });
        ui.colored_label(
            subdued_color(),
            "Длина окружающего контекста в карточке найденного документа (умолчание: 140 / 300).",
        );

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Сортировка по умолчанию:");
            egui::ComboBox::from_id_salt("default_sort_combo")
                .selected_text(sort_mode_title(self.draft.default_sort))
                .show_ui(ui, |ui| {
                    for (mode, title) in SORT_MODES {
                        selectable_choice(ui, &mut self.draft.default_sort, mode, title);
                    }
                });
        });
        ui.colored_label(
            subdued_color(),
            "Порядок ранжирования результатов при новом поисковом запросе.",
        );

        ui.separator();
        ui.colored_label(text_color(), "Слова «книжных» папок:");
        ui.colored_label(
            subdued_color(),
            "Имена папок, указывающие на категорию «Книга» (books, ebooks, библиотека и др.).",
        );
        render_string_list(
            ui,
            &mut self.draft.book_folder_words,
            &mut self.new_book_folder_word,
            "book_words_list",
            "Слово или шаблон папки...",
        );
    }

    // ========================================================================
    // 6. Вкладка «Обновление»
    // ========================================================================
    fn render_update_tab(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Обновление и автозапуск")
                .size(15.0)
                .strong(),
        );
        ui.colored_label(
            subdued_color(),
            "Режимы фоновой сверки источников и автозапуска программы вместе с Windows.",
        );
        ui.add_space(6.0);

        ui.checkbox(
            &mut self.draft.update_on_startup,
            "Искать изменения и обновлять поиск при запуске программы",
        );
        ui.colored_label(
            subdued_color(),
            "Автоматический запуск сверки изменённых и новых файлов при открытии программы.",
        );

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Периодическое обновление, минут (0 — выкл.):");
            ui.add(egui::DragValue::new(&mut self.draft.update_interval_minutes).range(0..=1440));
        });
        ui.colored_label(
            subdued_color(),
            "Интервал фоновой разностной сверки в минутах (0 — отключить периодическое обновление).",
        );

        ui.separator();
        ui.checkbox(
            &mut self.draft.autostart,
            "Запускать при входе в Windows (в фоновом режиме)",
        );
        ui.colored_label(
            subdued_color(),
            "Добавляет команду запуска программы в реестр Windows (HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run).",
        );
    }

    // ========================================================================
    // 7. Вкладка «Данные и производительность»
    // ========================================================================
    fn render_data_tab(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Данные и производительность")
                .size(15.0)
                .strong(),
        );
        ui.colored_label(
            subdued_color(),
            "Расположение папки данных, распределение ресурсов процессора и срок хранения журналов.",
        );
        ui.add_space(6.0);

        ui.colored_label(text_color(), "Папка данных программы:");
        ui.horizontal(|ui| {
            let path_display = if self.draft.data_dir.as_os_str().is_empty() {
                "%LOCALAPPDATA%\\ПоисКовичеК (по умолчанию)".to_string()
            } else {
                self.draft.data_dir.display().to_string()
            };
            ui.label(path_display);

            if ui.button("Выбрать папку…").clicked()
                && let Some(new_dir) = pick_folder()
            {
                self.draft.data_dir = new_dir.clone();
                self.selected_new_data_dir = Some(new_dir);
            }
        });
        ui.colored_label(
            subdued_color(),
            "Каталог для базы SQLite, индекса tantivy и файлов журнала.",
        );

        if self.selected_new_data_dir.is_some() {
            ui.add_space(4.0);
            ui.colored_label(
                warning_color(),
                "Выбрана новая папка данных. Выберите вариант переноса:",
            );
            ui.radio_value(
                &mut self.relocate_mode,
                RelocateMode::Move,
                "Перенести существующие данные (базу, индекс, имена) в новую папку",
            );
            ui.radio_value(
                &mut self.relocate_mode,
                RelocateMode::StartEmpty,
                "Начать с пустой папки (старые данные останутся на месте)",
            );
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Число рабочих процессов извлечения текста (0 — по числу ядер):");
            ui.add(egui::DragValue::new(&mut self.draft.worker_threads).range(0..=64));
        });
        ui.colored_label(
            subdued_color(),
            "Количество параллельных фоновых процессов извлечения текста (0 — автовыбор).",
        );

        ui.separator();
        ui.checkbox(
            &mut self.draft.hdd_gentle_mode,
            "Щадящий режим для жестких дисков (HDD)",
        );
        ui.colored_label(
            subdued_color(),
            "Ограничивает чтение не более чем одним потоком на физический том для снижения шума и износа.",
        );

        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Хранить журнал, дней:");
            ui.add(egui::DragValue::new(&mut self.draft.log_keep_days).range(1..=365));
        });
        ui.colored_label(
            subdued_color(),
            "Срок автоматического хранения ежедневных файлов журнала работы программы (умолчание: 14 дней).",
        );
    }

    // ========================================================================
    // 8. Вкладка «Интерфейс»
    // ========================================================================
    fn render_interface_tab(&mut self, ui: &mut egui::Ui) {
        ui.colored_label(
            accent_color(),
            egui::RichText::new("Интерфейс").size(15.0).strong(),
        );
        ui.colored_label(
            subdued_color(),
            "Выбор темы оформления и внешнего вида программы.",
        );
        ui.add_space(6.0);

        ui.colored_label(text_color(), "Тема:");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.draft.theme, Theme::Dark, "Тёмная");
            ui.radio_value(&mut self.draft.theme, Theme::Light, "Светлая");
            ui.radio_value(&mut self.draft.theme, Theme::Blue, "Синяя");
            ui.radio_value(&mut self.draft.theme, Theme::Brown, "Коричневая");
        });
        ui.add_space(4.0);
        ui.colored_label(
            subdued_color(),
            "• «Тёмная» — существующее оформление и значение по умолчанию.\n\
             • «Светлая» — полноценный светлый фон.\n\
             • «Синяя» — тёмно-синяя основа.\n\
             • «Коричневая» — тёплая коричневая основа.",
        );
    }

    // ========================================================================
    // Нижняя панель диалога (ошибки валидации + кнопки)
    // ========================================================================
    fn render_footer(&mut self, ui: &mut egui::Ui, open: &mut bool) -> SettingsDialogAction {
        self.validation_errors = self.draft.validate();
        let is_valid = self.validation_errors.is_empty();

        if !is_valid {
            ui.vertical(|ui| {
                for err in &self.validation_errors {
                    ui.colored_label(error_color(), format!("• {err}"));
                }
            });
            ui.add_space(4.0);
        }

        let mut action = SettingsDialogAction::None;

        ui.horizontal(|ui| {
            if ui.button("Вернуть умолчания").clicked() {
                self.confirm_reset_defaults = true;
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let save_btn = ui.add_enabled(
                    is_valid,
                    egui::Button::new(egui::RichText::new("Сохранить").strong()),
                );

                if save_btn.clicked() {
                    let relocate_info = self
                        .selected_new_data_dir
                        .take()
                        .map(|d| (d, self.relocate_mode));

                    action = SettingsDialogAction::Save(Box::new(SettingsSaveResult {
                        new_settings: self.draft.clone(),
                        old_settings: self.original.clone(),
                        relocate: relocate_info,
                    }));
                    *open = false;
                }

                if ui.button("Отмена").clicked() {
                    action = SettingsDialogAction::Cancel;
                    *open = false;
                }
            });
        });

        // Модальное подтверждение сброса настроек к умолчаниям
        if self.confirm_reset_defaults {
            let mut reset_open = true;
            egui::Window::new("Сброс настроек")
                .open(&mut reset_open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .title_frame(theme_title_frame())
                .show(ui.ctx(), |ui| {
                    ui.colored_label(
                        warning_color(),
                        "Вернуть все настройки к значениям по умолчанию? Источники останутся без изменений.",
                    );
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Сбросить").clicked() {
                            let kept_sources = self.draft.sources.clone();
                            self.draft = Settings::default();
                            self.draft.sources = kept_sources;
                            self.confirm_reset_defaults = false;
                        }
                        if ui.button("Отмена").clicked() {
                            self.confirm_reset_defaults = false;
                        }
                    });
                });
            if !reset_open {
                self.confirm_reset_defaults = false;
            }
        }

        action
    }
}

/// Вспомогательная функция отрисовки списка строк с добавлением и удалением
fn render_string_list(
    ui: &mut egui::Ui,
    items: &mut Vec<String>,
    new_item_buffer: &mut String,
    id_salt: &str,
    placeholder: &str,
) {
    ui.push_id(id_salt, |ui| {
        egui::ScrollArea::vertical()
            .max_height(120.0)
            .show(ui, |ui| {
                let mut remove_idx = None;
                for (idx, item) in items.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("• {item}"));
                        if ui.button("×").clicked() {
                            remove_idx = Some(idx);
                        }
                    });
                }
                if let Some(i) = remove_idx {
                    items.remove(i);
                }
            });

        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(new_item_buffer).hint_text(placeholder));
            if ui.button("Добавить").clicked() {
                let trimmed = new_item_buffer.trim().to_string();
                if !trimmed.is_empty() && !items.contains(&trimmed) {
                    items.push(trimmed);
                    new_item_buffer.clear();
                }
            }
        });
    });
}

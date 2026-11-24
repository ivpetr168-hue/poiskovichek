use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

static PROCESS_START_TIME: OnceLock<Instant> = OnceLock::new();

pub fn record_process_start(start: Instant) {
    let _ = PROCESS_START_TIME.set(start);
}

pub fn process_start_time() -> Instant {
    *PROCESS_START_TIME.get_or_init(Instant::now)
}

use crossbeam_channel::{Receiver, Sender};

use crate::duplicates::{DuplicateProgress, DuplicateStage};
use crate::journal;
use crate::obnovlenie::{self, Itog, Opisanie, Sobytie};
use crate::paths::AppPaths;
use crate::pipeline::{CancelToken, FinishedSummary, PipelineError, PipelineOptions, Progress};
use crate::relocate::RelocateMode;
use crate::search::{SearchEngine, SearchFilters, SearchRequest, SearchResponse};
use crate::settings::{MultiWordMode, Settings, SortMode, Source, SourceMode, WordFormsMode};
use crate::snippet::Snippet;
use crate::ui::dialogs::{
    pick_files, pick_folder, show_about_dialog, show_confirm_reindex_dialog,
    show_confirm_remove_record_dialog, show_in_folder, show_reindex_sources_dialog,
    show_remove_source_dialog,
};
use crate::ui::results::{SnippetState, render_results_list};
use crate::ui::settings_dialog::{SettingsDialogAction, SettingsDialogState, SettingsSaveResult};
use crate::ui::theme::{
    accent_color, apply_theme, configure_fonts, error_color, selectable_choice, set_theme,
    subdued_color, theme_title_frame, warning_color,
};
use crate::ui::wizard::{WizardAction, WizardState};
use crate::ui::{SORT_MODES, sort_mode_title};

/// Название операции в нижней строке до первого сообщения о ходе работы.
const TITLE_INDEXING: &str = "Индексирование: запуск…";
const TITLE_SYNC: &str = "Обновление дисков: запуск…";

/// Итог фоновой перезагрузки поискового движка: источники из базы `(id, путь)`.
type EngineReloadResult = Result<Vec<(i64, String)>, String>;

/// Новый движок после переноса папки данных: пути и открытый движок.
type EngineOpenResult = Result<(AppPaths, SearchEngine), String>;

/// Параметры конвейера с ходом работы для окна: сообщения о ходе пересылаются в очередь окна.
/// Поток-пересыльщик завершается сам, когда конвейер отпускает передающий конец канала.
fn options_with_progress(tx: &Sender<PipelineResultMsg>, cancel: CancelToken) -> PipelineOptions {
    let (ptx, prx) = crossbeam_channel::unbounded::<Progress>();
    let tx_clone = tx.clone();
    std::thread::spawn(move || {
        while let Ok(p) = prx.recv() {
            let _ = tx_clone.send(PipelineResultMsg::Progress(p));
        }
    });
    PipelineOptions {
        cancel_token: Some(cancel),
        progress_tx: Some(ptx),
        ..Default::default()
    }
}

/// Читает из базы источники `(id, путь)`; при ошибке — пустой список.
pub fn read_db_sources(conn: &rusqlite::Connection) -> Vec<(i64, String)> {
    let Ok(mut stmt) = conn.prepare("SELECT id, path FROM sources ORDER BY id;") else {
        return Vec::new();
    };
    let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) else {
        return Vec::new();
    };
    rows.flatten().collect()
}

/// Список источников для фильтра «Все диски»: включённые источники из настроек с их настоящим
/// id из таблицы `sources` (сопоставление по пути). Источники, которых ещё нет в базе
/// (индексирование не запускалось), в список не попадают.
pub fn filter_sources_for(
    settings_sources: &[Source],
    db_sources: &[(i64, String)],
) -> Vec<(i64, String)> {
    settings_sources
        .iter()
        .filter(|s| s.enabled)
        .filter_map(|s| {
            let path = s.path.to_string_lossy().to_string();
            db_sources
                .iter()
                .find(|(_, db_path)| *db_path == path)
                .map(|(id, _)| (*id, path))
        })
        .collect()
}

/// Сообщение о результате выгрузки отчёта
pub enum ExportResultMsg {
    Success(PathBuf),
    Error(String),
}

/// Сообщение о прогрессе и результате поиска дубликатов
pub enum DuplicateProgressMsg {
    Progress(DuplicateProgress),
    Finished(DuplicatesEnd),
}

/// Чем закончился поиск дубликатов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DuplicatesEnd {
    /// Доведён до конца: групп и сколько места занимают лишние копии.
    Done {
        groups: usize,
        reclaimable: u64,
    },
    /// Остановлен пользователем (это не ошибка).
    Stopped,
    Failed(String),
}

/// Итог последней завершившейся операции для нижней строки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastOutcome {
    /// Время окончания, ЧЧ:ММ.
    pub time: String,
    pub text: String,
    pub is_error: bool,
}

/// Вид строки состояния (цвет выбирает окно).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Warning,
    Error,
    Muted,
}

/// Что показать в нижней строке, когда не идёт индексирование и не ищутся дубликаты.
/// Выполняющийся перенос и выгрузка важнее итога; иначе — итог последней операции, какой бы она ни была.
pub fn bottom_status(
    relocate_progress: Option<&str>,
    is_relocating: bool,
    is_exporting: bool,
    last: Option<&LastOutcome>,
) -> (String, StatusKind) {
    if is_relocating {
        (
            relocate_progress
                .unwrap_or("Перенос папки данных…")
                .to_string(),
            StatusKind::Warning,
        )
    } else if is_exporting {
        ("Выгрузка перечня файлов…".to_string(), StatusKind::Info)
    } else if let Some(o) = last {
        (
            format!("{} {}", o.time, o.text),
            if o.is_error {
                StatusKind::Error
            } else {
                StatusKind::Info
            },
        )
    } else {
        ("Готов к поиску".to_string(), StatusKind::Muted)
    }
}

/// Размер для итоговых строк: «5,6 ГБ», «12,0 МБ», «3,5 КБ», «900 Б».
pub fn format_bytes_ru(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= KB * KB * KB {
        format!("{} ГБ", format_decimal(b / (KB * KB * KB)))
    } else if b >= KB * KB {
        format!("{} МБ", format_decimal(b / (KB * KB)))
    } else if b >= KB {
        format!("{} КБ", format_decimal(b / KB))
    } else {
        format!("{bytes} Б")
    }
}

/// Итог поиска дубликатов для нижней строки (без времени). `last` — последнее сообщение о ходе.
pub fn duplicates_outcome_text(end: &DuplicatesEnd, last: Option<&DuplicateProgress>) -> String {
    match end {
        DuplicatesEnd::Done { groups: 0, .. } => {
            "Поиск дубликатов завершён: дубликатов не найдено.".to_string()
        }
        DuplicatesEnd::Done {
            groups,
            reclaimable,
        } => format!(
            "Поиск дубликатов завершён: групп — {}, можно освободить {}.",
            format_thousands(*groups),
            format_bytes_ru(*reclaimable)
        ),
        DuplicatesEnd::Stopped => {
            let tail = "при следующем запуске продолжится с этого места.";
            match last {
                Some(p) if p.stage == DuplicateStage::QuickHash => format!(
                    "Поиск дубликатов остановлен: посчитано {} из {} файлов, {tail}",
                    format_thousands(p.files_done),
                    format_thousands(p.files_total)
                ),
                Some(p) => format!(
                    "Поиск дубликатов остановлен: быстрые отпечатки готовы, полные — {} из {} файлов, {tail}",
                    format_thousands(p.files_done),
                    format_thousands(p.files_total)
                ),
                None => format!("Поиск дубликатов остановлен, {tail}"),
            }
        }
        DuplicatesEnd::Failed(e) => format!("Ошибка поиска дубликатов: {e}"),
    }
}

/// Строка хода поиска дубликатов в окне: «Дубликаты: быстрые отпечатки — 12 345 из 106 762».
pub fn duplicates_progress_text(p: &DuplicateProgress) -> String {
    match p.stage {
        DuplicateStage::QuickHash => format!(
            "Дубликаты: быстрые отпечатки — {} из {}",
            format_thousands(p.files_done),
            format_thousands(p.files_total)
        ),
        DuplicateStage::FullHash => {
            let mut s = format!(
                "Дубликаты: полные отпечатки — {} из {}",
                format_thousands(p.files_done),
                format_thousands(p.files_total)
            );
            if p.bytes_total > 0 {
                s.push_str(&format!(
                    " ({} из {})",
                    format_bytes_ru(p.bytes_done.min(p.bytes_total)),
                    format_bytes_ru(p.bytes_total)
                ));
            }
            s
        }
        DuplicateStage::Finishing => "Дубликаты: сборка групп…".to_string(),
    }
}

/// Название операции индексирования для итога по заголовку, с которым она запущена.
pub fn pipeline_operation_label(title: &str) -> &'static str {
    if title.starts_with("Обновление дисков") {
        "Обновление дисков"
    } else if title.starts_with("Перестраиваю") {
        "Перестройка поиска"
    } else if title.starts_with("Убираю") {
        "Удаление из поиска"
    } else {
        "Индексирование"
    }
}

/// Итог операции индексирования: «Обновление дисков: добавлено — 4; изменено — 1199; убрано — 3.»
pub fn pipeline_outcome_text(label: &str, summary: &FinishedSummary) -> String {
    format!(
        "{label}: добавлено — {}; изменено — {}; убрано — {}.",
        summary.added, summary.changed, summary.deleted
    )
}

/// Местное время ЧЧ:ММ.
pub fn local_hhmm() -> String {
    use windows::Win32::System::SystemInformation::GetLocalTime;
    let st = unsafe { GetLocalTime() };
    format!("{:02}:{:02}", st.wHour, st.wMinute)
}

/// Параметры снимка экрана для режима самопроверки `--snimok`
#[derive(Clone, Debug)]
pub struct SnimokOptions {
    pub output_path: PathBuf,
    pub zapros: Option<String>,
    pub okno: Option<String>,
}

pub struct SearchTask {
    pub generation: u64,
    pub request: SearchRequest,
}

pub struct SearchResultMsg {
    pub generation: u64,
    pub response: SearchResponse,
}

pub struct SnippetTask {
    pub generation: u64,
    pub item_key: String,
    pub request: SearchRequest,
}

pub struct SnippetResultMsg {
    pub generation: u64,
    pub item_key: String,
    pub snippet: Snippet,
}

pub enum PipelineResultMsg {
    Progress(Progress),
    Finished(Result<FinishedSummary, PipelineError>),
}

/// Контроллер главного окна ПоисКовичеКа
pub struct PoiskovichekApp {
    pub paths: AppPaths,
    pub settings: Settings,
    pub engine: Arc<SearchEngine>,
    pub engine_holder: Arc<std::sync::RwLock<Arc<SearchEngine>>>,

    // Состояние строки поиска и истории
    pub query_text: String,
    pub last_searched_query: String,
    pub filters: SearchFilters,
    pub sort_mode: SortMode,
    pub multi_word_mode: MultiWordMode,
    pub word_forms_mode: WordFormsMode,
    pub debounce_deadline: Option<Instant>,
    pub query_generation: u64,
    pub history_idx: Option<usize>,
    pub draft_query: String,

    // Результаты поиска
    pub is_searching: bool,
    pub search_response: Option<SearchResponse>,
    pub search_error: Option<String>,
    pub search_elapsed: Option<Duration>,

    // Кэш сниппетов (поколение, ключ) -> состояние
    pub snippet_cache: HashMap<(u64, String), SnippetState>,

    // Каналы поиска и сниппетов
    pub search_task_tx: Sender<SearchTask>,
    pub search_result_rx: Receiver<SearchResultMsg>,
    pub snippet_task_tx: Sender<SnippetTask>,
    pub snippet_result_rx: Receiver<SnippetResultMsg>,

    // Состояние индексации / конвейера
    pub is_indexing: bool,
    pub index_progress: Option<Progress>,
    /// Итог последней завершившейся операции (любой) для нижней строки.
    pub last_outcome: Option<LastOutcome>,
    pub cancel_token: Option<CancelToken>,
    pub pipeline_rx: Option<Receiver<PipelineResultMsg>>,
    pub last_sync_time: Instant,
    /// Название текущей операции для нижней строки, пока нет сообщений о ходе работы.
    pub operation_title: String,

    // Перезагрузка поискового движка в фоне (имена и дубликаты из базы)
    pub engine_reload_rx: Option<Receiver<EngineReloadResult>>,
    pub engine_reload_pending: bool,
    // Открытие движка в новой папке данных после переноса
    pub engine_open_rx: Option<Receiver<EngineOpenResult>>,

    // Источники для фильтра «Все диски»: как записаны в базе и после сопоставления с настройками
    pub db_sources: Vec<(i64, String)>,
    pub filter_sources: Vec<(i64, String)>,

    // Диалоговые окна
    pub show_dialog_about: bool,
    pub show_dialog_settings: bool,
    pub settings_dialog_state: Option<SettingsDialogState>,
    pub show_prompt_reindex: bool,
    pub show_dialog_reindex_saved: bool,
    pub show_dialog_remove_source: bool,
    pub show_dialog_reindex_sources: bool,
    pub reindex_selected_sources: Vec<PathBuf>,
    pub confirm_remove_path: Option<String>,

    // Мастер первого запуска
    pub wizard_open: bool,
    pub wizard_state: WizardState,

    // Перенос данных
    pub is_relocating: bool,
    pub relocate_rx: Option<Receiver<Result<(PathBuf, Settings), String>>>,
    pub relocate_progress: Option<String>,

    // Выгрузка отчетов
    pub is_exporting: bool,
    pub export_rx: Option<Receiver<ExportResultMsg>>,
    pub export_target_dir: Option<PathBuf>,
    pub export_notice: Option<PathBuf>,

    // Поиск дубликатов
    pub is_finding_duplicates: bool,
    pub duplicates_rx: Option<Receiver<DuplicateProgressMsg>>,
    pub duplicates_cancel: Option<CancelToken>,
    pub duplicates_progress: Option<DuplicateProgress>,
    pub duplicates_available: bool,
    /// Название текущей операции индексирования для итога («Обновление дисков» и т. п.).
    pub operation_label: &'static str,

    // Самообновление
    pub obnovlenie_tx: Sender<Sobytie>,
    pub obnovlenie_rx: Receiver<Sobytie>,
    /// Вышедшая версия, о которой сказано в нижней панели.
    pub dostupnoe_obnovlenie: Option<Opisanie>,
    /// Пользователь нажал «Позже»: до следующего запуска фоновая проверка строку не показывает.
    pub obnovlenie_pozzhe: bool,
    /// Идёт ручная проверка (повторно не запускаем).
    pub idet_proverka_obnovleniy: bool,
    /// Ход скачивания: получено и всего байт.
    pub skachivanie_obnovleniya: Option<(u64, u64)>,
    pub otmena_skachivaniya: Option<Arc<AtomicBool>>,

    // Режим снимка окна
    pub snimok_options: Option<SnimokOptions>,
    pub snimok_start_time: Instant,
    pub snimok_search_ready_time: Option<Instant>,
    pub snimok_requested: bool,
    pub snimok_saved: Arc<AtomicBool>,
    pub snimok_frames_stable: u32,
    pub snimok_index_start: Option<Instant>,
    pub open_actions_menu: bool,

    // Флаг фонового режима и пробуждения
    pub should_restore_window: Arc<AtomicBool>,
    pub first_frame: bool,
    pub process_start_time: Instant,
    pub query_changed_at: Option<Instant>,
    pub egui_ctx: Arc<OnceLock<egui::Context>>,
}

impl PoiskovichekApp {
    pub fn new(
        paths: AppPaths,
        settings: Settings,
        engine: Arc<SearchEngine>,
        snimok_options: Option<SnimokOptions>,
        initial_ctx: Option<egui::Context>,
    ) -> Self {
        let egui_ctx: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
        if let Some(ctx) = initial_ctx {
            let _ = egui_ctx.set(ctx);
        }

        let (search_task_tx, search_task_rx) = crossbeam_channel::unbounded::<SearchTask>();
        let (search_result_tx, search_result_rx) =
            crossbeam_channel::unbounded::<SearchResultMsg>();

        let (snippet_task_tx, snippet_task_rx) = crossbeam_channel::unbounded::<SnippetTask>();
        let (snippet_result_tx, snippet_result_rx) =
            crossbeam_channel::unbounded::<SnippetResultMsg>();

        // Фоновый поток поиска с поддержкой динамического обновления движка
        let eng_holder = Arc::new(std::sync::RwLock::new(engine.clone()));
        let eng_holder_for_search = eng_holder.clone();
        let ctx_holder_search = egui_ctx.clone();
        std::thread::spawn(move || {
            while let Ok(task) = search_task_rx.recv() {
                let mut latest = task;
                while let Ok(newer) = search_task_rx.try_recv() {
                    if newer.generation > latest.generation {
                        latest = newer;
                    }
                }
                let eng = eng_holder_for_search.read().unwrap().clone();
                let resp = eng.search(latest.request);
                let _ = search_result_tx.send(SearchResultMsg {
                    generation: latest.generation,
                    response: resp,
                });
                if let Some(ctx) = ctx_holder_search.get() {
                    ctx.request_repaint();
                }
            }
        });

        // Пул из 4 фоновых потоков расчета фрагментов
        for _ in 0..4 {
            let rx = snippet_task_rx.clone();
            let tx = snippet_result_tx.clone();
            let eng_holder_for_snippets = eng_holder.clone();
            let ctx_holder_snippet = egui_ctx.clone();
            std::thread::spawn(move || {
                while let Ok(task) = rx.recv() {
                    let eng = eng_holder_for_snippets.read().unwrap().clone();
                    let snip = eng.snippet_for(&task.item_key, &task.request);
                    let _ = tx.send(SnippetResultMsg {
                        generation: task.generation,
                        item_key: task.item_key,
                        snippet: snip,
                    });
                    if let Some(ctx) = ctx_holder_snippet.get() {
                        ctx.request_repaint();
                    }
                }
            });
        }

        let should_restore_window = Arc::new(AtomicBool::new(false));
        start_single_instance_listener(should_restore_window.clone(), egui_ctx.clone());

        let (obnovlenie_tx, obnovlenie_rx) = crossbeam_channel::unbounded::<Sobytie>();

        let sort_mode = settings.default_sort;
        let multi_word_mode = settings.multi_word_mode;
        let word_forms_mode = settings.word_forms_mode;

        // Проверяем наличие дубликатов в базе каталога для активности фильтра
        let duplicates_available = {
            if paths.catalog_db.is_file() {
                rusqlite::Connection::open_with_flags(
                    &paths.catalog_db,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )
                .and_then(|c| {
                    c.query_row("SELECT COUNT(*) FROM duplicate_groups;", [], |r| {
                        r.get::<_, i64>(0)
                    })
                })
                .map(|cnt| cnt > 0)
                .unwrap_or(false)
            } else {
                false
            }
        };

        let db_sources = engine
            .read_connection()
            .map(|c| read_db_sources(&c))
            .unwrap_or_default();
        let filter_sources = filter_sources_for(&settings.sources, &db_sources);

        let mut app = Self {
            paths,
            settings,
            engine,
            engine_holder: eng_holder,
            query_text: String::new(),
            last_searched_query: String::new(),
            filters: SearchFilters::default(),
            sort_mode,
            multi_word_mode,
            word_forms_mode,
            debounce_deadline: None,
            query_generation: 0,
            history_idx: None,
            draft_query: String::new(),
            is_searching: false,
            search_response: None,
            search_error: None,
            search_elapsed: None,
            snippet_cache: HashMap::new(),
            search_task_tx,
            search_result_rx,
            snippet_task_tx,
            snippet_result_rx,
            is_indexing: false,
            index_progress: None,
            last_outcome: None,
            cancel_token: None,
            pipeline_rx: None,
            last_sync_time: Instant::now(),
            operation_title: TITLE_INDEXING.to_string(),
            engine_reload_rx: None,
            engine_reload_pending: false,
            engine_open_rx: None,
            db_sources,
            filter_sources,
            show_dialog_about: false,
            show_dialog_settings: false,
            settings_dialog_state: None,
            show_prompt_reindex: false,
            show_dialog_reindex_saved: false,
            show_dialog_remove_source: false,
            show_dialog_reindex_sources: false,
            reindex_selected_sources: Vec::new(),
            confirm_remove_path: None,
            wizard_open: false,
            wizard_state: WizardState::new(),
            is_relocating: false,
            relocate_rx: None,
            relocate_progress: None,
            is_exporting: false,
            export_rx: None,
            export_target_dir: None,
            export_notice: None,
            is_finding_duplicates: false,
            duplicates_rx: None,
            duplicates_cancel: None,
            duplicates_progress: None,
            duplicates_available,
            operation_label: "Индексирование",
            obnovlenie_tx,
            obnovlenie_rx,
            dostupnoe_obnovlenie: None,
            obnovlenie_pozzhe: false,
            idet_proverka_obnovleniy: false,
            skachivanie_obnovleniya: None,
            otmena_skachivaniya: None,
            snimok_options,
            snimok_start_time: Instant::now(),
            snimok_search_ready_time: None,
            snimok_requested: false,
            snimok_saved: Arc::new(AtomicBool::new(false)),
            snimok_frames_stable: 0,
            snimok_index_start: None,
            open_actions_menu: false,
            should_restore_window,
            first_frame: true,
            process_start_time: process_start_time(),
            query_changed_at: None,
            egui_ctx,
        };

        // Если задан режим снимка с параметрами
        if app.snimok_options.is_some() {
            journal::info("ui", "окно открылось");
            let okno = app.snimok_options.as_ref().and_then(|o| o.okno.clone());
            let zapros = app.snimok_options.as_ref().and_then(|o| o.zapros.clone());
            if let Some(ref okno) = okno {
                match okno.as_str() {
                    "o-programme" => app.show_dialog_about = true,
                    "nastroyki" => app.show_dialog_settings = true,
                    "master" => app.wizard_open = true,
                    "menu" => app.open_actions_menu = true,
                    "indeksirovanie" => app.start_snimok_indexing(),
                    "obnovlenie" => {
                        // Строка об обновлении без сети
                        app.dostupnoe_obnovlenie = Some(Opisanie {
                            version: "9.9.9".to_string(),
                            url: String::new(),
                            sha256: String::new(),
                            size: 0,
                            notes: String::new(),
                        });
                    }
                    _ => {}
                }
            }
            if let Some(ref q) = zapros {
                app.query_text = q.clone();
                app.wizard_open = false;
                app.query_changed_at = Some(Instant::now());
                app.trigger_search();
            }
        } else if app.settings.sources.is_empty() {
            app.wizard_open = true;
        } else if app.settings.update_on_startup {
            // При запуске программы — сверка источников
            app.action_sync_all();
        }

        app
    }

    /// Проверяет, можно ли выполнять операции, модифицирующие базу данных.
    pub fn can_perform_indexing_action(&self) -> bool {
        !self.is_indexing
    }

    /// Форматирует сводку результатов поиска (R2-Б-11)
    pub fn format_status_summary(total: usize, count: usize, elapsed: Duration) -> String {
        let time_str = if elapsed.as_millis() >= 1 {
            format!("{} мс", elapsed.as_millis())
        } else {
            let ms = elapsed.as_secs_f64() * 1000.0;
            format!("{:.1} мс", ms).replace('.', ",")
        };
        format!("Найдено: {total}. Показано: {count}. {time_str}")
    }

    /// Форматирует итог операции индексирования (R2-Б-61..63)
    pub fn format_finished_status(summary: &FinishedSummary) -> String {
        format!(
            "Готово: добавлено — {}; изменено — {}; убрано — {}.",
            summary.added, summary.changed, summary.deleted
        )
    }

    /// Запоминает итог только что завершившейся операции: он заменяет прежний, какой бы тот ни был.
    pub fn record_outcome(&mut self, text: String, is_error: bool) {
        self.last_outcome = Some(LastOutcome {
            time: local_hhmm(),
            text,
            is_error,
        });
    }

    /// Строка и вид нижней панели, когда не идёт индексирование и не ищутся дубликаты.
    pub fn status_line(&self) -> (String, StatusKind) {
        bottom_status(
            self.relocate_progress.as_deref(),
            self.is_relocating,
            self.is_exporting,
            self.last_outcome.as_ref(),
        )
    }

    /// Добавляет запрос в историю (до 50 записей)
    pub fn add_to_history(&mut self, q: &str) {
        let trimmed = q.trim();
        if trimmed.is_empty() {
            return;
        }
        self.settings.query_history.retain(|item| item != trimmed);
        self.settings.query_history.insert(0, trimmed.to_string());
        if self.settings.query_history.len() > 50 {
            self.settings.query_history.truncate(50);
        }
        let _ = self.settings.save(&self.paths.settings_file);
    }

    /// Перемещение вверх по истории запросов
    pub fn history_up(&mut self) {
        if self.settings.query_history.is_empty() {
            return;
        }
        match self.history_idx {
            None => {
                self.draft_query = self.query_text.clone();
                self.history_idx = Some(0);
                self.query_text = self.settings.query_history[0].clone();
            }
            Some(idx) => {
                if idx + 1 < self.settings.query_history.len() {
                    let next = idx + 1;
                    self.history_idx = Some(next);
                    self.query_text = self.settings.query_history[next].clone();
                }
            }
        }
    }

    /// Перемещение вниз по истории запросов
    pub fn history_down(&mut self) {
        if let Some(idx) = self.history_idx {
            if idx > 0 {
                let prev = idx - 1;
                self.history_idx = Some(prev);
                self.query_text = self.settings.query_history[prev].clone();
            } else {
                self.history_idx = None;
                self.query_text = self.draft_query.clone();
            }
        }
    }

    /// Запускает поисковый запрос в фоновом потоке
    pub fn trigger_search(&mut self) {
        if self.query_changed_at.is_none() {
            self.query_changed_at = Some(Instant::now());
        }
        self.debounce_deadline = None;
        self.query_generation += 1;
        let gen_id = self.query_generation;
        let query_text = self.query_text.trim().to_string();

        if !query_text.is_empty() {
            self.add_to_history(&query_text);
        }

        self.last_searched_query = query_text.clone();
        self.is_searching = true;
        self.search_error = None;

        let request = SearchRequest {
            text: query_text,
            filters: self.filters.clone(),
            sort: self.sort_mode,
            limit: self.settings.max_results,
            mode: self.multi_word_mode,
            forms: self.word_forms_mode,
        };

        let _ = self.search_task_tx.send(SearchTask {
            generation: gen_id,
            request,
        });
    }

    /// Выбор режима словоформ в главном окне: запоминает в настройках и повторяет поиск.
    pub fn set_word_forms_mode(&mut self, mode: WordFormsMode) {
        if self.word_forms_mode == mode {
            return;
        }
        self.word_forms_mode = mode;
        self.settings.word_forms_mode = mode;
        self.after_search_mode_changed();
    }

    /// Выбор режима нескольких слов («фраза подряд» / «все слова») в главном окне.
    pub fn set_multi_word_mode(&mut self, mode: MultiWordMode) {
        if self.multi_word_mode == mode {
            return;
        }
        self.multi_word_mode = mode;
        self.settings.multi_word_mode = mode;
        self.after_search_mode_changed();
    }

    /// Сохраняет настройки после смены переключателя и повторяет поиск.
    fn after_search_mode_changed(&mut self) {
        if let Err(e) = self.settings.save(&self.paths.settings_file) {
            journal::error("settings", &format!("Не удалось сохранить настройки: {e}"));
        }
        self.engine.update_settings(self.settings.clone());
        if !self.query_text.trim().is_empty() || !self.filters.is_empty() {
            self.trigger_search();
        }
    }

    /// Обрабатывает результат поиска (проверяет поколение, отбрасывая устаревшие)
    pub fn handle_search_result(&mut self, msg: SearchResultMsg) {
        // R2-А-38: Устаревшие ответы отбрасываются
        if msg.generation < self.query_generation {
            return;
        }

        self.is_searching = false;
        self.search_elapsed = Some(msg.response.elapsed);
        self.search_response = Some(msg.response);
        self.search_error = None;

        if let Some(changed_at) = self.query_changed_at.take() {
            let ms = changed_at.elapsed().as_millis();
            journal::info(
                "ui",
                &format!("выдача показана через {} мс после изменения запроса", ms),
            );
        }
    }

    /// Запрашивает расчет фрагмента для карточки
    pub fn request_snippet(&mut self, item_key: String) {
        let gen_id = self.query_generation;
        self.snippet_cache
            .insert((gen_id, item_key.clone()), SnippetState::Pending);

        let request = SearchRequest {
            text: self.last_searched_query.clone(),
            filters: self.filters.clone(),
            sort: self.sort_mode,
            limit: self.settings.max_results,
            mode: self.multi_word_mode,
            forms: self.word_forms_mode,
        };

        let _ = self.snippet_task_tx.send(SnippetTask {
            generation: gen_id,
            item_key,
            request,
        });
    }

    /// Запуск действия «Добавить диск или папку в поиск»
    pub fn action_add_folder(&mut self) {
        if !self.can_perform_indexing_action() {
            return;
        }
        if let Some(folder) = pick_folder() {
            let already = self.settings.sources.iter().any(|s| s.path == folder);
            if !already {
                let mode = if crate::drives::is_cloud_path(&folder) {
                    SourceMode::NamesOnly
                } else {
                    SourceMode::Full
                };
                self.settings.sources.push(Source {
                    path: folder.clone(),
                    mode,
                    max_depth: 0,
                    enabled: true,
                    volume_serial: None,
                    extra_excludes: Vec::new(),
                });
                let _ = self.settings.save(&self.paths.settings_file);
            }
            // «Источник добавлен — индексирование начинается сразу»
            self.action_index_sources(vec![folder]);
        }
    }

    /// Запуск действия «Добавить файлы в поиск»
    pub fn action_add_files(&mut self) {
        if !self.can_perform_indexing_action() {
            return;
        }
        if let Some(files) = pick_files() {
            let settings = self.settings.clone();
            self.start_pipeline_operation(TITLE_INDEXING, move |tx, cancel| {
                let opts = options_with_progress(&tx, cancel);
                let res = crate::pipeline::add_paths_with_settings(&settings, &files, opts);
                let _ = tx.send(PipelineResultMsg::Finished(res));
            });
        }
    }

    /// Запуск действия «Найти изменения и обновить поиск»
    pub fn action_sync_all(&mut self) {
        if !self.can_perform_indexing_action() {
            return;
        }
        self.last_sync_time = Instant::now();
        let settings = self.settings.clone();
        self.start_pipeline_operation(TITLE_SYNC, move |tx, cancel| {
            let opts = options_with_progress(&tx, cancel);
            let res = crate::pipeline::sync_all_with_options(&settings, opts);
            let _ = tx.send(PipelineResultMsg::Finished(res));
        });
    }

    /// Источники для повторного индексирования: настоящие записи из настроек по пути
    /// (режим, глубина, исключения диска сохраняются); путь, которого нет в настройках,
    /// индексируется полностью с настройками по умолчанию.
    pub fn sources_for_paths(&self, paths: Vec<PathBuf>) -> Vec<Source> {
        paths
            .into_iter()
            .map(|p| {
                self.settings
                    .sources
                    .iter()
                    .find(|s| s.path == p)
                    .cloned()
                    .unwrap_or_else(|| Source {
                        path: p,
                        mode: SourceMode::Full,
                        max_depth: 0,
                        enabled: true,
                        volume_serial: None,
                        extra_excludes: Vec::new(),
                    })
            })
            .collect()
    }

    /// Запуск индексирования указанных источников
    pub fn action_index_sources(&mut self, sources: Vec<PathBuf>) {
        if !self.can_perform_indexing_action() {
            return;
        }
        let settings = self.settings.clone();
        let sources_vec = self.sources_for_paths(sources);
        self.start_pipeline_operation(TITLE_INDEXING, move |tx, cancel| {
            let opts = options_with_progress(&tx, cancel);
            let res = crate::pipeline::index_sources_with_options(&settings, &sources_vec, opts);
            let _ = tx.send(PipelineResultMsg::Finished(res));
        });
    }

    /// Запуск действия «Перестроить поиск из сохранённых текстов»
    pub fn action_reindex_saved_texts(&mut self) {
        if !self.can_perform_indexing_action() {
            return;
        }
        let settings = self.settings.clone();
        self.start_pipeline_operation(
            "Перестраиваю индекс: запуск…",
            move |tx, cancel| {
                let opts = options_with_progress(&tx, cancel);
                let res = crate::pipeline::reindex_from_saved_texts_with_settings(&settings, opts);
                let _ = tx.send(PipelineResultMsg::Finished(res));
            },
        );
    }

    /// Запуск действия «Удалить запись из поиска (файл на диске останется)».
    /// Путь записи члена архива («архив.zip → файл») передаётся целиком: удаляется только он.
    pub fn action_remove_record(&mut self, path: String) {
        if !self.can_perform_indexing_action() {
            return;
        }
        let target = PathBuf::from(path.trim());
        let settings = self.settings.clone();
        self.start_pipeline_operation(
            "Убираю запись из поиска…",
            move |tx, _cancel| {
                let res = crate::pipeline::remove_paths_with_settings(&settings, &[target]);
                let _ = tx.send(PipelineResultMsg::Finished(res));
            },
        );
    }

    /// Запуск действия «Удалить источник из поиска»
    pub fn action_remove_source(&mut self, source_path: PathBuf) {
        if !self.can_perform_indexing_action() {
            return;
        }
        self.settings.sources.retain(|s| s.path != source_path);
        let _ = self.settings.save(&self.paths.settings_file);
        self.rebuild_filter_sources();

        let data_dir = self.paths.data_dir.clone();
        let path_str = source_path.to_string_lossy().to_string();
        self.start_pipeline_operation(
            "Убираю источник из поиска…",
            move |tx, _cancel| {
                let res = (|| -> Result<FinishedSummary, PipelineError> {
                    let db_path = data_dir.join("catalog.sqlite3");
                    if db_path.is_file() {
                        let mut conn = crate::catalog::Catalog::open_or_create(&db_path)?;
                        let deleted = crate::sync::remove_source(&mut conn, &path_str)?;
                        Ok(FinishedSummary {
                            added: 0,
                            changed: 0,
                            deleted,
                        })
                    } else {
                        Ok(FinishedSummary {
                            added: 0,
                            changed: 0,
                            deleted: 0,
                        })
                    }
                })();
                let _ = tx.send(PipelineResultMsg::Finished(res));
            },
        );
    }

    /// Запуск действия «Выгрузить перечень файлов» (CSV, JSON, HTML)
    pub fn action_export_reports(&mut self) {
        let Some(dest_dir) = pick_folder() else {
            return;
        };

        let cat_db = self.paths.catalog_db.clone();
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.export_rx = Some(rx);
        self.is_exporting = true;
        self.export_target_dir = Some(dest_dir.clone());
        self.export_notice = None;
        let ctx_holder = self.egui_ctx.clone();

        std::thread::spawn(move || {
            let res = crate::export::export_reports(&cat_db, &dest_dir);
            let _ = tx.send(match res {
                Ok(_) => ExportResultMsg::Success(dest_dir),
                Err(e) => ExportResultMsg::Error(e.to_string()),
            });
            if let Some(ctx) = ctx_holder.get() {
                ctx.request_repaint();
            }
        });
    }

    /// Запуск действия «Найти дубликаты»
    pub fn action_find_duplicates(&mut self) {
        if self.is_finding_duplicates {
            return;
        }

        let cat_db = self.paths.catalog_db.clone();
        let cancel = CancelToken::new();
        self.duplicates_cancel = Some(cancel.clone());
        let (tx, rx) = crossbeam_channel::unbounded::<DuplicateProgressMsg>();
        self.duplicates_rx = Some(rx);
        self.is_finding_duplicates = true;
        self.duplicates_progress = None;
        let ctx_holder = self.egui_ctx.clone();

        std::thread::spawn(move || {
            let tx_prog = tx.clone();
            let ctx_prog = ctx_holder.clone();
            let res = crate::duplicates::find_duplicates(&cat_db, &cancel, move |p| {
                let _ = tx_prog.send(DuplicateProgressMsg::Progress(p));
                if let Some(ctx) = ctx_prog.get() {
                    ctx.request_repaint();
                }
            });

            let msg = match res {
                Ok(groups) => DuplicateProgressMsg::Finished(DuplicatesEnd::Done {
                    groups: groups.len(),
                    reclaimable: groups.iter().map(|g| g.reclaimable_bytes).sum(),
                }),
                Err(crate::duplicates::DuplicateError::Cancelled) => {
                    DuplicateProgressMsg::Finished(DuplicatesEnd::Stopped)
                }
                Err(e) => DuplicateProgressMsg::Finished(DuplicatesEnd::Failed(e.to_string())),
            };
            let _ = tx.send(msg);
            if let Some(ctx) = ctx_holder.get() {
                ctx.request_repaint();
            }
        });
    }

    /// Применяет источники, выбранные в мастере первого запуска
    pub fn apply_new_sources(&mut self, sources: Vec<Source>) {
        self.settings.sources = sources;
        let _ = self.settings.save(&self.paths.settings_file);
        self.rebuild_filter_sources();
        self.engine.update_settings(self.settings.clone());
        self.action_sync_all();
    }

    /// Запуск переноса папки данных
    pub fn action_relocate_data(
        &mut self,
        old_data_dir: PathBuf,
        new_data_dir: PathBuf,
        mode: RelocateMode,
        new_settings: Settings,
    ) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.relocate_rx = Some(rx);
        self.is_relocating = true;
        self.relocate_progress = Some("Перенос папки данных…".to_string());
        let ctx_holder = self.egui_ctx.clone();

        std::thread::spawn(move || {
            let res = crate::relocate::relocate_data(&old_data_dir, &new_data_dir, mode);
            let _ = tx.send(res.map(|_| (new_data_dir, new_settings)).map_err(|e| e.to_string()));
            if let Some(ctx) = ctx_holder.get() {
                ctx.request_repaint();
            }
        });
    }

    /// Завершение переноса папки данных: сохраняет настройки и открывает поисковый движок
    /// в новой папке в фоновом потоке (на большой базе построение имён занимает заметное время,
    /// окно в это время не замирает). Результат подхватывает `poll_incoming`.
    pub fn finish_relocate(&mut self, new_data_dir: PathBuf, mut new_settings: Settings) {
        new_settings.data_dir = new_data_dir;
        if let Err(e) = new_settings.save(&self.paths.settings_file) {
            journal::error("settings", &format!("Не удалось сохранить настройки: {e}"));
            new_settings.theme = self.settings.theme;
        }
        if let Some(ctx) = self.egui_ctx.get() {
            apply_theme(ctx, new_settings.theme);
        } else {
            set_theme(new_settings.theme);
        }
        self.settings = new_settings;
        self.relocate_rx = None;
        self.relocate_progress = Some("Открываю базу в новой папке…".to_string());

        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let mut new_paths = crate::paths::resolve(&exe_dir, &self.settings);
        new_paths.settings_file = self.paths.settings_file.clone();
        let _ = new_paths.ensure_dirs();
        let settings = self.settings.clone();

        let (tx, rx) = crossbeam_channel::bounded::<EngineOpenResult>(1);
        self.engine_open_rx = Some(rx);
        let ctx_holder = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let morph_path = crate::paths::morph_path(&exe_dir);
            let morph = if morph_path.is_file() {
                crate::morph::Morph::open(&morph_path).ok()
            } else {
                None
            };
            // Режим «начать с пустой папки»: базы в новой папке ещё нет
            let created = if new_paths.catalog_db.is_file() {
                Ok(())
            } else {
                crate::catalog::Catalog::open_or_create(&new_paths.catalog_db)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            };
            let res = created.and_then(|_| {
                SearchEngine::open(&new_paths, &settings, morph)
                    .map(|engine| (new_paths, engine))
                    .map_err(|e| e.to_string())
            });
            let _ = tx.send(res);
            if let Some(ctx) = ctx_holder.get() {
                ctx.request_repaint();
            }
        });
    }

    /// Подхватывает движок, открытый в новой папке данных после переноса.
    fn install_relocated_engine(&mut self, res: EngineOpenResult) {
        self.engine_open_rx = None;
        self.is_relocating = false;
        self.relocate_progress = None;
        match res {
            Ok((new_paths, new_engine)) => {
                let arc_engine = Arc::new(new_engine);
                self.paths = new_paths;
                self.engine = arc_engine.clone();
                *self
                    .engine_holder
                    .write()
                    .unwrap_or_else(|e| e.into_inner()) = arc_engine;
                // Перезагрузка прежнего движка теряет смысл: новый построен заново
                self.engine_reload_rx = None;
                self.engine_reload_pending = false;
                self.db_sources = self
                    .engine
                    .read_connection()
                    .map(|c| read_db_sources(&c))
                    .unwrap_or_default();
                self.rebuild_filter_sources();
                self.duplicates_available = self.engine_has_duplicates();
                self.trigger_search();
                self.record_outcome("Перенос папки данных завершён.".to_string(), false);
            }
            Err(e) => {
                self.record_outcome(format!("Ошибка открытия базы в новой папке: {e}"), true);
            }
        }
    }

    /// Есть ли в базе группы дубликатов (для активности фильтра «Только дубликаты»).
    fn engine_has_duplicates(&self) -> bool {
        self.engine
            .read_connection()
            .and_then(|c| {
                c.query_row("SELECT COUNT(*) FROM duplicate_groups;", [], |r| {
                    r.get::<_, i64>(0)
                })
                .map_err(Into::into)
            })
            .map(|cnt| cnt > 0)
            .unwrap_or(false)
    }

    /// Пересобирает список источников для фильтра «Все диски» из настроек и списка из базы.
    /// Если выбранный в фильтре источник пропал, фильтр сбрасывается.
    pub fn rebuild_filter_sources(&mut self) {
        self.filter_sources = filter_sources_for(&self.settings.sources, &self.db_sources);
        if let Some(selected) = self.filters.source_id
            && !self.filter_sources.iter().any(|(id, _)| *id == selected)
        {
            self.filters.source_id = None;
        }
    }

    /// Запускает перезагрузку поискового движка в фоновом потоке: индекс, имена файлов и группы
    /// дубликатов перечитываются из базы. Пока она идёт, поиск работает на прежних данных;
    /// по готовности `poll_incoming` обновляет список источников и повторяет поиск.
    /// Если перезагрузка уже идёт, запоминается просьба повторить её сразу после.
    pub fn start_engine_reload(&mut self) {
        if self.engine_open_rx.is_some() {
            // Движок в новой папке строится заново: перезагружать прежний не нужно
            return;
        }
        if self.engine_reload_rx.is_some() {
            self.engine_reload_pending = true;
            return;
        }
        let engine = self.engine.clone();
        let (tx, rx) = crossbeam_channel::bounded::<EngineReloadResult>(1);
        self.engine_reload_rx = Some(rx);
        let ctx_holder = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let res = engine.reload().map_err(|e| e.to_string()).map(|_| {
                engine
                    .read_connection()
                    .map(|c| read_db_sources(&c))
                    .unwrap_or_default()
            });
            let _ = tx.send(res);
            if let Some(ctx) = ctx_holder.get() {
                ctx.request_repaint();
            }
        });
    }

    /// Применяет изменённые настройки из диалога настроек
    pub fn apply_settings(&mut self, save_result: SettingsSaveResult) {
        let settings_file = self.paths.settings_file.clone();

        let new_settings = save_result.new_settings;
        let old_settings = save_result.old_settings;

        // 1. Автозапуск Windows
        if new_settings.autostart != old_settings.autostart {
            let current_exe = std::env::current_exe().unwrap_or_default();
            let _ = crate::autostart::set_autostart(new_settings.autostart, &current_exe);
        }

        // 2. Смена папки данных
        if let Some((new_data_dir, mode)) = save_result.relocate {
            let old_data_dir = self.paths.data_dir.clone();
            self.action_relocate_data(old_data_dir, new_data_dir, mode, new_settings);
            return;
        }

        // 3. Сохранение настроек в файл
        if let Err(e) = new_settings.save(&settings_file) {
            journal::error("settings", &format!("Не удалось сохранить настройки: {e}"));
        }

        // Обновление темы оформления
        if new_settings.theme != old_settings.theme {
            if let Some(ctx) = self.egui_ctx.get() {
                apply_theme(ctx, new_settings.theme);
            } else {
                set_theme(new_settings.theme);
            }
        }

        // 4. Проверяем изменение правил индексации
        let sources_or_rules_changed = new_settings.sources != old_settings.sources
            || new_settings.exclude_folder_names != old_settings.exclude_folder_names
            || new_settings.documents_only_folder_names != old_settings.documents_only_folder_names
            || new_settings.exclude_paths != old_settings.exclude_paths
            || new_settings.exclude_files != old_settings.exclude_files
            || new_settings.extract_text_formats != old_settings.extract_text_formats
            || new_settings.extract_office_formats != old_settings.extract_office_formats
            || new_settings.extract_book_formats != old_settings.extract_book_formats
            || new_settings.extract_pdf_format != old_settings.extract_pdf_format
            || new_settings.limit_chars_books_documents != old_settings.limit_chars_books_documents
            || new_settings.limit_chars_code_service != old_settings.limit_chars_code_service
            || new_settings.limit_chars_other != old_settings.limit_chars_other
            || new_settings.limit_chars_large_txt != old_settings.limit_chars_large_txt
            || new_settings.large_txt_threshold_mb != old_settings.large_txt_threshold_mb
            || new_settings.zip_scan != old_settings.zip_scan
            || new_settings.zip_max_file_size_mb != old_settings.zip_max_file_size_mb
            || new_settings.secret_extra_patterns != old_settings.secret_extra_patterns
            || new_settings.pdf_extractor != old_settings.pdf_extractor
            || new_settings.pdf_external_path != old_settings.pdf_external_path
            || new_settings.extract_timeout_seconds != old_settings.extract_timeout_seconds;

        // 5. Перезапуск таймера фонового обновления
        if new_settings.update_interval_minutes != old_settings.update_interval_minutes {
            self.last_sync_time = Instant::now();
        }

        // 6. Обновление настроек поиска (действуют со следующего запроса)
        self.multi_word_mode = new_settings.multi_word_mode;
        self.word_forms_mode = new_settings.word_forms_mode;
        self.sort_mode = new_settings.default_sort;
        self.settings = new_settings.clone();
        self.rebuild_filter_sources();
        self.engine.update_settings(new_settings);

        if !self.query_text.trim().is_empty() {
            self.trigger_search();
        }

        // 7. Если изменились правила обхода — предлагаем обновить поиск
        if sources_or_rules_changed {
            self.show_prompt_reindex = true;
        }
    }

    fn start_pipeline_operation(
        &mut self,
        title: &str,
        op: impl FnOnce(Sender<PipelineResultMsg>, CancelToken) + Send + 'static,
    ) {
        self.is_indexing = true;
        self.operation_label = pipeline_operation_label(title);
        // Цифры прошлой операции не должны оставаться на экране до первого сообщения новой
        self.index_progress = None;
        self.operation_title = title.to_string();
        let token = CancelToken::new();
        self.cancel_token = Some(token.clone());

        let (tx, rx) = crossbeam_channel::unbounded::<PipelineResultMsg>();
        self.pipeline_rx = Some(rx);

        let ctx_holder = self.egui_ctx.clone();
        let (inner_tx, inner_rx) = crossbeam_channel::unbounded::<PipelineResultMsg>();
        std::thread::spawn(move || {
            while let Ok(msg) = inner_rx.recv() {
                let _ = tx.send(msg);
                if let Some(ctx) = ctx_holder.get() {
                    ctx.request_repaint();
                }
            }
        });

        std::thread::spawn(move || {
            op(inner_tx, token);
        });
    }

    /// Запускает фоновую проверку обновлений: первая через 10 с, затем каждые 6 часов.
    /// В режиме снимка не запускается.
    pub fn start_update_checker(&self) {
        if self.snimok_options.is_some() {
            return;
        }
        obnovlenie::zapustit_fonovuyu_proverku(self.obnovlenie_tx.clone(), self.egui_ctx.clone());
    }

    /// Действие «Проверить обновления»: итог — в строке состояния.
    pub fn action_check_updates(&mut self) {
        if self.idet_proverka_obnovleniy {
            return;
        }
        self.idet_proverka_obnovleniy = true;
        self.record_outcome("Проверка обновлений…".to_string(), false);
        obnovlenie::proverit_v_fone(self.obnovlenie_tx.clone(), self.egui_ctx.clone());
    }

    /// Кнопка «Обновить»: скачивание установщика в фоне.
    pub fn action_download_update(&mut self) {
        let Some(op) = self.dostupnoe_obnovlenie.clone() else {
            return;
        };
        if self.skachivanie_obnovleniya.is_some() {
            return;
        }
        let otmena = Arc::new(AtomicBool::new(false));
        self.otmena_skachivaniya = Some(otmena.clone());
        self.skachivanie_obnovleniya = Some((0, op.size));
        obnovlenie::skachat_v_fone(
            op,
            otmena,
            self.obnovlenie_tx.clone(),
            self.egui_ctx.clone(),
        );
    }

    /// Запускает скачанный установщик и закрывает программу штатно: настройки сохраняются,
    /// идущие операции останавливаются через токены отмены.
    fn install_update_and_close(&mut self, ctx: &egui::Context, installer: &std::path::Path) {
        if let Err(e) = obnovlenie::zapustit_ustanovshchik(installer) {
            journal::error("obnovlenie", &format!("не запустить установщик: {e}"));
            self.record_outcome(
                format!("Обновление: не удалось запустить установщик — {e}"),
                true,
            );
            return;
        }
        journal::info("obnovlenie", "установщик запущен, программа закрывается");
        if let Some(ref token) = self.cancel_token {
            token.cancel();
        }
        if let Some(ref token) = self.duplicates_cancel {
            token.cancel();
        }
        let _ = self.settings.save(&self.paths.settings_file);
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Разбирает события проверки и скачивания обновления.
    fn poll_obnovlenie(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.obnovlenie_rx.try_recv() {
            ctx.request_repaint();
            match msg {
                Sobytie::Proverka { ruchnaya, itog } => {
                    if ruchnaya {
                        self.idet_proverka_obnovleniy = false;
                    }
                    match itog {
                        Ok(Itog::Est(op)) => {
                            if ruchnaya {
                                self.obnovlenie_pozzhe = false;
                            }
                            if ruchnaya || !self.obnovlenie_pozzhe {
                                if ruchnaya {
                                    self.record_outcome(
                                        format!("Вышла версия {}", op.version),
                                        false,
                                    );
                                }
                                self.dostupnoe_obnovlenie = Some(op);
                            }
                        }
                        Ok(Itog::NetObnovleniy) => {
                            if ruchnaya {
                                self.record_outcome(
                                    format!(
                                        "Обновлений нет, у вас последняя версия {}",
                                        obnovlenie::TEKUSHCHAYA_VERSIYA
                                    ),
                                    false,
                                );
                            }
                        }
                        Err(e) => {
                            if ruchnaya {
                                self.record_outcome(
                                    format!("Проверка обновлений: ошибка — {e}"),
                                    true,
                                );
                            }
                        }
                    }
                }
                Sobytie::Hod { gotovo, vsego } => {
                    if self.skachivanie_obnovleniya.is_some() {
                        self.skachivanie_obnovleniya = Some((gotovo, vsego));
                    }
                }
                Sobytie::Skachano(put) => {
                    self.skachivanie_obnovleniya = None;
                    self.otmena_skachivaniya = None;
                    self.install_update_and_close(ctx, &put);
                }
                Sobytie::OshibkaSkachivaniya(e) => {
                    self.skachivanie_obnovleniya = None;
                    self.otmena_skachivaniya = None;
                    self.record_outcome(format!("Обновление: ошибка — {e}"), true);
                }
                Sobytie::Otmeneno => {
                    self.skachivanie_obnovleniya = None;
                    self.otmena_skachivaniya = None;
                    self.record_outcome("Скачивание обновления отменено".to_string(), false);
                }
            }
        }
    }

    /// Проверяет входящие сообщения из очередей и таймеров
    pub fn poll_incoming(&mut self, ctx: &egui::Context) {
        self.poll_obnovlenie(ctx);

        // 1. Результаты поиска
        while let Ok(msg) = self.search_result_rx.try_recv() {
            self.handle_search_result(msg);
            ctx.request_repaint();
        }

        // Если только что был отправлен поисковый запрос (при вводе или при запуске со снимком),
        // даём фоновому потоку поиска до 10 мс завершиться, чтобы выдать результат уже в текущем кадре
        if self.search_response.is_none()
            && self.query_changed_at.is_some()
            && let Ok(msg) = self
                .search_result_rx
                .recv_timeout(Duration::from_millis(10))
        {
            self.handle_search_result(msg);
            ctx.request_repaint();
        }

        // 2. Результаты сниппетов
        while let Ok(msg) = self.snippet_result_rx.try_recv() {
            if msg.generation == self.query_generation {
                self.snippet_cache.insert(
                    (msg.generation, msg.item_key),
                    SnippetState::Ready(msg.snippet),
                );
                ctx.request_repaint();
            }
        }

        // 3. Сообщения конвейера индексирования
        if let Some(ref rx) = self.pipeline_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    PipelineResultMsg::Progress(p) => {
                        self.index_progress = Some(p);
                        ctx.request_repaint();
                    }
                    PipelineResultMsg::Finished(res) => {
                        self.is_indexing = false;
                        self.cancel_token = None;
                        match res {
                            Ok(summary) => {
                                let text = pipeline_outcome_text(self.operation_label, &summary);
                                self.record_outcome(text, false);
                            }
                            Err(e) => {
                                let text = format!("{}: ошибка — {e}", self.operation_label);
                                self.record_outcome(text, true);
                            }
                        }
                        self.pipeline_rx = None;
                        // Имена и дубликаты перечитываются в фоне; поиск повторится по готовности
                        self.start_engine_reload();
                        ctx.request_repaint();
                        break;
                    }
                }
            }
        }

        // 3б. Сообщения выгрузки отчётов
        let export_msgs: Vec<ExportResultMsg> = self
            .export_rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for msg in export_msgs {
            match msg {
                ExportResultMsg::Success(dir) => {
                    self.is_exporting = false;
                    self.record_outcome(
                        format!("Выгрузка перечня файлов завершена: {}", dir.display()),
                        false,
                    );
                    self.export_notice = Some(dir);
                    ctx.request_repaint();
                }
                ExportResultMsg::Error(err) => {
                    self.is_exporting = false;
                    self.record_outcome(format!("Ошибка выгрузки перечня файлов: {err}"), true);
                    ctx.request_repaint();
                }
            }
        }

        // 3в. Сообщения поиска дубликатов
        if let Some(ref rx) = self.duplicates_rx {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    DuplicateProgressMsg::Progress(p) => {
                        self.duplicates_progress = Some(p);
                        ctx.request_repaint();
                    }
                    DuplicateProgressMsg::Finished(end) => {
                        self.is_finding_duplicates = false;
                        self.duplicates_cancel = None;
                        let text = duplicates_outcome_text(&end, self.duplicates_progress.as_ref());
                        let failed = matches!(end, DuplicatesEnd::Failed(_));
                        self.record_outcome(text, failed);
                        if matches!(end, DuplicatesEnd::Done { .. }) {
                            self.duplicates_available = true;
                            // Группы дубликатов читает движок: перезагрузка в фоне, затем поиск
                            self.start_engine_reload();
                        }
                        self.duplicates_rx = None;
                        ctx.request_repaint();
                        break;
                    }
                }
            }
        }

        // 3г. Сообщения переноса папки данных
        let mut relocate_res = None;
        if let Some(ref rx) = self.relocate_rx {
            while let Ok(res) = rx.try_recv() {
                relocate_res = Some(res);
            }
        }
        if let Some(res) = relocate_res {
            match res {
                Ok((new_data_dir, new_settings)) => {
                    self.finish_relocate(new_data_dir, new_settings);
                    ctx.request_repaint();
                }
                Err(e) => {
                    self.is_relocating = false;
                    self.record_outcome(format!("Ошибка переноса: {e}"), true);
                    self.relocate_rx = None;
                    ctx.request_repaint();
                }
            }
        }

        // 3д. Движок в новой папке данных открыт (после переноса)
        let open_res = self
            .engine_open_rx
            .as_ref()
            .map(|rx| rx.try_recv())
            .and_then(|r| match r {
                Ok(res) => Some(res),
                Err(crossbeam_channel::TryRecvError::Empty) => None,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    Some(Err("поток открытия базы завершился с ошибкой".to_string()))
                }
            });
        if let Some(res) = open_res {
            self.install_relocated_engine(res);
            ctx.request_repaint();
        }

        // 3е. Фоновая перезагрузка движка закончена: обновляем фильтр источников и повторяем поиск
        let reload_res = self
            .engine_reload_rx
            .as_ref()
            .map(|rx| rx.try_recv())
            .and_then(|r| match r {
                Ok(res) => Some(res),
                Err(crossbeam_channel::TryRecvError::Empty) => None,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    Some(Err("поток перезагрузки завершился с ошибкой".to_string()))
                }
            });
        if let Some(res) = reload_res {
            self.engine_reload_rx = None;
            match res {
                Ok(db_sources) => {
                    self.db_sources = db_sources;
                    self.rebuild_filter_sources();
                    self.duplicates_available = self.engine_has_duplicates();
                }
                Err(e) => {
                    journal::error("search", &format!("Не удалось обновить поиск: {e}"));
                }
            }
            if self.engine_reload_pending {
                self.engine_reload_pending = false;
                self.start_engine_reload();
            } else {
                self.trigger_search();
            }
            ctx.request_repaint();
        }

        // 4. Проверка задержки поиска при вводе (120 мс)
        if let Some(deadline) = self.debounce_deadline {
            if Instant::now() >= deadline {
                self.trigger_search();
            } else {
                ctx.request_repaint_after(Duration::from_millis(20));
            }
        }

        // Полоса прогресса фоновых операций
        if self.is_indexing || self.is_finding_duplicates || self.is_relocating {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        // 5. Периодическое фоновое обновление по таймеру
        if self.settings.update_interval_minutes > 0 && !self.is_indexing {
            let interval = Duration::from_secs(self.settings.update_interval_minutes as u64 * 60);
            if self.last_sync_time.elapsed() >= interval {
                self.action_sync_all();
            }
        }

        // 6. Проверка сигнала пробуждения из второго экземпляра
        if self.should_restore_window.swap(false, Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
    }

    /// Строки хода индексирования: по одной на источник и общая строка со скоростью.
    fn render_index_progress(&self, ui: &mut egui::Ui, p: &Progress) {
        let current_idx = p.sources.iter().position(|s| !s.finished);

        for (idx, src) in p.sources.iter().enumerate() {
            let color = if src.finished {
                subdued_color()
            } else if Some(idx) == current_idx {
                accent_color()
            } else {
                ui.style().visuals.text_color()
            };

            if !src.list_done {
                ui.colored_label(
                    color,
                    format!(
                        "{}:  составляю список… найдено {}",
                        src.label,
                        format_thousands(src.found)
                    ),
                );
                continue;
            }

            if src.finished {
                ui.colored_label(
                    color,
                    format!("{}:  готово — {}", src.label, format_thousands(src.found)),
                );
                continue;
            }

            let total = src.found.max(src.done);
            let fraction = if total == 0 {
                1.0
            } else {
                (src.done as f32 / total as f32).clamp(0.0, 1.0)
            };
            ui.horizontal(|ui| {
                ui.colored_label(
                    color,
                    format!(
                        "{}:  обработано {} из {} ({})",
                        src.label,
                        format_thousands(src.done),
                        format_thousands(total),
                        format_percent(src.done, total)
                    ),
                );
                ui.add(
                    egui::ProgressBar::new(fraction)
                        .desired_width(160.0)
                        .desired_height(8.0)
                        .corner_radius(4)
                        .fill(color),
                );
            });
        }

        // Общая строка: этап, скорость и папка последнего обработанного файла
        ui.horizontal(|ui| {
            // Кнопка первой: текст после неё меняется, и кнопка не должна двигаться
            if ui.button("Остановить").clicked()
                && let Some(ref token) = self.cancel_token
            {
                token.cancel();
            }
            if p.sources.is_empty() {
                // Операция без обхода источников (например, перестройка из сохранённых текстов):
                // вместо скорости чтения показываем, сколько записей уже обработано
                ui.colored_label(
                    accent_color(),
                    format!(
                        "Этап: {} — обработано {}",
                        p.phase,
                        format_thousands(p.written.max(p.extracted))
                    ),
                );
            } else {
                ui.colored_label(accent_color(), format!("Этап: {}", p.phase));
                ui.label(format!(
                    "{} МБ/с · {} файлов/с",
                    format_decimal(p.bytes_per_sec as f64 / 1_048_576.0),
                    format_thousands(p.files_per_sec.round() as usize)
                ));
            }
            if !p.current_dir.is_empty() {
                ui.colored_label(subdued_color(), shorten_path_middle(&p.current_dir, 70));
            }
        });
    }

    /// Режим снимка `--okno indeksirovanie`: два пробных источника по 3000 файлов,
    /// запуск индексирования и снимок окна, пока полоса хода видна.
    pub fn start_snimok_indexing(&mut self) {
        let root = std::env::temp_dir().join(format!(
            "poiskovichek-snimok-indeksirovanie-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);

        let base = "Архивный поиск находит каждое слово и точную фразу мгновенно. \
Холодный осенний ветер гнал серые тучи над крышами дачного поселка. ";
        let mut sources = Vec::new();
        for name in ["источник-а", "источник-б"] {
            let dir = root.join(name);
            if std::fs::create_dir_all(&dir).is_err() {
                continue;
            }
            for i in 0..3000 {
                let text = format!("Документ {i:04} источника «{name}».\n{}", base.repeat(30));
                let _ = std::fs::write(dir.join(format!("документ_{i:04}.txt")), text);
            }
            sources.push(Source {
                path: dir,
                mode: SourceMode::Full,
                max_depth: 0,
                enabled: true,
                volume_serial: None,
                extra_excludes: Vec::new(),
            });
        }

        let mut settings = self.settings.clone();
        settings.data_dir = root.join("данные");
        settings.enable_morphology = false;
        settings.sources = sources.clone();

        self.snimok_index_start = Some(Instant::now());
        self.start_pipeline_operation(TITLE_INDEXING, move |tx, cancel| {
            let opts = PipelineOptions {
                in_process_workers: true,
                ..options_with_progress(&tx, cancel)
            };
            let res = crate::pipeline::index_sources_with_options(&settings, &sources, opts);
            let _ = tx.send(PipelineResultMsg::Finished(res));
        });
    }

    /// Проверяет готовность окна для автоматического снимка (`--snimok`)
    fn check_snimok_capture(&mut self, ctx: &egui::Context) {
        let Some(opts) = self.snimok_options.clone() else {
            return;
        };

        if self.snimok_saved.load(Ordering::SeqCst) {
            let has_query = opts
                .zapros
                .as_ref()
                .map(|q| !q.trim().is_empty())
                .unwrap_or(false);
            let total = self.search_response.as_ref().map(|r| r.total).unwrap_or(0);
            if has_query && total == 0 {
                journal::error("ui", "снимок: выдача пустая");
                std::process::exit(3);
            } else {
                std::process::exit(0);
            }
        }

        if self.snimok_requested {
            ctx.request_repaint_after(Duration::from_millis(16));
            return;
        }

        // Снимок хода индексирования: ждём 1,8 с, пока полоса и строки источников видны
        if opts.okno.as_deref() == Some("indeksirovanie") {
            let started = self.snimok_index_start.unwrap_or(self.snimok_start_time);
            let has_rows = self
                .index_progress
                .as_ref()
                .is_some_and(|p| !p.sources.is_empty());
            if (!has_rows && started.elapsed() < Duration::from_secs(8))
                || started.elapsed() < Duration::from_millis(1800)
            {
                ctx.request_repaint_after(Duration::from_millis(50));
                return;
            }
            self.capture_snimok(ctx, opts.output_path.clone());
            return;
        }

        let elapsed = self.snimok_start_time.elapsed();
        let has_query = opts
            .zapros
            .as_ref()
            .map(|q| !q.trim().is_empty())
            .unwrap_or(false);

        if has_query {
            let search_timeout = Duration::from_secs(10);
            let has_results = self
                .search_response
                .as_ref()
                .map(|r| r.total > 0)
                .unwrap_or(false);

            // Ждём, пока придёт ответ на запрос с total > 0 (или 10 с)
            let search_stage_done = has_results || elapsed >= search_timeout;
            if !search_stage_done {
                ctx.request_repaint_after(Duration::from_millis(500));
                return;
            }

            if self.snimok_search_ready_time.is_none() {
                self.snimok_search_ready_time = Some(Instant::now());
            }

            let snippets_elapsed = self.snimok_search_ready_time.unwrap().elapsed();
            let snippets_timeout = Duration::from_secs(5);

            let snippets_ready = if let Some(ref resp) = self.search_response {
                if resp.total == 0 {
                    true
                } else {
                    let visible_count = resp.items.len().min(10);
                    (0..visible_count).all(|i| {
                        let k = (self.query_generation, resp.items[i].item_key.clone());
                        matches!(self.snippet_cache.get(&k), Some(SnippetState::Ready(_)))
                    })
                }
            } else {
                true
            };

            // И пока фрагменты видимых карточек готовы (или ещё 5 с)
            if !snippets_ready && snippets_elapsed < snippets_timeout {
                ctx.request_repaint_after(Duration::from_millis(500));
                return;
            }
        }

        self.snimok_frames_stable += 1;
        if self.snimok_frames_stable >= 6 {
            let target_path = opts.output_path.clone();
            self.capture_snimok(ctx, target_path);
        } else {
            ctx.request_repaint();
        }
    }

    /// Запрашивает снимок окна и сохраняет его по указанному пути.
    fn capture_snimok(&mut self, ctx: &egui::Context, target_path: PathBuf) {
        self.snimok_requested = true;
        let is_saved = self.snimok_saved.clone();
        let ctx_save = ctx.clone();

        ctx.request_screenshot(move |image: Arc<egui::ColorImage>| {
            let (w, h) = (image.width() as u32, image.height() as u32);
            let mut raw_bytes = Vec::with_capacity(image.pixels.len() * 4);
            for p in &image.pixels {
                raw_bytes.push(p.r());
                raw_bytes.push(p.g());
                raw_bytes.push(p.b());
                raw_bytes.push(p.a());
            }
            if let Some(rgba) = image::RgbaImage::from_raw(w, h, raw_bytes) {
                if let Some(parent) = target_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = rgba.save(&target_path);
                is_saved.store(true, Ordering::SeqCst);
                ctx_save.request_repaint();
            }
        });
    }
}

// ============================================================================
// Диагностика вертикальной синхронизации и измерение вывода кадров
// ============================================================================

#[derive(Debug, Clone, serde::Serialize)]
pub struct FrameTimingRecord {
    pub frame_nr: usize,
    pub swap_wait_ms: f64,
    pub present_interval_ms: f64,
    pub next_frame_start_ms: f64,
    pub frame_start_interval_ms: f64,
    pub cpu_update_ms: f64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DiagReport {
    pub vsync_enabled: bool,
    pub total_frames: usize,
    pub total_time_ms: f64,
    pub avg_swap_wait_ms: f64,
    pub max_swap_wait_ms: f64,
    pub avg_present_interval_ms: f64,
    pub min_present_interval_ms: f64,
    pub max_present_interval_ms: f64,
    pub std_dev_present_interval_ms: f64,
    pub hitches_gt_20ms: usize,
    pub hitches_gt_30ms: usize,
    pub hitches_gt_45ms: usize,
    pub avg_next_frame_start_ms: f64,
    pub avg_cpu_update_ms: f64,
    pub records: Vec<FrameTimingRecord>,
}

struct DiagCollector {
    vsync_enabled: bool,
    target_frames: usize,
    log_path: PathBuf,
    mouse_hz: Option<u32>,
    async_mouse_started: bool,
    async_mouse_running: Arc<AtomicBool>,
    start_time: Instant,
    last_swap_end: Option<Instant>,
    last_frame_start: Option<Instant>,
    current_swap_wait_ms: f64,
    current_present_interval_ms: f64,
    records: Vec<FrameTimingRecord>,
}

static DIAG_COLLECTOR: std::sync::Mutex<Option<DiagCollector>> = std::sync::Mutex::new(None);

#[cfg(windows)]
static mut ORIGINAL_SWAP_BUFFERS: Option<unsafe extern "system" fn(*mut std::ffi::c_void) -> i32> =
    None;

#[cfg(windows)]
unsafe extern "system" fn diag_hooked_swap_buffers(hdc: *mut std::ffi::c_void) -> i32 {
    let t_before = Instant::now();
    let res = unsafe {
        if let Some(orig) = ORIGINAL_SWAP_BUFFERS {
            orig(hdc)
        } else {
            1
        }
    };
    let t_after = Instant::now();

    if let Ok(mut lock) = DIAG_COLLECTOR.lock() {
        if let Some(col) = lock.as_mut() {
            let wait_ms = (t_after - t_before).as_secs_f64() * 1000.0;
            let present_interval = if let Some(prev) = col.last_swap_end {
                (t_after - prev).as_secs_f64() * 1000.0
            } else {
                0.0
            };
            col.last_swap_end = Some(t_after);
            col.current_swap_wait_ms = wait_ms;
            col.current_present_interval_ms = present_interval;
        }
    }

    res
}

#[cfg(windows)]
unsafe fn install_swap_hook() -> bool {
    unsafe extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut std::ffi::c_void;
        fn VirtualProtect(
            addr: *const std::ffi::c_void,
            size: usize,
            new_prot: u32,
            old_prot: *mut u32,
        ) -> i32;
    }

    let base = unsafe { GetModuleHandleW(std::ptr::null()) } as usize;
    if base == 0 {
        return false;
    }
    let dos = base as *const u8;
    if unsafe { *dos != b'M' || *dos.add(1) != b'Z' } {
        return false;
    }
    let e_lfanew = unsafe { *(dos.add(0x3C) as *const i32) } as usize;
    let nt = unsafe { dos.add(e_lfanew) };
    let magic = unsafe { *(nt.add(24) as *const u16) };
    if magic != 0x20B {
        return false;
    }
    let import_dir_rva = unsafe { *(nt.add(144) as *const u32) } as usize;
    if import_dir_rva == 0 {
        return false;
    }
    let mut desc = unsafe { dos.add(import_dir_rva) } as *const u32;
    while unsafe { *desc.add(3) != 0 || *desc.add(4) != 0 } {
        let name_rva = unsafe { *desc.add(3) } as usize;
        let first_thunk_rva = unsafe { *desc.add(4) } as usize;
        let orig_thunk_rva = unsafe { *desc } as usize;
        let thunk_rva = if orig_thunk_rva != 0 {
            orig_thunk_rva
        } else {
            first_thunk_rva
        };

        let name_ptr = unsafe { dos.add(name_rva) };
        let mut dll_name = Vec::new();
        let mut i = 0;
        while unsafe { *name_ptr.add(i) } != 0 {
            dll_name.push(unsafe { *name_ptr.add(i) });
            i += 1;
        }
        if let Ok(name_str) = std::str::from_utf8(&dll_name) {
            if name_str.eq_ignore_ascii_case("gdi32.dll") {
                let mut int_ptr = unsafe { dos.add(thunk_rva) } as *const usize;
                let mut iat_ptr = unsafe { dos.add(first_thunk_rva) } as *mut usize;
                while unsafe { *int_ptr } != 0 {
                    let val = unsafe { *int_ptr };
                    if (val & (1usize << 63)) == 0 {
                        let hint_name_ptr = unsafe { dos.add(val) };
                        let fn_name_ptr = unsafe { hint_name_ptr.add(2) };
                        let mut fn_name = Vec::new();
                        let mut j = 0;
                        while unsafe { *fn_name_ptr.add(j) } != 0 {
                            fn_name.push(unsafe { *fn_name_ptr.add(j) });
                            j += 1;
                        }
                        if fn_name == b"SwapBuffers" {
                            let original = unsafe { *iat_ptr };
                            let mut old_protect = 0u32;
                            if unsafe {
                                VirtualProtect(
                                    iat_ptr as *const _,
                                    std::mem::size_of::<usize>(),
                                    0x04,
                                    &mut old_protect,
                                )
                            } != 0
                            {
                                unsafe {
                                    *iat_ptr = diag_hooked_swap_buffers as *const () as usize;
                                    let _ = VirtualProtect(
                                        iat_ptr as *const _,
                                        std::mem::size_of::<usize>(),
                                        old_protect,
                                        &mut old_protect,
                                    );
                                    ORIGINAL_SWAP_BUFFERS = Some(std::mem::transmute(original));
                                }
                                return true;
                            }
                        }
                    }
                    int_ptr = unsafe { int_ptr.add(1) };
                    iat_ptr = unsafe { iat_ptr.add(1) };
                }
            }
        }
        desc = unsafe { desc.add(5) };
    }
    false
}

fn diag_is_active() -> bool {
    std::env::args().any(|a| a == "--diag-frames")
        || std::env::var("POISKOVICHEK_DIAG_FRAMES").is_ok()
}

fn diag_init(vsync_enabled: bool) {
    let target_frames = std::env::args()
        .position(|a| a == "--diag-frames")
        .and_then(|p| std::env::args().nth(p + 1))
        .and_then(|s| s.parse::<usize>().ok())
        .or_else(|| {
            std::env::var("POISKOVICHEK_DIAG_FRAMES")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
        })
        .unwrap_or(300);

    let log_path = std::env::args()
        .position(|a| a == "--diag-log")
        .and_then(|p| std::env::args().nth(p + 1))
        .map(PathBuf::from)
        .or_else(|| std::env::var("POISKOVICHEK_DIAG_LOG").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("diag_results.json"));

    let mouse_hz = std::env::args()
        .position(|a| a == "--diag-mouse-hz")
        .and_then(|p| std::env::args().nth(p + 1))
        .and_then(|s| s.parse::<u32>().ok())
        .or_else(|| {
            std::env::var("POISKOVICHEK_DIAG_MOUSE_HZ")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
        });

    let col = DiagCollector {
        vsync_enabled,
        target_frames,
        log_path,
        mouse_hz,
        async_mouse_started: false,
        async_mouse_running: Arc::new(AtomicBool::new(true)),
        start_time: Instant::now(),
        last_swap_end: None,
        last_frame_start: None,
        current_swap_wait_ms: 0.0,
        current_present_interval_ms: 0.0,
        records: Vec::with_capacity(target_frames + 10),
    };
    *DIAG_COLLECTOR.lock().unwrap() = Some(col);
}

fn diag_on_ui_start(ui: &mut egui::Ui, t_ui_start: Instant) {
    let mut should_exit = false;
    let mut exit_log_path = PathBuf::new();
    let mut exit_report = None;

    if let Ok(mut lock) = DIAG_COLLECTOR.lock() {
        if let Some(col) = lock.as_mut() {
            let next_start = if let Some(last_swap) = col.last_swap_end {
                (t_ui_start - last_swap).as_secs_f64() * 1000.0
            } else {
                0.0
            };
            let start_interval = if let Some(last_start) = col.last_frame_start {
                (t_ui_start - last_start).as_secs_f64() * 1000.0
            } else {
                0.0
            };
            col.last_frame_start = Some(t_ui_start);

            let rec = FrameTimingRecord {
                frame_nr: col.records.len() + 1,
                swap_wait_ms: col.current_swap_wait_ms,
                present_interval_ms: col.current_present_interval_ms,
                next_frame_start_ms: next_start,
                frame_start_interval_ms: start_interval,
                cpu_update_ms: 0.0,
            };
            col.records.push(rec);

            // Траектория мыши строго внутри окна программы (1100x780)
            if let Some(hz) = col.mouse_hz {
                if !col.async_mouse_started {
                    col.async_mouse_started = true;
                    let running = Arc::clone(&col.async_mouse_running);
                    let ctx = ui.ctx().clone();
                    let start = col.start_time;
                    let interval = std::time::Duration::from_secs_f64(1.0 / (hz as f64).max(1.0));
                    std::thread::spawn(move || {
                        while running.load(Ordering::Relaxed) {
                            let t = start.elapsed().as_secs_f64();
                            let period = 2.0;
                            let phase = (t % period) / period;
                            let angle = phase * 2.0 * std::f64::consts::PI;
                            let x = 550.0 + 350.0 * angle.sin();
                            let y = 390.0 + 200.0 * angle.cos();
                            let pos = egui::pos2(x as f32, y as f32);

                            ctx.input_mut(|i| {
                                i.events.push(egui::Event::PointerMoved(pos));
                            });
                            ctx.request_repaint();
                            std::thread::sleep(interval);
                        }
                    });
                }
            } else {
                let t = col.start_time.elapsed().as_secs_f64();
                let period = 2.0;
                let phase = (t % period) / period;
                let angle = phase * 2.0 * std::f64::consts::PI;
                let x = 550.0 + 350.0 * angle.sin();
                let y = 390.0 + 200.0 * angle.cos();
                let pos = egui::pos2(x as f32, y as f32);

                ui.ctx().input_mut(|i| {
                    i.events.push(egui::Event::PointerMoved(pos));
                });
                ui.ctx().request_repaint();
            }

            if col.records.len() >= col.target_frames {
                should_exit = true;
                col.async_mouse_running.store(false, Ordering::Relaxed);
                exit_log_path = col.log_path.clone();

                let total = col.records.len();
                let valid_records = if total > 5 {
                    &col.records[5..]
                } else {
                    &col.records[..]
                };
                let count = valid_records.len() as f64;

                let sum_swap: f64 = valid_records.iter().map(|r| r.swap_wait_ms).sum();
                let max_swap: f64 = valid_records
                    .iter()
                    .map(|r| r.swap_wait_ms)
                    .fold(0.0, f64::max);

                let sum_present: f64 = valid_records.iter().map(|r| r.present_interval_ms).sum();
                let min_present: f64 = valid_records
                    .iter()
                    .map(|r| r.present_interval_ms)
                    .fold(f64::MAX, f64::min);
                let max_present: f64 = valid_records
                    .iter()
                    .map(|r| r.present_interval_ms)
                    .fold(0.0, f64::max);

                let avg_present = if count > 0.0 {
                    sum_present / count
                } else {
                    0.0
                };
                let variance: f64 = if count > 1.0 {
                    valid_records
                        .iter()
                        .map(|r| (r.present_interval_ms - avg_present).powi(2))
                        .sum::<f64>()
                        / (count - 1.0)
                } else {
                    0.0
                };
                let std_dev = variance.sqrt();

                let hitches_20 = valid_records
                    .iter()
                    .filter(|r| r.present_interval_ms > 20.0)
                    .count();
                let hitches_30 = valid_records
                    .iter()
                    .filter(|r| r.present_interval_ms > 30.0)
                    .count();
                let hitches_45 = valid_records
                    .iter()
                    .filter(|r| r.present_interval_ms > 45.0)
                    .count();

                let sum_next: f64 = valid_records.iter().map(|r| r.next_frame_start_ms).sum();
                let sum_cpu: f64 = valid_records.iter().map(|r| r.cpu_update_ms).sum();

                let report = DiagReport {
                    vsync_enabled: col.vsync_enabled,
                    total_frames: total,
                    total_time_ms: col.start_time.elapsed().as_secs_f64() * 1000.0,
                    avg_swap_wait_ms: if count > 0.0 {
                        sum_swap / count
                    } else {
                        0.0
                    },
                    max_swap_wait_ms: max_swap,
                    avg_present_interval_ms: avg_present,
                    min_present_interval_ms: if min_present == f64::MAX {
                        0.0
                    } else {
                        min_present
                    },
                    max_present_interval_ms: max_present,
                    std_dev_present_interval_ms: std_dev,
                    hitches_gt_20ms: hitches_20,
                    hitches_gt_30ms: hitches_30,
                    hitches_gt_45ms: hitches_45,
                    avg_next_frame_start_ms: if count > 0.0 {
                        sum_next / count
                    } else {
                        0.0
                    },
                    avg_cpu_update_ms: if count > 0.0 { sum_cpu / count } else { 0.0 },
                    records: col.records.clone(),
                };
                exit_report = Some(report);
            }
        }
    }

    if should_exit {
        if let Some(rep) = exit_report {
            eprintln!(
                "[DIAG SUMMARY] VSync: {}, Frames: {}, AvgPresent: {:.2}ms, StdDev: {:.2}ms, Hitches(>20ms): {}, Hitches(>30ms): {}, Hitches(>45ms): {}, AvgSwapWait: {:.2}ms, AvgCpu: {:.2}ms",
                rep.vsync_enabled, rep.total_frames, rep.avg_present_interval_ms, rep.std_dev_present_interval_ms,
                rep.hitches_gt_20ms, rep.hitches_gt_30ms, rep.hitches_gt_45ms,
                rep.avg_swap_wait_ms, rep.avg_cpu_update_ms
            );
            if let Ok(json) = serde_json::to_string_pretty(&rep) {
                let _ = std::fs::write(&exit_log_path, json);
            }
        }
        std::process::exit(0);
    }
}

fn diag_on_ui_end(t_ui_start: Instant) {
    if let Ok(mut lock) = DIAG_COLLECTOR.lock() {
        if let Some(col) = lock.as_mut() {
            if let Some(last) = col.records.last_mut() {
                last.cpu_update_ms = t_ui_start.elapsed().as_secs_f64() * 1000.0;
            }
        }
    }
}

impl eframe::App for PoiskovichekApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let t_ui_start = std::time::Instant::now();
        diag_on_ui_start(ui, t_ui_start);
        let ctx = ui.ctx().clone();
        let _ = self.egui_ctx.set(ctx.clone());
        self.poll_incoming(&ctx);

        // Запоминание размеров и положения окна
        ctx.input(|i| {
            if let Some(rect) = i.viewport().inner_rect {
                self.settings.window_width = Some(rect.width() as u32);
                self.settings.window_height = Some(rect.height() as u32);
                self.settings.window_x = Some(rect.min.x as i32);
                self.settings.window_y = Some(rect.min.y as i32);
            }
        });

        // Верхняя панель меню «Действия»
        egui::Panel::top("top_menu").show(ui, |ui| {
            ui.horizontal(|ui| {
                let btn = egui::Button::new("Действия");
                let response = ui.add(btn);
                let mut menu = egui::Popup::menu(&response);
                if self.open_actions_menu {
                    menu = menu.open(true);
                }
                menu.show(|ui| {
                    let can_modify = self.can_perform_indexing_action();

                    if ui
                        .add_enabled(
                            can_modify,
                            egui::Button::new("Добавить диск или папку в поиск"),
                        )
                        .clicked()
                    {
                        ui.close();
                        self.action_add_folder();
                    }
                    if ui
                        .add_enabled(can_modify, egui::Button::new("Добавить файлы в поиск"))
                        .clicked()
                    {
                        ui.close();
                        self.action_add_files();
                    }
                    if ui
                        .add_enabled(
                            can_modify,
                            egui::Button::new(
                                "Удалить диск или папку из поиска (файлы на диске останутся)",
                            ),
                        )
                        .clicked()
                    {
                        ui.close();
                        self.show_dialog_remove_source = true;
                    }
                    ui.separator();
                    if ui
                        .add_enabled(
                            can_modify,
                            egui::Button::new("Найти изменения и обновить поиск"),
                        )
                        .clicked()
                    {
                        ui.close();
                        self.action_sync_all();
                    }
                    if ui
                        .add_enabled(
                            can_modify,
                            egui::Button::new("Проиндексировать выбранные диски заново"),
                        )
                        .clicked()
                    {
                        ui.close();
                        self.reindex_selected_sources.clear();
                        self.show_dialog_reindex_sources = true;
                    }
                    if ui
                        .add_enabled(
                            can_modify,
                            egui::Button::new(
                                "Перестроить поиск из сохранённых текстов (файлы не читаются)",
                            ),
                        )
                        .clicked()
                    {
                        ui.close();
                        self.show_dialog_reindex_saved = true;
                    }
                    if ui
                        .add_enabled(can_modify, egui::Button::new("Выгрузить перечень файлов"))
                        .clicked()
                    {
                        ui.close();
                        self.action_export_reports();
                    }
                    if ui
                        .add_enabled(can_modify, egui::Button::new("Найти дубликаты"))
                        .clicked()
                    {
                        ui.close();
                        self.action_find_duplicates();
                    }
                    ui.separator();
                    if ui.button("Настройки").clicked() {
                        ui.close();
                        self.show_dialog_settings = true;
                    }
                    if ui
                        .add_enabled(
                            !self.idet_proverka_obnovleniy,
                            egui::Button::new("Проверить обновления"),
                        )
                        .clicked()
                    {
                        ui.close();
                        self.action_check_updates();
                    }
                    if ui.button("О программе").clicked() {
                        ui.close();
                        self.show_dialog_about = true;
                    }
                });
            });
        });

        // Нижняя панель состояния и полосы индексирования
        let mut obnovit = false;
        let mut pozzhe = false;
        let mut otmenit_skachivanie = false;
        egui::Panel::bottom("bottom_status").show(ui, |ui| {
            // Строка об обновлении — над прочими
            if let Some((gotovo, vsego)) = self.skachivanie_obnovleniya {
                ui.horizontal(|ui| {
                    if ui.button("Отменить").clicked() {
                        otmenit_skachivanie = true;
                    }
                    ui.colored_label(
                        accent_color(),
                        format!(
                            "Скачиваю обновление: {} из {} МБ",
                            format_decimal(gotovo as f64 / 1_048_576.0),
                            format_decimal(vsego as f64 / 1_048_576.0)
                        ),
                    );
                });
            } else if let Some(ref op) = self.dostupnoe_obnovlenie {
                ui.horizontal(|ui| {
                    if ui.button("Обновить").clicked() {
                        obnovit = true;
                    }
                    if ui.button("Позже").clicked() {
                        pozzhe = true;
                    }
                    ui.colored_label(
                        accent_color(),
                        format!(
                            "Вышла версия {} (у вас {})",
                            op.version,
                            obnovlenie::TEKUSHCHAYA_VERSIYA
                        ),
                    );
                });
            }
            if self.is_indexing {
                if let Some(ref p) = self.index_progress {
                    self.render_index_progress(ui, p);
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("Остановить").clicked()
                            && let Some(ref token) = self.cancel_token
                        {
                            token.cancel();
                        }
                        ui.colored_label(accent_color(), self.operation_title.as_str());
                    });
                }
            } else {
                ui.horizontal(|ui| {
                    if self.is_finding_duplicates {
                        if ui.button("Остановить").clicked()
                            && let Some(ref c) = self.duplicates_cancel
                        {
                            c.cancel();
                        }
                        if let Some(ref p) = self.duplicates_progress {
                            ui.colored_label(accent_color(), duplicates_progress_text(p));
                        } else {
                            ui.colored_label(accent_color(), "Поиск дубликатов…");
                        }
                    } else {
                        let (text, kind) = self.status_line();
                        let color = match kind {
                            StatusKind::Info => accent_color(),
                            StatusKind::Warning => warning_color(),
                            StatusKind::Error => error_color(),
                            StatusKind::Muted => subdued_color(),
                        };
                        ui.colored_label(color, text);
                    }
                });
            }
        });

        if obnovit {
            self.action_download_update();
        }
        if pozzhe {
            self.dostupnoe_obnovlenie = None;
            self.obnovlenie_pozzhe = true;
        }
        if otmenit_skachivanie && let Some(ref otmena) = self.otmena_skachivaniya {
            otmena.store(true, Ordering::SeqCst);
        }

        // Центральная область
        egui::CentralPanel::default().show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;

            // 1. Строка поиска
            ui.horizontal(|ui| {
                let search_id = ui.make_persistent_id("search_query_input");
                let text_edit = egui::TextEdit::singleline(&mut self.query_text)
                    .id(search_id)
                    .hint_text("Имя, путь или текст внутри документа…")
                    .desired_width(ui.available_width() - 90.0);

                let response = ui.add(text_edit);

                // Фокус при запуске
                if self.first_frame {
                    response.request_focus();
                    self.first_frame = false;
                    let ms = self.process_start_time.elapsed().as_millis();
                    journal::info(
                        "ui",
                        &format!("окно готово к поиску через {} мс от старта процесса", ms),
                    );
                }

                // Обработка клавиш в поле поиска
                if response.has_focus() {
                    let (up, down, esc, enter) = ctx.input(|i| {
                        (
                            i.key_pressed(egui::Key::ArrowUp),
                            i.key_pressed(egui::Key::ArrowDown),
                            i.key_pressed(egui::Key::Escape),
                            i.key_pressed(egui::Key::Enter),
                        )
                    });

                    if up {
                        self.history_up();
                    } else if down {
                        self.history_down();
                    } else if esc {
                        self.query_text.clear();
                        self.history_idx = None;
                        self.trigger_search();
                    } else if enter {
                        self.trigger_search();
                    }
                }

                if response.changed() {
                    self.history_idx = None;
                    self.query_changed_at = Some(Instant::now());
                    self.debounce_deadline = Some(Instant::now() + Duration::from_millis(120));
                }

                if ui.button("Найти").clicked() {
                    self.trigger_search();
                }
            });

            // 2. Предупреждения поиска
            if let Some(ref resp) = self.search_response {
                for w in &resp.warnings {
                    ui.colored_label(warning_color(), w);
                }
            }

            // 2а. Режимы поиска: словоформы и несколько слов
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(subdued_color(), "Слова:");
                let mut forms = self.word_forms_mode;
                selectable_choice(ui, &mut forms, WordFormsMode::AllForms, "все формы")
                    .on_hover_text(
                        "Находит слово во всех формах: «книга» найдёт «книги», «книгами». \
                         Для слов, которых нет в словаре, — слова с таким началом.",
                    );
                selectable_choice(ui, &mut forms, WordFormsMode::Exact, "точно как набрано")
                    .on_hover_text(
                        "Только целое слово в этой форме: «книга» не найдёт «книги» и «книгалово».",
                    );
                if forms != self.word_forms_mode {
                    self.set_word_forms_mode(forms);
                }

                ui.separator();

                ui.colored_label(subdued_color(), "Несколько слов:");
                let mut multi = self.multi_word_mode;
                selectable_choice(ui, &mut multi, MultiWordMode::StrictPhrase, "фраза подряд")
                    .on_hover_text(
                        "Слова идут подряд в том же порядке: «красный дом» не найдёт «дом красный».",
                    );
                selectable_choice(ui, &mut multi, MultiWordMode::AllWords, "все слова")
                    .on_hover_text(
                        "Все слова есть в документе в любом месте и порядке; точная фраза выше в выдаче.",
                    );
                if multi != self.multi_word_mode {
                    self.set_multi_word_mode(multi);
                }
            });

            // 3. Строка фильтров и сортировки
            ui.horizontal_wrapped(|ui| {
                // Источник / диск: включённые источники из настроек с id из базы
                let selected_source_text = match self.filters.source_id {
                    None => "Все диски".to_string(),
                    Some(id) => self
                        .filter_sources
                        .iter()
                        .find(|(src_id, _)| *src_id == id)
                        .map(|(_, label)| label.clone())
                        .unwrap_or_else(|| format!("Источник #{id}")),
                };
                egui::ComboBox::from_id_salt("filter_source")
                    .selected_text(selected_source_text)
                    .show_ui(ui, |ui| {
                        let mut trigger = false;
                        if selectable_choice(ui, &mut self.filters.source_id, None, "Все диски")
                            .clicked()
                        {
                            trigger = true;
                        }
                        let sources_list = self.filter_sources.clone();
                        for (src_id, label) in sources_list {
                            if selectable_choice(ui, &mut self.filters.source_id, Some(src_id), label)
                                .clicked()
                            {
                                trigger = true;
                            }
                        }
                        if trigger {
                            self.trigger_search();
                        }
                    });

                // Категория
                egui::ComboBox::from_id_salt("filter_category")
                    .selected_text(self.filters.category.as_deref().unwrap_or("Все категории"))
                    .show_ui(ui, |ui| {
                        if selectable_choice(ui, &mut self.filters.category, None, "Все категории")
                            .clicked()
                        {
                            self.trigger_search();
                        }
                        let categories = [
                            "книга",
                            "документ",
                            "изображение",
                            "аудио",
                            "видео",
                            "исходный код",
                            "архив или образ",
                            "установщик или программа",
                            "резервная копия",
                            "прочее",
                        ];
                        for cat in categories {
                            if selectable_choice(ui,
                                    &mut self.filters.category,
                                    Some(cat.to_string()),
                                    cat,
                                )
                                .clicked()
                            {
                                self.trigger_search();
                            }
                        }
                    });

                // Формат
                ui.label("Формат:");
                let mut ext = self.filters.extension.clone().unwrap_or_default();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut ext)
                            .desired_width(60.0)
                            .hint_text("pdf"),
                    )
                    .changed()
                {
                    self.filters.extension = if ext.trim().is_empty() {
                        None
                    } else {
                        Some(ext.trim().to_lowercase())
                    };
                    self.debounce_deadline = Some(Instant::now() + Duration::from_millis(120));
                }

                // Не меньше, МБ
                ui.label("Не меньше, МБ:");
                let mut mb_str = self
                    .filters
                    .min_size
                    .map(|b| (b / 1_048_576).to_string())
                    .unwrap_or_default();
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut mb_str)
                            .desired_width(50.0)
                            .hint_text("0"),
                    )
                    .changed()
                {
                    self.filters.min_size = mb_str
                        .trim()
                        .parse::<u64>()
                        .ok()
                        .filter(|&mb| mb > 0)
                        .map(|mb| mb * 1_048_576);
                    self.debounce_deadline = Some(Instant::now() + Duration::from_millis(120));
                }

                // Только дубликаты
                let dup_cb = ui.add_enabled(
                    self.duplicates_available,
                    egui::Checkbox::new(&mut self.filters.duplicates_only, "Только дубликаты"),
                );
                if !self.duplicates_available {
                    dup_cb.on_disabled_hover_text(
                        "Запустите «Найти дубликаты» в меню «Действия» для включения фильтра",
                    );
                } else if dup_cb.changed() {
                    self.trigger_search();
                }

                ui.separator();

                // Сортировка (4 варианта)
                egui::ComboBox::from_id_salt("sort_mode")
                    .selected_text(sort_mode_title(self.sort_mode))
                    .show_ui(ui, |ui| {
                        let mut changed = false;
                        for (mode, title) in SORT_MODES {
                            changed |= selectable_choice(ui, &mut self.sort_mode, mode, title)
                                .clicked();
                        }
                        if changed {
                            self.trigger_search();
                        }
                    });
            });

            // 4. Сводка «Найдено: N. Показано: M. K мс»
            ui.horizontal(|ui| {
                if self.is_searching {
                    ui.colored_label(accent_color(), "Идёт поиск…");
                } else if let Some(ref err) = self.search_error {
                    ui.colored_label(error_color(), err);
                } else if let Some(ref resp) = self.search_response {
                    ui.colored_label(
                        subdued_color(),
                        Self::format_status_summary(
                            resp.total,
                            resp.items.len(),
                            self.search_elapsed.unwrap_or(resp.elapsed),
                        ),
                    );
                } else if self.query_text.trim().is_empty() && self.filters.is_empty() {
                    if self.settings.sources.is_empty() {
                        ui.colored_label(warning_color(), "Добавьте диск или папку: меню «Действия»");
                    } else {
                        ui.colored_label(subdued_color(), "Введите слова или фразу");
                    }
                }
            });

            ui.separator();

            // Подсказка, если в поиске нет источников
            if self.settings.sources.is_empty() && !self.wizard_open {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.colored_label(
                        warning_color(),
                        egui::RichText::new("Добавьте диск или папку: меню «Действия»").size(15.0),
                    );
                });
            }

            // 5. Виртуальный список карточек результатов
            self.poll_incoming(&ctx);
            let mut requested_snippets = Vec::new();
            let mut pending_remove_path = None;
            if let Some(ref resp) = self.search_response {
                let gen_id = self.query_generation;
                let mut req_snippet = |key: String| {
                    requested_snippets.push(key);
                };
                render_results_list(
                    ui,
                    &resp.items,
                    gen_id,
                    &self.snippet_cache,
                    &mut req_snippet,
                    &mut pending_remove_path,
                );
            }
            if let Some(p) = pending_remove_path {
                self.confirm_remove_path = Some(p);
            }
            for key in requested_snippets {
                self.request_snippet(key);
            }
        });

        // Мастер первого запуска
        if self.wizard_open {
            match self.wizard_state.show(&ctx, &mut self.wizard_open) {
                WizardAction::Skip => {
                    self.wizard_open = false;
                }
                WizardAction::Start(sources) => {
                    self.wizard_open = false;
                    self.apply_new_sources(sources);
                }
                WizardAction::None => {}
            }
        }

        // Окно настроек
        if self.show_dialog_settings {
            if self.settings_dialog_state.is_none() {
                self.settings_dialog_state = Some(SettingsDialogState::new(&self.settings));
            }
            if let Some(ref mut st) = self.settings_dialog_state {
                match st.show(&ctx, &mut self.show_dialog_settings) {
                    SettingsDialogAction::Save(save_res) => {
                        self.apply_settings(*save_res);
                        self.settings_dialog_state = None;
                    }
                    SettingsDialogAction::Cancel => {
                        self.settings_dialog_state = None;
                    }
                    SettingsDialogAction::None => {}
                }
            }
        } else {
            self.settings_dialog_state = None;
        }

        // Окно «О программе»
        if self.show_dialog_about {
            show_about_dialog(&ctx, &mut self.show_dialog_about);
        }

        // Окно уведомления о завершении выгрузки отчёта
        if let Some(ref dir) = self.export_notice.clone() {
            let mut open_notice = true;
            egui::Window::new("Выгрузка перечня файлов")
                .open(&mut open_notice)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .title_frame(theme_title_frame())
                .show(&ctx, |ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.colored_label(
                        accent_color(),
                        format!("Готово: перечень сохранён в {}", dir.display()),
                    );
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Открыть папку").clicked() {
                            let _ = show_in_folder(&dir.to_string_lossy());
                            self.export_notice = None;
                        }
                        if ui.button("Закрыть").clicked() {
                            self.export_notice = None;
                        }
                    });
                });
            if !open_notice {
                self.export_notice = None;
            }
        }

        // Предложение обновить поиск после изменения настроек источников или правил
        if self.show_prompt_reindex {
            let mut prompt_open = true;
            egui::Window::new("Обновление поиска")
                .open(&mut prompt_open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .title_frame(theme_title_frame())
                .show(&ctx, |ui| {
                    ui.spacing_mut().item_spacing.y = 10.0;
                    ui.colored_label(
                        warning_color(),
                        "Настройки источников или правил извлечения изменились. Найти изменения и обновить поиск сейчас?",
                    );
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Да, обновить").clicked() {
                            self.show_prompt_reindex = false;
                            self.action_sync_all();
                        }
                        if ui.button("Позже").clicked() {
                            self.show_prompt_reindex = false;
                        }
                    });
                });
            if !prompt_open {
                self.show_prompt_reindex = false;
            }
        }
        if self.show_dialog_reindex_saved {
            let mut perform = false;
            show_confirm_reindex_dialog(&ctx, &mut self.show_dialog_reindex_saved, || {
                perform = true;
            });
            if perform {
                self.action_reindex_saved_texts();
            }
        }
        if self.confirm_remove_path.is_some() {
            let mut to_remove = None;
            show_confirm_remove_record_dialog(
                &ctx,
                &mut self.confirm_remove_path,
                |path_to_remove| {
                    to_remove = Some(path_to_remove);
                },
            );
            if let Some(path) = to_remove {
                self.action_remove_record(path);
            }
        }
        if self.show_dialog_remove_source {
            let sources: Vec<PathBuf> = self
                .settings
                .sources
                .iter()
                .map(|s| s.path.clone())
                .collect();
            let mut chosen = None;
            show_remove_source_dialog(
                &ctx,
                &mut self.show_dialog_remove_source,
                &sources,
                |chosen_path| {
                    chosen = Some(chosen_path);
                },
            );
            if let Some(path) = chosen {
                self.action_remove_source(path);
            }
        }
        if self.show_dialog_reindex_sources {
            let sources: Vec<PathBuf> = self
                .settings
                .sources
                .iter()
                .map(|s| s.path.clone())
                .collect();
            let mut chosen = None;
            show_reindex_sources_dialog(
                &ctx,
                &mut self.show_dialog_reindex_sources,
                &sources,
                &mut self.reindex_selected_sources,
                |chosen_sources| {
                    chosen = Some(chosen_sources);
                },
            );
            if let Some(sources) = chosen {
                self.action_index_sources(sources);
            }
        }

        // Проверка готовности снимка для режима --snimok
        self.check_snimok_capture(&ctx);
        diag_on_ui_end(t_ui_start);
    }
}

/// Запускает фоновый поток Win32 для прослушивания широковещательного сообщения
/// от второго экземпляра программы (`SHOW_MESSAGE_NAME`).
fn start_single_instance_listener(
    should_restore: Arc<AtomicBool>,
    ctx_holder: Arc<OnceLock<egui::Context>>,
) {
    std::thread::spawn(move || {
        use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, RegisterClassExW,
            WNDCLASSEXW, WS_OVERLAPPED,
        };
        use windows::core::PCWSTR;

        let msg_id = crate::single::show_message_id();
        let class_name: Vec<u16> = "PoiskovichekListener\0".encode_utf16().collect();

        unsafe extern "system" fn wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }

        let wnd_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wnd_proc),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };

        unsafe {
            let _ = RegisterClassExW(&wnd_class);
            let hwnd = CreateWindowExW(
                Default::default(),
                PCWSTR(class_name.as_ptr()),
                PCWSTR(class_name.as_ptr()),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            );
            if hwnd.is_err() {
                return;
            }

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                if msg.message == msg_id {
                    should_restore.store(true, Ordering::SeqCst);
                    if let Some(ctx) = ctx_holder.get() {
                        ctx.request_repaint();
                    }
                }
                DispatchMessageW(&msg);
            }
        }
    });
}

/// Загружает значок приложения из встроенного файла `assets/poiskovichek.ico`.
fn load_app_icon() -> Option<egui::IconData> {
    let icon_bytes = include_bytes!("../assets/poiskovichek.ico");
    let img = image::load_from_memory_with_format(icon_bytes, image::ImageFormat::Ico).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

/// Запуск GUI приложения с fallback: glow -> wgpu -> MessageBoxW
pub fn run_gui_app(
    paths: AppPaths,
    settings: Settings,
    engine: Arc<SearchEngine>,
    snimok_options: Option<SnimokOptions>,
    is_background: bool,
) {
    let app_title = "ПоисКовичеК";
    // Установщик ждёт, пока этот мьютекс освободится (обновление «на месте»)
    crate::single::hold_installer_mutex();

    let width = settings.window_width.unwrap_or(1100) as f32;
    let height = settings.window_height.unwrap_or(780) as f32;

    let mut viewport = egui::ViewportBuilder::default()
        .with_title(app_title)
        .with_inner_size([width, height]);

    if let (Some(x), Some(y)) = (settings.window_x, settings.window_y) {
        viewport = viewport.with_position([x as f32, y as f32]);
    }

    if is_background {
        viewport = viewport.with_visible(false);
    }

    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    // Диагностический переключатель вертикальной синхронизации OpenGL:
    // По умолчанию vsync: false (узкая правка устранения рывков при движении мыши в Glow, аналогично AutoNoVsync в WGPU)
    let opengl_vsync = {
        let env_val = std::env::var("POISKOVICHEK_OPENGL_VSYNC").ok();
        let arg_val = std::env::args()
            .position(|a| a == "--opengl-vsync")
            .and_then(|p| std::env::args().nth(p + 1));
        match arg_val.as_deref().or(env_val.as_deref()) {
            Some("on") | Some("true") | Some("1") => true,
            _ => false,
        }
    };

    #[cfg(windows)]
    if diag_is_active() {
        diag_init(opengl_vsync);
        let hooked = unsafe { install_swap_hook() };
        eprintln!("[DIAG] SwapBuffers hook installed: {hooked}, VSync: {opengl_vsync}");
    }

    let mut native_options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: opengl_vsync,
            ..Default::default()
        },
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            surface: eframe::egui_wgpu::SurfaceConfig {
                present_mode: eframe::egui_wgpu::wgpu::PresentMode::AutoNoVsync,
                desired_maximum_frame_latency: Some(1),
            },
            ..Default::default()
        },
        ..Default::default()
    };

    let paths_clone = paths.clone();
    let settings_clone = settings.clone();
    let engine_clone = engine.clone();
    let snimok_clone = snimok_options.clone();

    // 1. Попытка запустить с Glow
    let res_glow = eframe::run_native(
        app_title,
        native_options.clone(),
        Box::new(move |cc| {
            journal::info("ui", "рендерер: glow");
            configure_fonts(&cc.egui_ctx);
            apply_theme(&cc.egui_ctx, settings_clone.theme);
            let app = PoiskovichekApp::new(
                paths_clone,
                settings_clone,
                engine_clone,
                snimok_clone,
                Some(cc.egui_ctx.clone()),
            );
            app.start_update_checker();
            Ok(Box::new(app))
        }),
    );

    if let Err(err_glow) = res_glow {
        journal::error(
            "ui",
            &format!("Не удалось поднять отрисовку glow: {err_glow}. Пробуем wgpu…"),
        );

        // 2. Попытка запустить с WGPU
        native_options.renderer = eframe::Renderer::Wgpu;

        let paths_wgpu = paths.clone();
        let settings_wgpu = settings.clone();
        let engine_wgpu = engine.clone();
        let snimok_wgpu = snimok_options.clone();

        let res_wgpu = eframe::run_native(
            app_title,
            native_options,
            Box::new(move |cc| {
                journal::info("ui", "рендерер: wgpu");
                configure_fonts(&cc.egui_ctx);
                apply_theme(&cc.egui_ctx, settings_wgpu.theme);
                let app = PoiskovichekApp::new(
                    paths_wgpu,
                    settings_wgpu,
                    engine_wgpu,
                    snimok_wgpu,
                    Some(cc.egui_ctx.clone()),
                );
                app.start_update_checker();
                Ok(Box::new(app))
            }),
        );

        if let Err(err_wgpu) = res_wgpu {
            // 3. Запись в журнал и Win32 MessageBoxW
            let err_msg = format!(
                "Не удалось запустить графический интерфейс ПоисКовичеКа.\n\nОшибка glow: {err_glow}\nОшибка wgpu: {err_wgpu}"
            );
            journal::error("ui", &err_msg);

            use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
            use windows::core::HSTRING;
            unsafe {
                let _ = MessageBoxW(
                    None,
                    &HSTRING::from(&err_msg),
                    &HSTRING::from(app_title),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
    }
}

/// Число с разделителем тысяч: 412345 → «412 345».
pub fn format_thousands(n: usize) -> String {
    let digits = n.to_string();
    let len = digits.len();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// Одно число после запятой: 12.34 → «12,3».
pub fn format_decimal(value: f64) -> String {
    format!("{value:.1}").replace('.', ",")
}

/// Процент с запятой: 6484 из 412345 → «1,6 %».
pub fn format_percent(done: usize, total: usize) -> String {
    if total == 0 {
        return "100,0 %".to_string();
    }
    let percent = (done as f64 / total as f64 * 100.0).min(100.0);
    format!("{percent:.1} %").replace('.', ",")
}

/// Обрезает середину длинного пути, оставляя начало и конец.
pub fn shorten_path_middle(path: &str, max_chars: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= max_chars || max_chars < 10 {
        return path.to_string();
    }
    let keep_right = (max_chars - 1) * 2 / 3;
    let keep_left = max_chars - 1 - keep_right;
    let mut out = String::with_capacity(max_chars);
    out.extend(chars.iter().take(keep_left));
    out.push('…');
    out.extend(chars.iter().skip(chars.len() - keep_right));
    out
}

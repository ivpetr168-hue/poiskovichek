use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::tempdir;

use poiskovichek::app::{PoiskovichekApp, SearchResultMsg, filter_sources_for};
use poiskovichek::pipeline::FinishedSummary;
use poiskovichek::search::{SearchEngine, SearchResponse};
use poiskovichek::settings::{Settings, Source, SourceMode};

fn create_test_app(dir: &Path) -> PoiskovichekApp {
    let settings = Settings {
        data_dir: dir.join("data"),
        ..Default::default()
    };
    let mut paths = poiskovichek::paths::resolve(dir, &settings);
    // В обычном режиме resolve возвращает настоящий %APPDATA%\ПоисКовичеК\settings.toml,
    // а приложение сохраняет в него настройки (например, историю запросов).
    // Тест не должен трогать файл пользователя — файл настроек во временной папке.
    paths.settings_file = dir.join("settings.toml");
    paths.ensure_dirs().expect("ensure dirs");

    let _ =
        poiskovichek::catalog::Catalog::open_or_create(&paths.catalog_db).expect("create catalog");
    let _ = poiskovichek::index::open_or_create(&paths.index_dir, None).expect("create index");

    let engine = SearchEngine::open(&paths, &settings, None).expect("open engine");
    PoiskovichekApp::new(paths, settings, Arc::new(engine), None, None)
}

#[test]
fn test_format_status_summary() {
    let summary = PoiskovichekApp::format_status_summary(42, 10, Duration::from_millis(15));
    assert_eq!(summary, "Найдено: 42. Показано: 10. 15 мс");

    let summary2 = PoiskovichekApp::format_status_summary(1, 1, Duration::from_micros(400));
    assert_eq!(summary2, "Найдено: 1. Показано: 1. 0,4 мс");

    let summary3 = PoiskovichekApp::format_status_summary(0, 0, Duration::from_millis(0));
    assert_eq!(summary3, "Найдено: 0. Показано: 0. 0,0 мс");
}

#[test]
fn test_format_finished_status() {
    let summary = FinishedSummary {
        added: 15,
        changed: 4,
        deleted: 2,
    };
    let formatted = PoiskovichekApp::format_finished_status(&summary);
    assert_eq!(
        formatted,
        "Готово: добавлено — 15; изменено — 4; убрано — 2."
    );

    let summary_zero = FinishedSummary {
        added: 0,
        changed: 0,
        deleted: 0,
    };
    assert_eq!(
        PoiskovichekApp::format_finished_status(&summary_zero),
        "Готово: добавлено — 0; изменено — 0; убрано — 0."
    );
}

#[test]
fn test_bottom_status_shows_last_outcome_of_any_operation() {
    use poiskovichek::app::{
        DuplicatesEnd, LastOutcome, StatusKind, bottom_status, duplicates_outcome_text,
        pipeline_outcome_text,
    };

    // Нет итога — «Готов к поиску»
    assert_eq!(
        bottom_status(None, false, false, None),
        ("Готов к поиску".to_string(), StatusKind::Muted)
    );

    let sync = LastOutcome {
        time: "20:58".to_string(),
        text: pipeline_outcome_text(
            "Обновление дисков",
            &FinishedSummary {
                added: 4,
                changed: 1199,
                deleted: 3,
            },
        ),
        is_error: false,
    };
    assert_eq!(
        bottom_status(None, false, false, Some(&sync)),
        (
            "20:58 Обновление дисков: добавлено — 4; изменено — 1199; убрано — 3.".to_string(),
            StatusKind::Info
        )
    );

    // Позже закончился поиск дубликатов: показывается он, а не старый итог индексации
    let dup = LastOutcome {
        time: "22:10".to_string(),
        text: duplicates_outcome_text(
            &DuplicatesEnd::Done {
                groups: 1234,
                reclaimable: 6_012_954_214, // ≈ 5,6 ГБ
            },
            None,
        ),
        is_error: false,
    };
    assert_eq!(
        bottom_status(None, false, false, Some(&dup)).0,
        "22:10 Поиск дубликатов завершён: групп — 1 234, можно освободить 5,6 ГБ."
    );

    // Ошибка — красным, с временем
    let err = LastOutcome {
        time: "22:11".to_string(),
        text: duplicates_outcome_text(&DuplicatesEnd::Failed("диск занят".to_string()), None),
        is_error: true,
    };
    assert_eq!(
        bottom_status(None, false, false, Some(&err)),
        (
            "22:11 Ошибка поиска дубликатов: диск занят".to_string(),
            StatusKind::Error
        )
    );

    // Идущие перенос и выгрузка важнее итога
    assert_eq!(
        bottom_status(None, false, true, Some(&err)).1,
        StatusKind::Info
    );
    assert_eq!(
        bottom_status(Some("Копирую…"), true, false, Some(&err)),
        ("Копирую…".to_string(), StatusKind::Warning)
    );
}

#[test]
fn test_duplicates_stop_is_not_an_error_and_mentions_progress() {
    use poiskovichek::app::{DuplicatesEnd, duplicates_outcome_text, duplicates_progress_text};
    use poiskovichek::duplicates::{DuplicateProgress, DuplicateStage};

    let quick = DuplicateProgress {
        stage: DuplicateStage::QuickHash,
        files_done: 12345,
        files_total: 106762,
        bytes_done: 0,
        bytes_total: 0,
    };
    assert_eq!(
        duplicates_outcome_text(&DuplicatesEnd::Stopped, Some(&quick)),
        "Поиск дубликатов остановлен: посчитано 12 345 из 106 762 файлов, \
         при следующем запуске продолжится с этого места."
    );
    assert_eq!(
        duplicates_progress_text(&quick),
        "Дубликаты: быстрые отпечатки — 12 345 из 106 762"
    );

    let full = DuplicateProgress {
        stage: DuplicateStage::FullHash,
        files_done: 3,
        files_total: 40,
        bytes_done: 1_572_864,
        bytes_total: 41_943_040,
    };
    assert_eq!(
        duplicates_progress_text(&full),
        "Дубликаты: полные отпечатки — 3 из 40 (1,5 МБ из 40,0 МБ)"
    );
    assert!(
        duplicates_outcome_text(&DuplicatesEnd::Stopped, Some(&full))
            .contains("полные — 3 из 40 файлов")
    );
    assert_eq!(
        duplicates_outcome_text(
            &DuplicatesEnd::Done {
                groups: 0,
                reclaimable: 0
            },
            None
        ),
        "Поиск дубликатов завершён: дубликатов не найдено."
    );
}

#[test]
fn test_record_outcome_replaces_previous_and_stamps_time() {
    let dir = tempdir().expect("tempdir");
    let mut app = create_test_app(dir.path());

    app.record_outcome(
        "Обновление дисков: добавлено — 1; изменено — 0; убрано — 0.".into(),
        false,
    );
    app.record_outcome("Поиск дубликатов остановлен.".into(), false);
    let (text, kind) = app.status_line();
    assert!(text.ends_with(" Поиск дубликатов остановлен."), "{text}");
    assert_eq!(kind, poiskovichek::app::StatusKind::Info);
    // Время: ЧЧ:ММ
    let time = &text[..5];
    assert_eq!(time.as_bytes()[2], b':', "{text}");
    assert!(
        time.chars().filter(|c| c.is_ascii_digit()).count() == 4,
        "{text}"
    );
}

#[test]
fn test_query_generations_and_drop_outdated() {
    let dir = tempdir().expect("tempdir");
    let mut app = create_test_app(dir.path());

    assert_eq!(app.query_generation, 0);
    assert!(app.search_response.is_none());

    // Устанавливаем текущее поколение запроса = 3
    app.query_generation = 3;
    app.is_searching = true;

    // Устаревший ответ поколения 2 должен быть отброшен (R2-А-38)
    let outdated_msg = SearchResultMsg {
        generation: 2,
        response: SearchResponse {
            items: Vec::new(),
            total: 100,
            elapsed: Duration::from_millis(50),
            warnings: Vec::new(),
        },
    };
    app.handle_search_result(outdated_msg);
    assert!(app.search_response.is_none());
    assert!(app.is_searching);

    // Актуальный ответ поколения 3 должен быть принят
    let valid_msg = SearchResultMsg {
        generation: 3,
        response: SearchResponse {
            items: Vec::new(),
            total: 42,
            elapsed: Duration::from_millis(12),
            warnings: Vec::new(),
        },
    };
    app.handle_search_result(valid_msg);
    assert!(!app.is_searching);
    assert!(app.search_response.is_some());
    assert_eq!(app.search_response.as_ref().unwrap().total, 42);
    assert_eq!(app.search_elapsed, Some(Duration::from_millis(12)));

    // Новый устаревший ответ поколения 1 снова отбрасывается
    let old_msg = SearchResultMsg {
        generation: 1,
        response: SearchResponse {
            items: Vec::new(),
            total: 999,
            elapsed: Duration::from_millis(5),
            warnings: Vec::new(),
        },
    };
    app.handle_search_result(old_msg);
    assert_eq!(app.search_response.as_ref().unwrap().total, 42);
}

#[test]
fn test_indexing_action_lock() {
    let dir = tempdir().expect("tempdir");
    let mut app = create_test_app(dir.path());

    // Когда индексация не идет, действия доступны
    app.is_indexing = false;
    assert!(app.can_perform_indexing_action());

    // Когда индексация идет, действия, изменяющие базу, недоступны
    app.is_indexing = true;
    assert!(!app.can_perform_indexing_action());
}

#[test]
fn test_query_history_navigation() {
    let dir = tempdir().expect("tempdir");
    let mut app = create_test_app(dir.path());

    // Добавляем запросы
    app.add_to_history("первый");
    app.add_to_history("второй");
    app.add_to_history("третий");

    assert_eq!(
        app.settings.query_history,
        vec!["третий", "второй", "первый"]
    );

    // Навигация вверх
    app.query_text = "черновик".to_string();
    app.history_up();
    assert_eq!(app.query_text, "третий");

    app.history_up();
    assert_eq!(app.query_text, "второй");

    app.history_up();
    assert_eq!(app.query_text, "первый");

    // Навигация вниз возвращает к черновику
    app.history_down();
    assert_eq!(app.query_text, "второй");

    app.history_down();
    assert_eq!(app.query_text, "третий");

    app.history_down();
    assert_eq!(app.query_text, "черновик");

    // Проверка лимита в 50 запросов
    for i in 0..100 {
        app.add_to_history(&format!("запрос_{i}"));
    }
    assert_eq!(app.settings.query_history.len(), 50);
}

#[test]
fn test_no_external_process_invocation_in_src() {
    fn scan_dir(dir: &Path) {
        for entry in fs::read_dir(dir).expect("read dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                scan_dir(&path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
                let content = fs::read_to_string(&path).expect("read file");
                let forbidden = [
                    "powershell",
                    "cmd",
                    "explorer",
                    "powershell.exe",
                    "cmd.exe",
                    "explorer.exe",
                ];
                for bad in forbidden {
                    let needle1 = format!("Command::new(\"{bad}\")");
                    let needle2 = format!("Command::new(\"{bad}.exe\")");
                    assert!(
                        !content.contains(&needle1) && !content.contains(&needle2),
                        "Файл {} содержит недопустимый запуск внешнего процесса: {}",
                        path.display(),
                        bad
                    );
                }
            }
        }
    }

    scan_dir(Path::new("src"));
}

fn src(path: &str, mode: SourceMode) -> Source {
    Source {
        path: PathBuf::from(path),
        mode,
        max_depth: 3,
        enabled: true,
        volume_serial: None,
        extra_excludes: vec!["tmp".to_string()],
    }
}

#[test]
fn test_filter_sources_use_real_db_ids_and_skip_unknown() {
    // №221: id берутся из таблицы sources по пути, а не по номеру в настройках
    let mut disabled = src("E:/off", SourceMode::Full);
    disabled.enabled = false;
    let settings_sources = vec![
        src("C:/a", SourceMode::Full),
        src("D:/b", SourceMode::Full),
        src("F:/new", SourceMode::Full),
        disabled,
    ];
    let db = vec![
        (7, "D:/b".to_string()),
        (9, "C:/a".to_string()),
        (11, "E:/off".to_string()),
        (12, "Z:/gone".to_string()),
    ];
    let list = filter_sources_for(&settings_sources, &db);
    assert_eq!(list, vec![(9, "C:/a".to_string()), (7, "D:/b".to_string())]);
}

#[test]
fn test_read_db_sources_and_app_filter_list() {
    let dir = tempdir().expect("tempdir");
    let src_dir = dir.path().join("papka");
    fs::create_dir_all(&src_dir).unwrap();
    let settings = Settings {
        data_dir: dir.path().join("data"),
        sources: vec![src(&src_dir.to_string_lossy(), SourceMode::Full)],
        ..Default::default()
    };
    let mut paths = poiskovichek::paths::resolve(dir.path(), &settings);
    paths.settings_file = dir.path().join("settings.toml");
    paths.ensure_dirs().unwrap();
    let mut catalog = poiskovichek::catalog::Catalog::open_or_create(&paths.catalog_db).unwrap();
    // Вторая запись, чтобы id в базе не совпадал с номером в настройках
    poiskovichek::sync::register_source(
        &mut catalog,
        &src("X:/other", SourceMode::Full),
        None,
        None,
        "ok",
    )
    .unwrap();
    let (real_id, _) =
        poiskovichek::sync::register_source(&mut catalog, &settings.sources[0], None, None, "ok")
            .unwrap();
    drop(catalog);
    let _ = poiskovichek::index::open_or_create(&paths.index_dir, None).unwrap();
    let engine = SearchEngine::open(&paths, &settings, None).unwrap();
    let app = PoiskovichekApp::new(paths, settings, Arc::new(engine), None, None);
    assert_eq!(real_id, 2);
    assert_eq!(
        app.filter_sources,
        vec![(real_id, src_dir.to_string_lossy().to_string())]
    );
}

#[test]
fn test_reindex_sources_keep_real_source_settings() {
    // №225: настройки диска не теряются при «проиндексировать заново»
    let dir = tempdir().expect("tempdir");
    let mut app = create_test_app(dir.path());
    app.settings.sources = vec![src("C:/a", SourceMode::NamesOnly)];
    let got = app.sources_for_paths(vec![PathBuf::from("C:/a"), PathBuf::from("D:/unknown")]);
    assert_eq!(got[0].mode, SourceMode::NamesOnly);
    assert_eq!(got[0].max_depth, 3);
    assert_eq!(got[0].extra_excludes, vec!["tmp".to_string()]);
    assert_eq!(got[1].mode, SourceMode::Full);
}

#[test]
fn test_background_reload_updates_app_and_repeats_search() {
    // №224: после reload в фоне окно повторяет поиск и обновляет источники
    let dir = tempdir().expect("tempdir");
    let mut app = create_test_app(dir.path());
    let ctx = egui::Context::default();
    app.start_engine_reload();
    assert!(app.engine_reload_rx.is_some());
    app.start_engine_reload(); // повторная просьба запоминается
    assert!(app.engine_reload_pending);
    let gen_before = app.query_generation;
    let start = std::time::Instant::now();
    while app.engine_reload_rx.is_some() || app.engine_reload_pending {
        app.poll_incoming(&ctx);
        assert!(start.elapsed() < Duration::from_secs(20), "reload завис");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.query_generation > gen_before, "поиск не повторён");
}

// ---------------------------------------------------------------------------
// Задача №239: переключатели режимов поиска в главном окне
// ---------------------------------------------------------------------------
#[test]
fn test_window_switches_change_modes_save_settings_and_repeat_search() {
    use poiskovichek::settings::{MultiWordMode, WordFormsMode};

    let dir = tempdir().expect("tempdir");
    let mut app = create_test_app(dir.path());
    let settings_file = dir.path().join("settings.toml");
    assert_eq!(app.word_forms_mode, WordFormsMode::AllForms);
    assert_eq!(app.multi_word_mode, MultiWordMode::StrictPhrase);

    // Пустой запрос: режим меняется и сохраняется, поиск не запускается
    let gen0 = app.query_generation;
    app.set_word_forms_mode(WordFormsMode::Exact);
    assert_eq!(app.word_forms_mode, WordFormsMode::Exact);
    assert_eq!(app.settings.word_forms_mode, WordFormsMode::Exact);
    assert_eq!(app.query_generation, gen0);
    let saved = Settings::load(&settings_file).expect("настройки сохранены во временную папку");
    assert_eq!(saved.word_forms_mode, WordFormsMode::Exact);
    assert_eq!(saved.multi_word_mode, MultiWordMode::StrictPhrase);

    // С непустым запросом смена любого переключателя сразу повторяет поиск
    app.query_text = "красный дом".to_string();
    let gen1 = app.query_generation;
    app.set_multi_word_mode(MultiWordMode::AllWords);
    assert_eq!(app.multi_word_mode, MultiWordMode::AllWords);
    assert_eq!(app.settings.multi_word_mode, MultiWordMode::AllWords);
    assert!(app.query_generation > gen1, "поиск не повторён");
    let saved = Settings::load(&settings_file).unwrap();
    assert_eq!(saved.multi_word_mode, MultiWordMode::AllWords);
    assert_eq!(saved.word_forms_mode, WordFormsMode::Exact);

    let gen2 = app.query_generation;
    app.set_word_forms_mode(WordFormsMode::AllForms);
    assert!(app.query_generation > gen2, "поиск не повторён");
    assert_eq!(
        app.engine.settings().word_forms_mode,
        WordFormsMode::AllForms
    );

    // Тот же выбор повторно — ничего не происходит
    let gen3 = app.query_generation;
    app.set_word_forms_mode(WordFormsMode::AllForms);
    app.set_multi_word_mode(MultiWordMode::AllWords);
    assert_eq!(app.query_generation, gen3);
}

#[test]
fn test_window_word_forms_switch_changes_search_results() {
    use poiskovichek::pipeline::{PipelineOptions, sync_all_with_options};
    use poiskovichek::settings::WordFormsMode;

    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let source_dir = root.join("source");
    fs::create_dir_all(&source_dir).unwrap();
    fs::write(source_dir.join("a.txt"), "книга").unwrap();
    fs::write(source_dir.join("b.txt"), "книгами").unwrap();

    let data_dir = root.join("data");
    let settings = Settings {
        data_dir: data_dir.clone(),
        enable_morphology: false,
        sources: vec![src(&source_dir.to_string_lossy(), SourceMode::Full)],
        ..Default::default()
    };
    let paths = poiskovichek::paths::AppPaths {
        settings_file: root.join("settings.toml"),
        catalog_db: data_dir.join("catalog.sqlite3"),
        index_dir: data_dir.join("index"),
        logs_dir: root.join("logs"),
        data_dir,
    };
    fs::create_dir_all(&paths.logs_dir).unwrap();
    sync_all_with_options(
        &settings,
        PipelineOptions {
            queue_capacity: 8,
            in_process_workers: true,
            ..Default::default()
        },
    )
    .expect("sync_all");
    let engine = SearchEngine::open(&paths, &settings, None).expect("open engine");
    let mut app = PoiskovichekApp::new(paths, settings, Arc::new(engine), None, None);
    let ctx = egui::Context::default();

    let wait_total = |app: &mut PoiskovichekApp| -> usize {
        let start = std::time::Instant::now();
        loop {
            app.poll_incoming(&ctx);
            if !app.is_searching
                && let Some(resp) = &app.search_response
            {
                return resp.total;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "поиск завис");
            std::thread::sleep(Duration::from_millis(10));
        }
    };

    app.query_text = "книга".to_string();
    app.trigger_search();
    assert_eq!(
        wait_total(&mut app),
        2,
        "все формы без словаря: слова с таким началом"
    );

    app.set_word_forms_mode(WordFormsMode::Exact);
    assert_eq!(wait_total(&mut app), 1, "точно как набрано: только «книга»");

    app.set_word_forms_mode(WordFormsMode::AllForms);
    assert_eq!(wait_total(&mut app), 2);
}

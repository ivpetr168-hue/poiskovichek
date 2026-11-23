#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;

use poiskovichek::app::{SnimokOptions, run_gui_app};
use poiskovichek::journal;
use poiskovichek::paths;
use poiskovichek::pipeline::PipelineOptions;
use poiskovichek::search::{SearchEngine, SearchRequest};
use poiskovichek::settings::Settings;

fn get_exe_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn show_error_dialog(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::HSTRING;

    let title = HSTRING::from("ПоисКовичеК");
    let text = HSTRING::from(message);
    unsafe {
        let _ = MessageBoxW(None, &text, &title, MB_OK | MB_ICONERROR);
    }
}

fn run_self_test(docs_dir: &Path, exe_dir: &Path) -> i32 {
    #[cfg(windows)]
    unsafe {
        let _ = windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }

    let log_path = docs_dir.join("self-test.log");
    let mut log_lines = Vec::new();
    let mut log = |msg: String| {
        println!("{msg}");
        log_lines.push(msg);
    };

    log("=== Самопроверка ПоисКовичеКа ===".to_string());
    log(format!("Папка документов: {}", docs_dir.display()));

    let is_portable = paths::is_portable_mode(exe_dir);
    let settings_file = paths::default_settings_path(exe_dir);
    // Файл настроек пользователя только читается; самопроверка работает на копии в памяти
    // и никогда его не сохраняет (№228).
    let user_settings = Settings::load(&settings_file).unwrap_or_default();

    let temp_data_dir = if is_portable {
        None
    } else {
        let t = exe_dir.join(format!(".tmp-self-test-{}", process::id()));
        let _ = fs::remove_dir_all(&t);
        Some(t)
    };

    let settings = user_settings.for_self_test(docs_dir, temp_data_dir.as_deref());

    let app_paths = paths::resolve(exe_dir, &settings);
    if let Err(e) = app_paths.ensure_dirs() {
        log_lines.push(format!("ОШИБКА: Не удалось создать папку данных: {e}"));
        let log_str = log_lines.join("\r\n");
        let _ = fs::write(&log_path, &log_str);
        let _ = fs::write(exe_dir.join("self-test.log"), &log_str);
        if let Some(ref t) = temp_data_dir {
            let _ = fs::remove_dir_all(t);
        }
        return 1;
    }

    // Загрузка морфологического словаря morph.fst
    let morph_path = exe_dir.join("morph.fst");
    let morph = if morph_path.is_file() {
        poiskovichek::morph::Morph::open(&morph_path).ok()
    } else {
        let assets_morph = PathBuf::from("assets").join("morph.fst");
        if assets_morph.is_file() {
            poiskovichek::morph::Morph::open(&assets_morph).ok()
        } else {
            None
        }
    };

    if morph.is_some() {
        log("Морфологический словарь morph.fst успешно загружен".to_string());
    } else {
        log(format!(
            "ВНИМАНИЕ: Словарь morph.fst не найден по пути {}",
            morph_path.display()
        ));
    }

    // 1. Индексация папки документов
    log("Индексирование источников…".to_string());
    let pipeline_opts = PipelineOptions {
        queue_capacity: 8,
        in_process_workers: true,
        morph: morph.clone(),
        ..Default::default()
    };

    match poiskovichek::pipeline::index_sources_with_options(
        &settings,
        &settings.sources,
        pipeline_opts,
    ) {
        Ok(summary) => {
            log(format!(
                "Индексация завершена: добавлено={}, изменено={}, удалено={}",
                summary.added, summary.changed, summary.deleted
            ));
        }
        Err(e) => {
            log(format!("ОШИБКА индексации: {e}"));
            let log_str = log_lines.join("\r\n");
            let _ = fs::write(&log_path, &log_str);
            let _ = fs::write(exe_dir.join("self-test.log"), &log_str);
            if let Some(ref t) = temp_data_dir {
                let _ = fs::remove_dir_all(t);
            }
            return 1;
        }
    }

    // 2. Открытие поискового движка
    let engine = match SearchEngine::open(&app_paths, &settings, morph) {
        Ok(eng) => eng,
        Err(e) => {
            log(format!("ОШИБКА открытия поискового движка: {e}"));
            let log_str = log_lines.join("\r\n");
            let _ = fs::write(&log_path, &log_str);
            let _ = fs::write(exe_dir.join("self-test.log"), &log_str);
            if let Some(ref t) = temp_data_dir {
                let _ = fs::remove_dir_all(t);
            }
            return 1;
        }
    };

    // 3. Чтение файла self-test.txt
    let self_test_file = docs_dir.join("self-test.txt");
    if !self_test_file.is_file() {
        log(format!(
            "ОШИБКА: Файл self-test.txt не найден по пути {}",
            self_test_file.display()
        ));
        let log_str = log_lines.join("\r\n");
        let _ = fs::write(&log_path, &log_str);
        let _ = fs::write(exe_dir.join("self-test.log"), &log_str);
        if let Some(ref t) = temp_data_dir {
            let _ = fs::remove_dir_all(t);
        }
        return 1;
    }

    let content = match fs::read_to_string(&self_test_file) {
        Ok(c) => c,
        Err(e) => {
            log(format!("ОШИБКА чтения self-test.txt: {e}"));
            let log_str = log_lines.join("\r\n");
            let _ = fs::write(&log_path, &log_str);
            let _ = fs::write(exe_dir.join("self-test.log"), &log_str);
            if let Some(ref t) = temp_data_dir {
                let _ = fs::remove_dir_all(t);
            }
            return 1;
        }
    };

    let mut all_ok = true;
    let mut test_count = 0;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        test_count += 1;
        let parts: Vec<&str> = trimmed.split('\t').collect();
        if parts.len() < 3 {
            log(format!(
                "Строка {test_count}: неверный формат (ожидалось 3 поля через TAB): {trimmed}"
            ));
            all_ok = false;
            continue;
        }

        let query = parts[0].trim();
        let expected_count: usize = match parts[1].trim().parse() {
            Ok(c) => c,
            Err(_) => {
                log(format!(
                    "Строка {test_count}: неверное ожидаемое число групп: {}",
                    parts[1]
                ));
                all_ok = false;
                continue;
            }
        };
        let required_path = parts[2].trim();

        let req = SearchRequest::new(query).with_limit(200);
        let resp = engine.search(req.clone());

        let path_matched = resp
            .items
            .iter()
            .any(|item| item.paths.iter().any(|p| p.contains(required_path)));

        if resp.total == expected_count && path_matched {
            // Проверяем вычисление фрагмента
            if let Some(item) = resp.items.first() {
                let _ = engine.snippet_for(&item.item_key, &req);
            }
            log(format!(
                "[OK] Запрос «{query}»: найдено {count} (ожидалось {expected_count}), путь содержит «{required_path}»",
                count = resp.total
            ));
        } else {
            all_ok = false;
            log(format!(
                "[ПРОВАЛ] Запрос «{query}»: найдено {count} (ожидалось {expected_count}), путь содержит «{required_path}»: {path_matched}",
                count = resp.total
            ));
        }
    }

    if all_ok && test_count > 0 {
        log(format!("ИТОГ: ВСЕ ТЕСТЫ ПРОЙДЕНЫ ({test_count} проверок)"));
    } else {
        log("ИТОГ: ОБНАРУЖЕНЫ ОШИБКИ".to_string());
    }

    let log_str = log_lines.join("\r\n");
    let _ = fs::write(&log_path, &log_str);
    let _ = fs::write(exe_dir.join("self-test.log"), &log_str);
    if let Some(ref t) = temp_data_dir {
        let _ = fs::remove_dir_all(t);
    }

    if all_ok && test_count > 0 { 0 } else { 1 }
}

fn main() {
    poiskovichek::app::record_process_start(std::time::Instant::now());
    let args: Vec<String> = env::args().collect();

    // Режим рабочего процесса извлечения текста (раздел 2.6 ТЗ)
    if args.iter().any(|arg| arg == "--worker") {
        poiskovichek::run_worker_loop();
        process::exit(0);
    }

    // Обработка ключей командной строки
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("Poiskovichek {}", env!("CARGO_PKG_VERSION"));
        process::exit(0);
    }

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        println!("ПоисКовичеК — быстрый поиск документов на дисках");
        println!("Использование: Poiskovichek [КЛЮЧИ]");
        println!();
        println!("Ключи:");
        println!("  --worker                      Запуск в режиме рабочего процесса извлечения");
        println!(
            "  --self-test <папка>           Автоматическая самопроверка поиска на документах"
        );
        println!("  --snimok <путь>               Создать снимок экрана окна программы");
        println!("  --zapros <текст>              Поисковый запрос для режима снимка");
        println!("  --okno <поиск|o-programme|..> Целевое окно для режима снимка");
        println!("  --background                  Запуск в фоновом режиме (окно свёрнуто)");
        println!("  --version, -V                 Показать версию программы и выйти");
        println!("  --help, -h                    Показать это справочное сообщение и выйти");
        process::exit(0);
    }

    let exe_dir = get_exe_dir();

    // Режим автоматической самопроверки --self-test <папка>
    if let Some(pos) = args.iter().position(|arg| arg == "--self-test") {
        if let Some(folder_str) = args.get(pos + 1) {
            let docs_dir = PathBuf::from(folder_str);
            let code = run_self_test(&docs_dir, &exe_dir);
            process::exit(code);
        } else {
            eprintln!("Ошибка: ключ --self-test требует путь к папке документов");
            process::exit(1);
        }
    }

    let is_background = args.iter().any(|arg| arg == "--background");

    // Параметры для режима снимка окна --snimok <файл.png>
    let snimok_options = if let Some(pos) = args.iter().position(|arg| arg == "--snimok") {
        if let Some(file_str) = args.get(pos + 1) {
            let zapros = if let Some(p) = args.iter().position(|arg| arg == "--zapros") {
                let words: Vec<String> = args[p + 1..]
                    .iter()
                    .take_while(|arg| !arg.starts_with("--"))
                    .cloned()
                    .collect();
                if words.is_empty() {
                    None
                } else {
                    Some(words.join(" "))
                }
            } else {
                None
            };
            let okno = args
                .iter()
                .position(|arg| arg == "--okno")
                .and_then(|p| args.get(p + 1))
                .cloned();

            Some(SnimokOptions {
                output_path: PathBuf::from(file_str),
                zapros,
                okno,
            })
        } else {
            None
        }
    } else {
        None
    };

    let settings_file = paths::default_settings_path(&exe_dir);

    // Загрузка настроек
    let settings = match Settings::load(&settings_file) {
        Ok(s) => s,
        Err(err) => {
            let fallback_paths = paths::resolve(&exe_dir, &Settings::default());
            let _ = fallback_paths.ensure_dirs();
            let _ = journal::init(&fallback_paths.logs_dir, 14);

            let err_msg = format!("Ошибка загрузки настроек:\n{err}");
            journal::error("main", &err_msg);
            show_error_dialog(&err_msg);
            process::exit(2);
        }
    };

    // Разрешение путей и подготовка папок
    let app_paths = paths::resolve(&exe_dir, &settings);
    if let Err(e) = app_paths.ensure_dirs() {
        show_error_dialog(&format!("Не удалось создать каталоги данных:\n{e}"));
        process::exit(1);
    }

    // Инициализация журнала
    if let Err(e) = journal::init(&app_paths.logs_dir, settings.log_keep_days) {
        show_error_dialog(&format!("Не удалось инициализировать журнал:\n{e}"));
        process::exit(1);
    }

    journal::info("main", "запуск");

    // Контроль единственной копии программы через single::acquire
    let _single_instance_guard = match poiskovichek::single::acquire(&app_paths.data_dir) {
        Ok(guard) => guard,
        Err(poiskovichek::single::AlreadyRunning) => {
            // Второй запуск поднимает окно первой копии и выходит
            journal::info(
                "main",
                "программа уже запущена, отправляем сигнал показать окно",
            );
            poiskovichek::single::notify_existing();
            process::exit(0);
        }
    };

    // Инициализация базы каталога, если она еще не создана
    if !app_paths.catalog_db.is_file()
        && let Err(e) = poiskovichek::catalog::Catalog::open_or_create(&app_paths.catalog_db)
    {
        show_error_dialog(&format!("Не удалось создать базу каталога:\n{e}"));
        process::exit(1);
    }

    // Загрузка морфологического словаря
    let morph_path = paths::morph_path(&exe_dir);
    let morph = if morph_path.is_file() {
        poiskovichek::morph::Morph::open(&morph_path).ok()
    } else {
        let assets_morph = PathBuf::from("assets").join("morph.fst");
        if assets_morph.is_file() {
            poiskovichek::morph::Morph::open(&assets_morph).ok()
        } else {
            None
        }
    };

    if morph.is_none() {
        journal::warn(
            "main",
            &format!(
                "Словарь словоформ не найден: {}. Поиск работает без словоформ",
                morph_path.display()
            ),
        );
    }

    // Открытие поискового движка
    let engine = match SearchEngine::open(&app_paths, &settings, morph) {
        Ok(eng) => eng,
        Err(e) => {
            show_error_dialog(&format!("Не удалось открыть поисковый движок:\n{e}"));
            process::exit(1);
        }
    };

    // Запуск GUI окна программы
    run_gui_app(
        app_paths,
        settings,
        Arc::new(engine),
        snimok_options,
        is_background,
    );

    journal::info("main", "завершение работы");
    process::exit(0);
}

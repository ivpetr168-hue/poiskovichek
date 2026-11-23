#![allow(clippy::field_reassign_with_default)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

use poiskovichek::autostart::{
    AUTOSTART_VALUE_NAME, delete_key, is_autostart_enabled_custom, set_autostart_custom,
};
use poiskovichek::catalog::Catalog;
use poiskovichek::extract::{ExtractConfig, extract_bytes};
use poiskovichek::journal;
use poiskovichek::morph::{Morph, build_morph_fst};
use poiskovichek::pipeline::{PipelineOptions, sync_all_with_options};
use poiskovichek::relocate::{RelocateMode, relocate_data};
use poiskovichek::rules::{category_for, is_sensitive_path};
use poiskovichek::search::{SearchEngine, SearchRequest};
use poiskovichek::settings::{
    MultiWordMode, PdfExtractorMode, Settings, SortMode, Source, SourceMode,
};
use poiskovichek::walk::walk_source;
use rusqlite::Connection;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;
use zip::write::SimpleFileOptions;

/// Создаёт тестовый морфологический словарь для проверки поиска со словоформами
fn setup_test_morph(dir: &Path) -> Morph {
    let morph_db = dir.join("morph.sqlite3");
    let fst_path = dir.join("morph.fst");
    let conn = Connection::open(&morph_db).expect("open morph_db");
    conn.execute_batch(
        "CREATE TABLE word_primary (word TEXT NOT NULL, lexeme_id INTEGER NOT NULL);
         INSERT INTO word_primary VALUES
             ('спешившую', 10), ('спешившая', 10),
             ('сельская', 20), ('сельские', 20),
             ('мужики', 30), ('мужик', 30);",
    )
    .expect("insert morph");
    build_morph_fst(&morph_db, &fst_path).expect("build fst");
    Morph::open(&fst_path).expect("open morph")
}

// =========================================================================
// 1. Вкладка «Источники»: mode, max_depth, enabled
// =========================================================================

#[test]
fn test_setting_source_mode_names_only() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let test_file = src_dir.join("document_alpha.txt");
    fs::write(&test_file, "секретноесодержимое внутри файла").unwrap();

    let data_dir = root.join("data");

    // 1. Режим NamesOnly: текст не должен быть проиндексирован
    let mut settings = Settings {
        data_dir: data_dir.clone(),
        sources: vec![Source {
            path: src_dir.clone(),
            mode: SourceMode::NamesOnly,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        ..Default::default()
    };

    let paths = poiskovichek::paths::resolve(root, &settings);
    paths.ensure_dirs().unwrap();
    sync_all_with_options(&settings, PipelineOptions::default()).unwrap();

    let morph = setup_test_morph(root);
    let engine = SearchEngine::open(&paths, &settings, Some(morph)).unwrap();

    // Поиск по слову из текста ничего не находит
    let resp_text = engine.search(SearchRequest::new("секретноесодержимое"));
    assert_eq!(
        resp_text.total, 0,
        "В режиме NamesOnly содержимое файла не должно попадать в поиск"
    );

    // Но поиск по имени файла находит
    let resp_name = engine.search(SearchRequest::new("document_alpha"));
    assert_eq!(
        resp_name.total, 1,
        "В режиме NamesOnly имя файла должно находиться"
    );

    // 2. Меняем режим на Full: содержимое становится доступным
    drop(engine);
    settings.sources[0].mode = SourceMode::Full;
    let _ = fs::remove_dir_all(&data_dir);
    paths.ensure_dirs().unwrap();
    sync_all_with_options(&settings, PipelineOptions::default()).unwrap();

    let engine2 = SearchEngine::open(&paths, &settings, None).unwrap();
    let resp_text_full = engine2.search(SearchRequest::new("секретноесодержимое"));
    assert_eq!(
        resp_text_full.total, 1,
        "В режиме Full содержимое файла должно находиться"
    );
}

#[test]
fn test_setting_source_mode_documents_only() {
    let dir = tempdir().expect("tempdir");
    let src_dir = dir.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("code.rs"), "fn main() {}").unwrap();
    fs::write(src_dir.join("notes.txt"), "простые заметки").unwrap();

    // В режиме DocumentsOnly обход пропускает файлы кода (.rs)
    let settings = Settings {
        sources: vec![Source {
            path: src_dir.clone(),
            mode: SourceMode::DocumentsOnly,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        ..Default::default()
    };

    let walk_res = walk_source(&settings.sources[0], &settings, None);
    let paths: Vec<String> = walk_res.entries().iter().map(|e| e.path.clone()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("notes.txt")),
        "notes.txt должен быть включен"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("code.rs")),
        "code.rs должен быть отфильтрован в режиме DocumentsOnly"
    );
}

#[test]
fn test_setting_source_max_depth() {
    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("subfolder");
    fs::create_dir_all(&sub).unwrap();
    fs::write(dir.path().join("root_file.txt"), "root").unwrap();
    fs::write(sub.join("deep_file.txt"), "deep").unwrap();

    // При max_depth = 1 файлы в корне обходятся, вложенные папки не обходятся
    let mut settings = Settings {
        sources: vec![Source {
            path: dir.path().to_path_buf(),
            mode: SourceMode::Full,
            max_depth: 1,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        ..Default::default()
    };

    let walk_limited = walk_source(&settings.sources[0], &settings, None);
    let paths_limited: Vec<String> = walk_limited
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    assert!(
        paths_limited.iter().any(|p| p.ends_with("root_file.txt")),
        "root_file.txt в корне должен присутствовать при max_depth = 1"
    );
    assert!(
        !paths_limited.iter().any(|p| p.ends_with("deep_file.txt")),
        "deep_file.txt во вложенной папке должен быть пропущен при max_depth = 1"
    );

    // При max_depth = 0 (без предела) deep_file.txt найден
    settings.sources[0].max_depth = 0;
    let walk_unlimited = walk_source(&settings.sources[0], &settings, None);
    let paths_unlimited: Vec<String> = walk_unlimited
        .entries()
        .iter()
        .map(|e| e.path.clone())
        .collect();
    assert!(
        paths_unlimited.iter().any(|p| p.ends_with("deep_file.txt")),
        "deep_file.txt должен быть включен при max_depth = 0"
    );
}

#[test]
fn test_setting_source_enabled() {
    let dir = tempdir().expect("tempdir");
    let src_dir = dir.path().join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("active.txt"), "содержимое активного источника").unwrap();

    let data_dir = dir.path().join("data");
    let mut settings = Settings {
        data_dir,
        sources: vec![Source {
            path: src_dir.clone(),
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: false, // выключен
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        ..Default::default()
    };

    let paths = poiskovichek::paths::resolve(dir.path(), &settings);
    paths.ensure_dirs().unwrap();

    // Сверка выключенного источника ничего не добавляет
    let summary = sync_all_with_options(&settings, PipelineOptions::default()).unwrap();
    assert_eq!(
        summary.added, 0,
        "Выключенный источник не должен индексироваться"
    );

    // Включаем источник
    settings.sources[0].enabled = true;
    let summary_enabled = sync_all_with_options(&settings, PipelineOptions::default()).unwrap();
    assert_eq!(
        summary_enabled.added, 1,
        "Включенный источник должен быть проиндексирован"
    );
}

// =========================================================================
// 2. Вкладка «Исключения»: exclude_folder_names, documents_only_folder_names, exclude_paths, exclude_files
// =========================================================================

#[test]
fn test_setting_exclude_folder_names() {
    let dir = tempdir().expect("tempdir");
    let node_modules = dir.path().join("node_modules");
    fs::create_dir_all(&node_modules).unwrap();
    fs::write(node_modules.join("package.json"), "{}").unwrap();

    // По умолчанию node_modules входит в список исключений
    let mut settings = Settings::default();
    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let res = walk_source(&source, &settings, None);
    assert!(
        res.entries().is_empty(),
        "Папка node_modules должна быть исключена по умолчанию"
    );

    // Убираем node_modules из исключений
    settings
        .exclude_folder_names
        .retain(|x| x != "node_modules");
    let res2 = walk_source(&source, &settings, None);
    assert_eq!(
        res2.entries().len(),
        1,
        "После удаления из исключений папка node_modules должна обходиться"
    );
}

#[test]
fn test_setting_documents_only_folder_names() {
    let dir = tempdir().expect("tempdir");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(cache_dir.join("script.py"), "print(1)").unwrap();
    fs::write(cache_dir.join("doc.docx"), "doc").unwrap();

    let mut settings = Settings::default();
    // cache по умолчанию входит в documents_only_folder_names
    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let res = walk_source(&source, &settings, None);
    let paths: Vec<String> = res.entries().iter().map(|e| e.path.clone()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("doc.docx")),
        "В папке cache документ docx должен быть включен"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("script.py")),
        "В папке cache скрипт py должен быть пропущен"
    );

    // Удаляем cache из documents_only_folder_names
    settings.documents_only_folder_names.clear();
    let res2 = walk_source(&source, &settings, None);
    let paths2: Vec<String> = res2.entries().iter().map(|e| e.path.clone()).collect();
    assert!(
        paths2.iter().any(|p| p.ends_with("script.py")),
        "После удаления cache из списка скрипт py должен быть включен"
    );
}

#[test]
fn test_setting_exclude_paths() {
    let dir = tempdir().expect("tempdir");
    let sub = dir.path().join("special_skip_dir");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("file.txt"), "hello").unwrap();

    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let mut settings = Settings::default();
    settings
        .exclude_paths
        .push(sub.to_string_lossy().to_string());

    let res = walk_source(&source, &settings, None);
    assert!(
        res.entries().is_empty(),
        "Путь из exclude_paths должен быть пропущен"
    );

    settings.exclude_paths.clear();
    let res2 = walk_source(&source, &settings, None);
    assert_eq!(
        res2.entries().len(),
        1,
        "После очистки exclude_paths файл должен быть найден"
    );
}

#[test]
fn test_setting_exclude_files() {
    let dir = tempdir().expect("tempdir");
    fs::write(dir.path().join("regular.txt"), "regular").unwrap();
    fs::write(dir.path().join("backup.tmp"), "backup").unwrap();

    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let mut settings = Settings::default();
    settings.exclude_files.push("*.tmp".to_string());

    let res = walk_source(&source, &settings, None);
    let paths: Vec<String> = res.entries().iter().map(|e| e.path.clone()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("regular.txt")),
        "regular.txt должен быть включен"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("backup.tmp")),
        "backup.tmp должен быть исключен по маске *.tmp"
    );
}

// =========================================================================
// 3. Вкладка «Форматы и пределы»
// =========================================================================

#[test]
fn test_setting_extract_text_formats() {
    let mut config = ExtractConfig::default();
    config.extract_text_formats = false;
    let res = extract_bytes(b"hello world", "txt", false, &config);
    assert!(
        res.text.is_empty(),
        "При выключенных текстовых форматах текст не должен извлекаться"
    );
    assert_eq!(res.status, "metadata-only");

    config.extract_text_formats = true;
    let res2 = extract_bytes(b"hello world", "txt", false, &config);
    assert_eq!(
        res2.text, "hello world",
        "При включенных текстовых форматах текст извлекается"
    );
}

#[test]
fn test_setting_extract_office_formats() {
    let mut config = ExtractConfig::default();
    config.extract_office_formats = false;
    let res = extract_bytes(b"PK\x03\x04fake docx data", "docx", false, &config);
    assert!(
        res.text.is_empty(),
        "При выключенных офисных форматах docx возвращает metadata-only"
    );
    assert_eq!(res.status, "metadata-only");
}

#[test]
fn test_setting_extract_book_formats() {
    let mut config = ExtractConfig::default();
    config.extract_book_formats = false;
    let html_bytes = b"<html><body>Book content</body></html>";
    let res = extract_bytes(html_bytes, "html", false, &config);
    assert!(
        res.text.is_empty(),
        "При выключенных книжных форматах html возвращает metadata-only"
    );

    config.extract_book_formats = true;
    let res2 = extract_bytes(html_bytes, "html", false, &config);
    assert!(
        res2.text.contains("Book content"),
        "При включенных книжных форматах html текст извлекается"
    );
}

#[test]
fn test_setting_extract_pdf_format() {
    let mut config = ExtractConfig::default();
    config.extract_pdf_format = false;
    let res = extract_bytes(b"%PDF-1.4...", "pdf", false, &config);
    assert!(
        res.text.is_empty(),
        "При выключенном extract_pdf_format pdf возвращает metadata-only"
    );
    assert_eq!(res.status, "metadata-only");
}

#[test]
fn test_setting_limit_chars_books_documents() {
    let mut config = ExtractConfig::default();
    config.limit_chars_books_documents = 10;
    let html_bytes = b"<html><body>1234567890EXTRA_TEXT_LONG</body></html>";
    let res = extract_bytes(html_bytes, "html", false, &config);
    assert_eq!(
        res.text.chars().count(),
        10,
        "Текст должен быть обрезан до limit_chars_books_documents = 10"
    );
}

#[test]
fn test_setting_limit_chars_code_service() {
    let mut config = ExtractConfig::default();
    config.limit_chars_code_service = 15;
    let json_bytes = b"{\"key\": \"012345678901234567890\"}";
    let res = extract_bytes(json_bytes, "json", false, &config);
    assert_eq!(
        res.text.chars().count(),
        15,
        "Код должен быть обрезан до limit_chars_code_service = 15"
    );
}

#[test]
fn test_setting_limit_chars_other() {
    let mut config = ExtractConfig::default();
    config.limit_chars_other = 42;
    assert_eq!(config.char_limit("unknown_ext", 500), 42);

    config.limit_chars_other = 100;
    assert_eq!(config.char_limit("unknown_ext", 500), 100);
}

#[test]
fn test_setting_limit_chars_large_txt() {
    let mut config = ExtractConfig::default();
    config.large_txt_threshold_mb = 1;
    config.limit_chars_large_txt = 25;

    // Размер больше 1 МБ -> применяется limit_chars_large_txt
    let limit = config.char_limit("txt", 2 * 1024 * 1024);
    assert_eq!(limit, 25);
}

#[test]
fn test_setting_large_txt_threshold_mb() {
    let mut config = ExtractConfig::default();
    config.large_txt_threshold_mb = 10;
    config.limit_chars_large_txt = 50;
    config.limit_chars_books_documents = 5000;

    // Файл 5 МБ меньше порога 10 МБ -> используется limit_chars_books_documents
    assert_eq!(config.char_limit("txt", 5 * 1024 * 1024), 5000);

    // Снижаем порог до 2 МБ -> теперь файл 5 МБ превышает порог и получает limit_chars_large_txt
    config.large_txt_threshold_mb = 2;
    assert_eq!(config.char_limit("txt", 5 * 1024 * 1024), 50);
}

#[test]
fn test_setting_zip_scan() {
    let dir = tempdir().expect("tempdir");
    let zip_path = dir.path().join("archive.zip");
    {
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("inner_file.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"inner content").unwrap();
        zip.finish().unwrap();
    }

    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    // При zip_scan = false внутренние файлы архива не сканируются
    let mut settings = Settings::default();
    settings.zip_scan = false;
    let res = walk_source(&source, &settings, None);
    let inner_entries: Vec<&poiskovichek::walk::WalkEntry> = res
        .entries()
        .into_iter()
        .filter(|e| e.inside_archive)
        .collect();
    assert!(
        inner_entries.is_empty(),
        "При zip_scan = false файлы внутри ZIP не должны сканироваться"
    );

    // При zip_scan = true внутренний файл найден
    settings.zip_scan = true;
    let res2 = walk_source(&source, &settings, None);
    let inner_entries2: Vec<&poiskovichek::walk::WalkEntry> = res2
        .entries()
        .into_iter()
        .filter(|e| e.inside_archive)
        .collect();
    assert_eq!(
        inner_entries2.len(),
        1,
        "При zip_scan = true внутренний файл ZIP должен быть найден"
    );
}

#[test]
fn test_setting_zip_max_file_size_mb() {
    let dir = tempdir().expect("tempdir");
    let zip_path = dir.path().join("archive_large.zip");
    {
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("huge.txt", SimpleFileOptions::default())
            .unwrap();
        // Записываем 2 МБ данных
        zip.write_all(&[b'A'; 2 * 1024 * 1024]).unwrap();
        zip.finish().unwrap();
    }

    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    // Устанавливаем предел 1 МБ
    let mut settings = Settings::default();
    settings.zip_scan = true;
    settings.zip_max_file_size_mb = 1;

    let res = walk_source(&source, &settings, None);
    assert!(
        res.entries().iter().all(|e| !e.inside_archive),
        "Внутренний файл 2 МБ должен быть пропущен при лимите 1 МБ"
    );

    let skips = res.skips();
    assert!(
        skips
            .iter()
            .any(|s| s.reason.contains("внутренний файл больше 1 МБ")),
        "Должен быть зарегистрирован пропуск из-за превышения размера: {:?}",
        skips
    );
}

#[test]
fn test_setting_pdf_extractor() {
    let mut settings = Settings::default();
    settings.pdf_extractor = PdfExtractorMode::External;
    settings.pdf_external_path = PathBuf::from("non_existent_pdftotext.exe");

    // Валидация выявляет недействительный путь к внешней утилите
    let errors = settings.validate();
    assert!(
        errors.iter().any(|e| e.contains("pdftotext")),
        "Для несуществующего пути pdftotext должна быть ошибка валидации"
    );

    // При возврате к Builtin валидация проходит
    settings.pdf_extractor = PdfExtractorMode::Builtin;
    assert!(
        settings.validate().is_empty(),
        "Для Builtin валидация должна быть успешной"
    );
}

#[test]
fn test_setting_extract_timeout_seconds() {
    let mut settings = Settings::default();
    settings.extract_timeout_seconds = 45;
    assert_eq!(settings.extract_timeout_seconds, 45);

    settings.extract_timeout_seconds = 0;
    let errs = settings.validate();
    assert!(
        errs.iter()
            .any(|e| e.contains("время") || e.contains("секунд")),
        "extract_timeout_seconds = 0 не должно проходить валидацию"
    );
}

// =========================================================================
// 4. Вкладка «Закрытые данные»: secret_extra_patterns
// =========================================================================

#[test]
fn test_setting_secret_extra_patterns() {
    let path = r"C:\work\confidential_budget.xlsx";

    // Без пользовательского шаблона путь не считается закрытым
    assert!(
        !is_sensitive_path(path, &[]),
        "Без шаблона confidential_budget не является закрытым"
    );

    // Добавляем шаблон "budget"
    let patterns = vec!["budget".to_string()];
    assert!(
        is_sensitive_path(path, &patterns),
        "С пользовательским шаблоном budget путь должен определяться как закрытый"
    );
}

// =========================================================================
// 5. Вкладка «Поиск»: enable_morphology, multi_word_mode, max_results, fragment_before/after, default_sort, book_folder_words
// =========================================================================

#[test]
fn test_setting_enable_morphology() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("story.txt"),
        "спешившая женщина вышла на перрон",
    )
    .unwrap();

    let data_dir = root.join("data");
    let mut settings = Settings {
        data_dir,
        sources: vec![Source {
            path: src_dir,
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        enable_morphology: true,
        ..Default::default()
    };

    let morph = setup_test_morph(root);
    let paths = poiskovichek::paths::resolve(root, &settings);
    paths.ensure_dirs().unwrap();
    sync_all_with_options(
        &settings,
        PipelineOptions {
            morph: Some(morph.clone()),
            ..Default::default()
        },
    )
    .unwrap();

    let engine = SearchEngine::open(&paths, &settings, Some(morph)).unwrap();

    // 1. При включенной морфологии «спешившую» находит документ со словом «спешившая»
    let resp_on = engine.search(SearchRequest::new("спешившую"));
    assert_eq!(
        resp_on.total, 1,
        "При включенной морфологии словоформа должна находиться"
    );

    // 2. Выключаем морфологию без перезапуска базы
    settings.enable_morphology = false;
    engine.update_settings(settings);

    let resp_off = engine.search(SearchRequest::new("спешившую"));
    assert_eq!(
        resp_off.total, 0,
        "При выключенной морфологии словоформа «спешившую» не должна находить «спешившая»"
    );
}

#[test]
fn test_setting_multi_word_mode() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(
        src_dir.join("tale.txt"),
        "сельская тихая жизнь мужики на поле",
    )
    .unwrap();

    let data_dir = root.join("data");
    let settings = Settings {
        data_dir,
        sources: vec![Source {
            path: src_dir,
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        multi_word_mode: MultiWordMode::StrictPhrase,
        ..Default::default()
    };

    let morph = setup_test_morph(root);
    let paths = poiskovichek::paths::resolve(root, &settings);
    paths.ensure_dirs().unwrap();
    sync_all_with_options(
        &settings,
        PipelineOptions {
            morph: Some(morph.clone()),
            ..Default::default()
        },
    )
    .unwrap();

    let engine = SearchEngine::open(&paths, &settings, Some(morph)).unwrap();

    // В режиме StrictPhrase слова «сельская мужики» не стоят рядом -> 0 результатов
    let req_strict = SearchRequest::new("сельская мужики").with_mode(MultiWordMode::StrictPhrase);
    let resp_strict = engine.search(req_strict);
    assert_eq!(
        resp_strict.total, 0,
        "В режиме StrictPhrase разнесённые слова не должны находиться"
    );

    // В режиме AllWords слова в любом месте документа находятся
    let req_all = SearchRequest::new("сельская мужики").with_mode(MultiWordMode::AllWords);
    let resp_all = engine.search(req_all);
    assert_eq!(
        resp_all.total, 1,
        "В режиме AllWords разнесённые слова должны находиться"
    );
}

#[test]
fn test_setting_max_results() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    for i in 1..=5 {
        fs::write(
            src_dir.join(format!("doc_{i}.txt")),
            format!("одинаковое уникальный_хэш_{i} во всех документах"),
        )
        .unwrap();
    }

    let data_dir = root.join("data");
    let mut settings = Settings {
        data_dir,
        sources: vec![Source {
            path: src_dir,
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        max_results: 200,
        ..Default::default()
    };

    let paths = poiskovichek::paths::resolve(root, &settings);
    paths.ensure_dirs().unwrap();
    sync_all_with_options(&settings, PipelineOptions::default()).unwrap();

    let engine = SearchEngine::open(&paths, &settings, None).unwrap();

    // С max_results = 200 возвращаются все 5
    let resp_all = engine.search(SearchRequest::new("одинаковое").with_limit(200));
    assert_eq!(resp_all.items.len(), 5);

    // С max_results = 1 возвращается ровно 1 элемент, хотя total = 5
    settings.max_results = 1;
    engine.update_settings(settings);
    let resp_one = engine.search(SearchRequest::new("одинаковое").with_limit(1));
    assert_eq!(resp_one.items.len(), 1);
    assert_eq!(resp_one.total, 5);
}

#[test]
fn test_setting_fragment_length() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let long_text = format!(
        "{} ЦЕЛЕВОЕСЛОВО {}",
        "префикс ".repeat(50),
        "суффикс ".repeat(50)
    );
    fs::write(src_dir.join("long.txt"), &long_text).unwrap();

    let data_dir = root.join("data");
    let mut settings = Settings {
        data_dir,
        sources: vec![Source {
            path: src_dir,
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        fragment_before: 140,
        fragment_after: 300,
        ..Default::default()
    };

    let paths = poiskovichek::paths::resolve(root, &settings);
    paths.ensure_dirs().unwrap();
    sync_all_with_options(&settings, PipelineOptions::default()).unwrap();

    let engine = SearchEngine::open(&paths, &settings, None).unwrap();
    let req = SearchRequest::new("ЦЕЛЕВОЕСЛОВО");
    let resp = engine.search(req.clone());
    let item = &resp.items[0];

    // Фрагмент с длинными границами
    let snippet_long = engine.snippet_for(&item.item_key, &req);
    let long_len = snippet_long.text().chars().count();

    // Уменьшаем границы фрагмента
    settings.fragment_before = 10;
    settings.fragment_after = 15;
    engine.update_settings(settings);

    let snippet_short = engine.snippet_for(&item.item_key, &req);
    let short_len = snippet_short.text().chars().count();

    assert!(
        short_len < long_len,
        "Длина короткого фрагмента ({short_len}) должна быть меньше длинного ({long_len})"
    );
}

#[test]
fn test_setting_default_sort() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path();
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();

    // Создаём файл "aaa.txt" размером 100 байт и "zzz.txt" размером 10000 байт
    fs::write(src_dir.join("aaa.txt"), vec![b'a'; 100]).unwrap();
    fs::write(src_dir.join("zzz.txt"), vec![b'z'; 10_000]).unwrap();

    let data_dir = root.join("data");
    let settings = Settings {
        data_dir,
        sources: vec![Source {
            path: src_dir,
            mode: SourceMode::NamesOnly,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        default_sort: SortMode::SizeDesc,
        ..Default::default()
    };

    let paths = poiskovichek::paths::resolve(root, &settings);
    paths.ensure_dirs().unwrap();
    sync_all_with_options(&settings, PipelineOptions::default()).unwrap();

    let engine = SearchEngine::open(&paths, &settings, None).unwrap();

    // Сортировка по размеру (SizeDesc): zzz.txt (крупный) идёт первым
    let req_size = SearchRequest::new("txt").with_sort(SortMode::SizeDesc);
    let resp_size = engine.search(req_size);
    assert_eq!(resp_size.items[0].name, "zzz.txt");

    // Сортировка по имени (NameAsc): aaa.txt идёт первым
    let req_name = SearchRequest::new("txt").with_sort(SortMode::NameAsc);
    let resp_name = engine.search(req_name);
    assert_eq!(resp_name.items[0].name, "aaa.txt");
}

#[test]
fn test_setting_book_folder_words() {
    let path = r"C:\downloads\rare_novels\article.txt";

    // По умолчанию папка rare_novels не считается книжной
    let default_cat = category_for(path, &[]);
    assert_ne!(
        default_cat, "книга",
        "Без настройки папка не должна классифицироваться как книга"
    );

    // Добавляем "rare_novels" в book_folder_words
    let words = vec!["rare_novels".to_string()];
    let custom_cat = category_for(path, &words);
    assert_eq!(
        custom_cat, "книга",
        "С добавленным словом папка должна классифицироваться как книга"
    );
}

// =========================================================================
// 6. Вкладка «Обновление»: update_on_startup, update_interval_minutes, autostart
// =========================================================================

#[test]
fn test_setting_update_on_startup() {
    let mut settings = Settings::default();
    assert!(settings.update_on_startup, "По умолчанию вкл");

    settings.update_on_startup = false;
    assert!(!settings.update_on_startup, "Можно выключить");
}

#[test]
fn test_setting_update_interval_minutes() {
    let mut settings = Settings::default();
    assert_eq!(settings.update_interval_minutes, 60);

    // Валидация диапазона 0..=1440
    settings.update_interval_minutes = 15;
    assert!(settings.validate().is_empty());

    settings.update_interval_minutes = 2000;
    let errs = settings.validate();
    assert!(
        errs.iter().any(|e| e.contains("1440")),
        "Интервал больше 1440 должен давать ошибку валидации"
    );
}

#[test]
fn test_setting_autostart() {
    const TEST_KEY: &str = r"Software\ПоисКовичеК-тест-настройки\Run";
    const TEST_PARENT: &str = r"Software\ПоисКовичеК-тест-настройки";
    let fake_exe = Path::new(r"C:\bin\Poiskovichek.exe");

    let _ = delete_key(HKEY_CURRENT_USER, TEST_KEY);
    let _ = delete_key(HKEY_CURRENT_USER, TEST_PARENT);

    // Включаем автозапуск в тестовом разделе
    set_autostart_custom(
        HKEY_CURRENT_USER,
        TEST_KEY,
        AUTOSTART_VALUE_NAME,
        true,
        fake_exe,
    )
    .expect("set autostart true");

    let enabled =
        is_autostart_enabled_custom(HKEY_CURRENT_USER, TEST_KEY, AUTOSTART_VALUE_NAME).unwrap();
    assert!(enabled, "Автозапуск должен быть включен в реестре");

    // Выключаем автозапуск
    set_autostart_custom(
        HKEY_CURRENT_USER,
        TEST_KEY,
        AUTOSTART_VALUE_NAME,
        false,
        fake_exe,
    )
    .expect("set autostart false");

    let disabled =
        is_autostart_enabled_custom(HKEY_CURRENT_USER, TEST_KEY, AUTOSTART_VALUE_NAME).unwrap();
    assert!(!disabled, "Автозапуск должен быть выключен в реестре");

    let _ = delete_key(HKEY_CURRENT_USER, TEST_KEY);
    let _ = delete_key(HKEY_CURRENT_USER, TEST_PARENT);
}

// =========================================================================
// 7. Вкладка «Данные и производительность»: data_dir, worker_threads, hdd_gentle_mode, log_keep_days
// =========================================================================

#[test]
fn test_setting_data_dir() {
    let dir = tempdir().expect("tempdir");
    let old_data = dir.path().join("data_old");
    let new_data = dir.path().join("data_new");
    fs::create_dir_all(&old_data).unwrap();

    let cat_path = old_data.join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&cat_path).unwrap();
    let run_id = catalog
        .record_run("scan", "completed", Some("initial"))
        .unwrap();
    drop(catalog);

    // Выполняем перенос данных
    relocate_data(&old_data, &new_data, RelocateMode::Move).expect("relocate");

    assert!(
        new_data.join("catalog.sqlite3").exists(),
        "База должна быть перенесена в новую папку"
    );

    // Открываем перенесённую базу и проверяем сохранность данных
    let reopened = Catalog::open(&new_data.join("catalog.sqlite3")).unwrap();
    let count: i64 = reopened
        .connection()
        .query_row("SELECT count(*) FROM runs WHERE id = ?1", [run_id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn test_setting_worker_threads() {
    let mut settings = Settings::default();
    settings.worker_threads = 4;
    assert!(settings.validate().is_empty());

    settings.worker_threads = 100; // превышает 64
    let errs = settings.validate();
    assert!(
        errs.iter().any(|e| e.contains("64")),
        "Число потоков больше 64 должно давать ошибку валидации"
    );
}

#[test]
fn test_setting_hdd_gentle_mode() {
    let dir = tempdir().expect("tempdir");
    fs::write(dir.path().join("f1.txt"), "1").unwrap();
    fs::write(dir.path().join("f2.txt"), "2").unwrap();

    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let mut settings = Settings::default();
    settings.hdd_gentle_mode = true;

    // В щадящем режиме обход успешно находит все файлы, работая строго в 1 поток
    let res = walk_source(&source, &settings, None);
    assert_eq!(res.entries().len(), 2);
}

#[test]
fn test_setting_log_keep_days() {
    let dir = tempdir().expect("tempdir");
    let logs_dir = dir.path().join("logs");
    fs::create_dir_all(&logs_dir).unwrap();

    // Создаём старый лог-файл (имя 2020-01-01.log)
    let old_log = logs_dir.join("2020-01-01.log");
    fs::write(&old_log, "старый лог").unwrap();

    // Очистка с keep_days = 14 должна удалить файл 2020 года
    let deleted = journal::clean_old_logs(&logs_dir, 14).expect("clean logs");
    assert_eq!(deleted, 1, "Должен быть удалён 1 старый файл лога");
    assert!(!old_log.exists(), "Старый лог должен быть удалён");
}

/// №228: подготовка настроек самопроверки работает на копии в памяти
/// и не меняет ни исходные настройки, ни файл настроек на диске.
#[test]
fn test_self_test_settings_are_prepared_in_memory_only() {
    let dir = tempdir().unwrap();
    let settings_file = dir.path().join("settings.toml");
    let user = Settings {
        data_dir: PathBuf::from(r"C:\ДанныеПользователя"),
        exclude_files: vec!["свой.tmp".to_string()],
        ..Settings::default()
    };
    user.save(&settings_file).unwrap();
    let before = fs::read(&settings_file).unwrap();

    let loaded = Settings::load(&settings_file).unwrap();
    let docs = dir.path().join("docs");
    let temp_data = dir.path().join(".tmp-self-test-1");
    let prepared = loaded.for_self_test(&docs, Some(&temp_data));

    assert!(prepared.sources.iter().any(|s| s.path == docs && s.enabled));
    assert!(prepared.exclude_files.iter().any(|f| f == "self-test.txt"));
    assert!(prepared.exclude_files.iter().any(|f| f == "self-test.log"));
    assert!(prepared.exclude_files.iter().any(|f| f == "свой.tmp"));
    assert_eq!(prepared.data_dir, temp_data);

    // Исходные настройки и файл на диске не изменились
    assert_eq!(loaded, user);
    assert_eq!(fs::read(&settings_file).unwrap(), before);
    assert_eq!(
        Settings::load(&settings_file).unwrap().data_dir,
        user.data_dir
    );

    // Повторная подготовка не дублирует источник и исключения; без временной папки данные те же
    let again = prepared.for_self_test(&docs, None);
    assert_eq!(again.sources.len(), prepared.sources.len());
    assert_eq!(again.exclude_files, prepared.exclude_files);
    assert_eq!(again.data_dir, temp_data);
    let portable = loaded.for_self_test(&docs, None);
    assert_eq!(portable.data_dir, user.data_dir);
}

// Задача №239: новое поле word_forms_mode — старые файлы читаются, значение сохраняется
#[test]
fn test_word_forms_mode_default_for_old_settings_and_roundtrip() {
    use poiskovichek::settings::WordFormsMode;

    // Старый файл без нового поля
    let old = Settings::from_toml_str("max_results = 77\nmulti_word_mode = \"AllWords\"\n")
        .expect("старый файл настроек читается");
    assert_eq!(old.word_forms_mode, WordFormsMode::AllForms);
    assert_eq!(old.max_results, 77);
    assert_eq!(Settings::default().word_forms_mode, WordFormsMode::AllForms);

    // Сохранение и чтение обратно
    let dir = tempdir().unwrap();
    let file = dir.path().join("settings.toml");
    let mut s = Settings::default();
    s.word_forms_mode = WordFormsMode::Exact;
    s.save(&file).unwrap();
    assert_eq!(
        Settings::load(&file).unwrap().word_forms_mode,
        WordFormsMode::Exact
    );

    // Запись в файле читаема по названию
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains("word_forms_mode = \"Exact\""), "{text}");
    assert_eq!(
        Settings::from_toml_str("word_forms_mode = \"точно как набрано\"")
            .unwrap()
            .word_forms_mode,
        WordFormsMode::Exact
    );
}

//! Отдельная тестовая программа (не входит в tests\vse): тест
//! `test_morph_path_points_to_exe_dir_not_current_dir` меняет текущую папку процесса
//! (`env::set_current_dir`), а текущая папка общая на весь процесс — рядом с другими
//! тестами (например, чтением `src` по относительному пути) он давал бы гонку.

use std::env;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

use poiskovichek::paths::{self, local_appdata_dir, roaming_appdata_dir};
use poiskovichek::settings::{
    MultiWordMode, PdfExtractorMode, Settings, SettingsError, SortMode, Source, SourceMode,
};

#[test]
fn test_default_settings_pass_validation() {
    let settings = Settings::default();
    let errors = settings.validate();
    assert!(
        errors.is_empty(),
        "Умолчания должны проходить проверку без ошибок: {errors:?}"
    );
}

#[test]
fn test_save_load_roundtrip() {
    let dir = tempdir().expect("Не удалось создать временную папку");
    let file_path = dir.path().join("settings.toml");

    let mut original = Settings {
        max_results: 350,
        fragment_before: 180,
        fragment_after: 400,
        enable_morphology: false,
        multi_word_mode: MultiWordMode::AllWords,
        default_sort: SortMode::SizeDesc,
        pdf_extractor: PdfExtractorMode::External,
        pdf_external_path: PathBuf::from(r"C:\tools\pdftotext.exe"),
        log_keep_days: 30,
        data_dir: PathBuf::from(r"C:\CustomData"),
        ..Settings::default()
    };
    original.sources.push(Source {
        path: PathBuf::from(r"D:\Documents"),
        mode: SourceMode::DocumentsOnly,
        max_depth: 3,
        enabled: true,
        volume_serial: Some(0x1234ABCD),
        extra_excludes: vec!["*.tmp".into(), "Drafts".into()],
    });

    original
        .save(&file_path)
        .expect("Не удалось сохранить настройки");
    assert!(file_path.exists(), "Файл настроек должен существовать");

    let loaded = Settings::load(&file_path).expect("Не удалось загрузить настройки");
    assert_eq!(
        original, loaded,
        "Загруженные настройки должны совпадать с исходными"
    );
}

#[test]
fn test_corrupted_toml_returns_error_with_line_number() {
    let dir = tempdir().expect("Не удалось создать временную папку");
    let file_path = dir.path().join("corrupted.toml");

    let corrupted_content = "max_results = 200\n[invalid syntax here\nupdate_on_startup = true\n";
    fs::write(&file_path, corrupted_content).expect("Не удалось записать файл");

    let err =
        Settings::load(&file_path).expect_err("Загрузка повреждённого файла должна вернуть ошибку");

    match &err {
        SettingsError::Parse { line, message } => {
            assert!(
                line.is_some(),
                "Ошибка должна содержать номер строки: {message}"
            );
            assert_eq!(line.unwrap(), 2, "Номер строки с ошибкой должен быть 2");
        }
        _ => panic!("Ожидалась ошибка Parse, получено: {err:?}"),
    }

    let err_str = err.to_string();
    assert!(
        err_str.contains("строке 2"),
        "Текст ошибки должен содержать номер строки: {err_str}"
    );
}

#[test]
fn test_unknown_fields_ignored_and_missing_fields_defaulted() {
    let toml_data = r#"
        unknown_future_field = "some_random_value"
        another_unknown = 12345
        max_results = 321
    "#;

    let settings =
        Settings::from_toml_str(toml_data).expect("Неизвестные поля должны быть проигнорированы");

    assert_eq!(
        settings.max_results, 321,
        "Заданное поле должно загрузиться"
    );
    assert_eq!(
        settings.log_keep_days, 14,
        "Отсутствующее поле должно получить значение по умолчанию"
    );
    assert_eq!(
        settings.update_interval_minutes, 60,
        "Отсутствующее поле должно получить значение по умолчанию"
    );
    assert!(
        settings.enable_morphology,
        "Отсутствующее поле должно получить значение по умолчанию"
    );
}

#[test]
fn test_portable_and_standard_mode_resolution() {
    // 1. Переносной режим: settings.toml лежит рядом с exe
    let portable_dir = tempdir().expect("Не удалось создать временную папку");
    let portable_settings_file = portable_dir.path().join("settings.toml");
    fs::write(&portable_settings_file, "max_results = 250\n")
        .expect("Не удалось создать settings.toml");

    let default_settings = Settings::default();
    let portable_paths = paths::resolve(portable_dir.path(), &default_settings);

    assert_eq!(
        portable_paths.settings_file, portable_settings_file,
        "В переносном режиме settings_file должен быть рядом с exe"
    );
    assert_eq!(
        portable_paths.data_dir,
        portable_dir.path().join("data"),
        "В переносном режиме data_dir по умолчанию <папка exe>\\data"
    );
    assert_eq!(
        portable_paths.catalog_db,
        portable_dir.path().join("data").join("catalog.sqlite3")
    );
    assert_eq!(
        portable_paths.index_dir,
        portable_dir.path().join("data").join("index")
    );
    assert_eq!(
        portable_paths.logs_dir,
        portable_dir.path().join("data").join("logs")
    );

    // 2. Стандартный режим: settings.toml отсутствует рядом с exe
    let standard_dir = tempdir().expect("Не удалось создать временную папку");
    let standard_paths = paths::resolve(standard_dir.path(), &default_settings);

    let expected_settings_file = roaming_appdata_dir()
        .join("ПоисКовичеК")
        .join("settings.toml");
    let expected_data_dir = local_appdata_dir().join("ПоисКовичеК");

    assert_eq!(
        standard_paths.settings_file, expected_settings_file,
        "В обычном режиме settings.toml должен быть в %APPDATA%\\ПоисКовичеК"
    );
    assert_eq!(
        standard_paths.data_dir, expected_data_dir,
        "В обычном режиме data_dir должен быть в %LOCALAPPDATA%\\ПоисКовичеК"
    );
    assert_eq!(
        standard_paths.catalog_db,
        expected_data_dir.join("catalog.sqlite3")
    );
    assert_eq!(standard_paths.index_dir, expected_data_dir.join("index"));
    assert_eq!(standard_paths.logs_dir, expected_data_dir.join("logs"));
}

#[test]
fn test_changed_data_dir_returns_new_database_and_index_in_new_folder() {
    let exe_dir = tempdir().expect("Не удалось создать exe_dir");
    let custom_data_dir = tempdir().expect("Не удалось создать custom_data_dir");

    let settings = Settings {
        data_dir: custom_data_dir.path().to_path_buf(),
        ..Settings::default()
    };

    let resolved = paths::resolve(exe_dir.path(), &settings);

    assert_eq!(
        resolved.data_dir,
        custom_data_dir.path(),
        "Папка данных должна быть изменена на новую"
    );
    assert_eq!(
        resolved.catalog_db,
        custom_data_dir.path().join("catalog.sqlite3"),
        "База данных должна размещаться в новой папке данных"
    );
    assert_eq!(
        resolved.index_dir,
        custom_data_dir.path().join("index"),
        "Индекс должен размещаться в новой папке данных"
    );
    assert_eq!(
        resolved.logs_dir,
        custom_data_dir.path().join("logs"),
        "Журнал должен размещаться в новой папке данных"
    );
}

#[test]
fn test_morph_path_points_to_exe_dir_not_current_dir() {
    let exe_dir = tempdir().expect("Не удалось создать exe_dir");
    let work_dir = tempdir().expect("Не удалось создать work_dir");

    let old_cwd = env::current_dir().expect("Не удалось получить текущую папку");
    env::set_current_dir(work_dir.path()).expect("Не удалось сменить текущую папку");

    let morph = paths::morph_path(exe_dir.path());

    // Восстанавливаем рабочую папку перед проверками
    let _ = env::set_current_dir(&old_cwd);

    assert_eq!(
        morph,
        exe_dir.path().join("morph.fst"),
        "morph_path должен указывать строго в папку exe"
    );
    assert_ne!(
        morph,
        work_dir.path().join("morph.fst"),
        "morph_path НЕ должен зависеть от текущей папки процесса"
    );
}

#[test]
fn test_validation_catches_invalid_values() {
    let mut settings = Settings {
        max_results: 0,
        limit_chars_books_documents: 0,
        data_dir: PathBuf::from("relative/path"),
        ..Settings::default()
    };
    settings.sources.push(Source {
        path: PathBuf::from("relative/source"),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    });

    let errors = settings.validate();
    assert!(
        errors.iter().any(|e| e.contains("Число результатов")),
        "Валидация должна проверять max_results"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("Предел символов для книг")),
        "Валидация должна проверять предел символов"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("Папка данных должна быть абсолютным")),
        "Валидация должна требовать абсолютный путь к data_dir"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.contains("Путь источника 'relative/source'")),
        "Валидация должна требовать абсолютный путь к источникам"
    );
}

#[test]
fn test_journal_writes_and_cleans_old_logs() {
    let temp = tempdir().expect("Не удалось создать временную папку");
    let logs_dir = temp.path().join("logs");

    let journal =
        poiskovichek::journal::Journal::open(&logs_dir, 14).expect("Не удалось открыть журнал");

    journal
        .write_entry(
            poiskovichek::journal::Level::Info,
            "test_mod",
            "Тестовое сообщение",
        )
        .expect("Не удалось записать в журнал");

    // Проверяем наличие файла журнала
    let entries: Vec<_> = fs::read_dir(&logs_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 1, "Должен создаться один файл журнала");

    let log_content = fs::read_to_string(entries[0].path()).unwrap();
    assert!(
        log_content.contains("INFO test_mod Тестовое сообщение"),
        "Журнал должен содержать строку в формате: время уровень модуль текст. Содержимое: {log_content}"
    );

    // Создаем устаревший файл журнала (например, за прошлый год)
    let old_log = logs_dir.join("2020-01-01.log");
    fs::write(&old_log, "2020-01-01 10:00:00.000 INFO mod старая запись\n").unwrap();
    assert!(old_log.exists());

    // Вызываем очистку (хранить 14 дней)
    let deleted = journal.clean_old_logs(14).unwrap();
    assert_eq!(deleted, 1, "Должен быть удален ровно один старый файл");
    assert!(!old_log.exists(), "Старый файл должен быть удален");
    assert!(entries[0].path().exists(), "Свежий файл должен остаться");
}

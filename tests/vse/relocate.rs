use std::fs;
use tempfile::tempdir;

use poiskovichek::catalog::Catalog;
use poiskovichek::index::open_or_create;
use poiskovichek::relocate::{RelocateMode, relocate_data};

#[test]
fn test_relocate_move_success() {
    // перенос: база и индекс открываются в новой папке, в старой пусто
    let temp_root = tempdir().expect("tempdir");
    let old_dir = temp_root.path().join("old_data");
    let new_dir = temp_root.path().join("new_data");

    fs::create_dir_all(&old_dir).unwrap();

    // 1. Создаем catalog.sqlite3 в old_dir
    let old_catalog = old_dir.join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&old_catalog).unwrap();
    let run_id = catalog
        .record_run("scan", "completed", Some("test run"))
        .unwrap();
    assert!(run_id > 0);
    drop(catalog);

    // 2. Создаем tantivy index в old_dir/index
    let old_index = old_dir.join("index");
    let writer_handle = open_or_create(&old_index, None).unwrap();
    drop(writer_handle);

    // 3. Выполняем перенос
    relocate_data(&old_dir, &new_dir, RelocateMode::Move).expect("Перенос должен пройти успешно");

    // Проверяем новую папку
    assert!(
        new_dir.join("catalog.sqlite3").exists(),
        "База должна быть в new_dir"
    );
    assert!(
        new_dir.join("index").exists(),
        "Индекс должен быть в new_dir"
    );

    // Проверяем, что база в новой папке открывается и содержит наши данные
    let new_catalog =
        Catalog::open(&new_dir.join("catalog.sqlite3")).expect("База должна открываться");
    let check: String = new_catalog
        .connection()
        .query_row("PRAGMA integrity_check;", [], |r| r.get(0))
        .unwrap();
    assert_eq!(check, "ok");

    let run_count: i64 = new_catalog
        .connection()
        .query_row("SELECT COUNT(*) FROM runs;", [], |r| r.get(0))
        .unwrap();
    assert_eq!(run_count, 1, "Записи должны сохраниться в новой базе");
    drop(new_catalog);

    // Проверяем, что индекс в новой папке открывается
    let new_writer =
        open_or_create(&new_dir.join("index"), None).expect("Индекс должен открываться в new_dir");
    drop(new_writer);

    // Проверяем старую папку: в старой пусто!
    let old_entries_count = fs::read_dir(&old_dir).unwrap().count();
    assert_eq!(
        old_entries_count, 0,
        "В старой папке должно быть пусто: {old_entries_count}"
    );
}

#[test]
fn test_relocate_error_preserves_old_data() {
    // ошибка (папка назначения только для чтения или имитация) -> старая папка цела
    let temp_root = tempdir().expect("tempdir");
    let old_dir = temp_root.path().join("old_data_fail");
    fs::create_dir_all(&old_dir).unwrap();

    let old_catalog = old_dir.join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&old_catalog).unwrap();
    catalog.record_run("scan", "test", None).unwrap();
    drop(catalog);

    let old_extra = old_dir.join("extra.bin");
    fs::write(&old_extra, b"preserved").unwrap();

    // Создаем файл на месте новой папки, чтобы fs::create_dir_all или запись завершились ошибкой
    let blocking_file = temp_root.path().join("new_blocked");
    fs::write(&blocking_file, b"i am a file, not a directory").unwrap();
    let new_dir = blocking_file.join("subfolder");

    let res = relocate_data(&old_dir, &new_dir, RelocateMode::Move);
    assert!(
        res.is_err(),
        "Перенос в некорректную папку должен вернуть ошибку"
    );

    // Проверяем: старая папка полностью цела
    assert!(old_dir.exists(), "Старая папка должна существовать");
    assert!(old_catalog.exists(), "Старая база должна остаться на месте");
    assert!(
        old_extra.exists(),
        "Старый extra.bin должен остаться на месте"
    );

    let cat_check = Catalog::open(&old_catalog).expect("Старая база должна нормально открываться");
    let runs: i64 = cat_check
        .connection()
        .query_row("SELECT COUNT(*) FROM runs;", [], |r| r.get(0))
        .unwrap();
    assert_eq!(runs, 1);
}

#[test]
fn test_relocate_start_empty() {
    // StartEmpty — просто создает новую папку
    let temp_root = tempdir().expect("tempdir");
    let old_dir = temp_root.path().join("old_empty");
    let new_dir = temp_root.path().join("new_empty");

    fs::create_dir_all(&old_dir).unwrap();

    relocate_data(&old_dir, &new_dir, RelocateMode::StartEmpty).unwrap();
    assert!(new_dir.exists(), "Новая папка должна быть создана");
    assert_eq!(
        fs::read_dir(&new_dir).unwrap().count(),
        0,
        "Новая папка должна быть пустой"
    );
}

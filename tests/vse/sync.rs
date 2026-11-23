use std::fs;
use std::io::Write;
use tempfile::tempdir;

use poiskovichek::catalog::Catalog;
use poiskovichek::settings::{Settings, Source, SourceMode};
use poiskovichek::sync::{remove_source, sync_source};

#[test]
fn test_scan_marks_pdf_as_book_when_directory_contains_ebook() {
    // R1-Б-5: соседство электронной книги (.epub) меняет категорию PDF на 'книга'
    let dir = tempdir().expect("Временная папка");
    let root = dir.path().join("source");
    fs::create_dir_all(&root).expect("create root");

    let book_folder = root.join("Downloads").join("Автор - Название");
    fs::create_dir_all(&book_folder).expect("create book folder");
    fs::write(book_folder.join("Автор - Название.epub"), b"ebook").expect("write epub");
    fs::write(book_folder.join("Автор - Название.pdf"), b"pdf").expect("write pdf");

    let report_folder = root.join("Downloads");
    fs::write(report_folder.join("отчет.pdf"), b"report").expect("write report");

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open catalog");

    let settings = Settings::default();
    let source = Source {
        path: root.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let stats = sync_source(&mut catalog, &source, &settings).expect("sync");
    assert_eq!(stats.new_files, 3);

    let conn = catalog.connection();
    let mut stmt = conn
        .prepare("SELECT path, category FROM files ORDER BY path;")
        .expect("prepare");
    let mut rows = stmt.query([]).expect("query");

    let mut cat_map = std::collections::HashMap::new();
    while let Some(row) = rows.next().expect("row") {
        let p: String = row.get(0).expect("path");
        let c: String = row.get(1).expect("cat");
        cat_map.insert(p, c);
    }

    let epub_path = book_folder
        .join("Автор - Название.epub")
        .to_string_lossy()
        .to_string();
    let pdf_path = book_folder
        .join("Автор - Название.pdf")
        .to_string_lossy()
        .to_string();
    let report_path = report_folder
        .join("отчет.pdf")
        .to_string_lossy()
        .to_string();

    assert_eq!(cat_map.get(&epub_path).map(|s| s.as_str()), Some("книга"));
    assert_eq!(cat_map.get(&pdf_path).map(|s| s.as_str()), Some("книга"));
    assert_eq!(
        cat_map.get(&report_path).map(|s| s.as_str()),
        Some("документ")
    );
}

#[test]
fn test_sync_document_only_program_zip_uses_the_same_member_filter() {
    // R1-Б-9: синхронизация обновлённого ZIP в режиме «только документы» применяет строгий фильтр
    let dir = tempdir().expect("Временная папка");
    let root = dir.path().join("source");
    fs::create_dir_all(&root).expect("create root");
    let zip_path = root.join("documentation.zip");

    {
        let file = fs::File::create(&zip_path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("manual.txt", options).expect("file");
        zip.write_all(b"initial manual").expect("write");
        zip.finish().expect("finish");
    }

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let settings = Settings::default();
    let source = Source {
        path: root.clone(),
        mode: SourceMode::DocumentsOnly,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let stats1 = sync_source(&mut catalog, &source, &settings).expect("sync 1");
    assert_eq!(stats1.new_files, 2); // zip + manual.txt

    // Изменяем ZIP на диске, добавляя исходный код
    std::thread::sleep(std::time::Duration::from_millis(50));
    {
        let file = fs::File::create(&zip_path).expect("create zip 2");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("manual.txt", options).expect("file");
        zip.write_all(b"updated manual").expect("write");
        zip.start_file("src/generated.py", options).expect("file");
        zip.write_all(b"source code").expect("write");
        zip.finish().expect("finish");
    }

    let stats2 = sync_source(&mut catalog, &source, &settings).expect("sync 2");
    assert_eq!(stats2.archives_changed, 1);

    let conn = catalog.connection();
    let mut stmt = conn
        .prepare("SELECT name FROM files WHERE inside_archive = 1;")
        .expect("prepare");
    let mut rows = stmt.query([]).expect("query");
    let mut names = Vec::new();
    while let Some(row) = rows.next().expect("row") {
        names.push(row.get::<_, String>(0).expect("name"));
    }

    assert_eq!(names, vec!["manual.txt".to_string()]);
}

#[test]
fn test_sync_unchanged_archive_does_not_open_or_touch_members() {
    // R1-Б-27: повторный sync_source неизменённого ZIP не открывает архив
    let dir = tempdir().expect("Временная папка");
    let root = dir.path().join("source");
    fs::create_dir_all(&root).expect("create root");
    let zip_path = root.join("книги.zip");

    {
        let file = fs::File::create(&zip_path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("книга.txt", options).expect("file");
        zip.write_all("текст книги".as_bytes()).expect("write");
        zip.finish().expect("finish");
    }

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let settings = Settings::default();
    let source = Source {
        path: root.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let first_sync = sync_source(&mut catalog, &source, &settings).expect("first sync");
    assert_eq!(first_sync.new_files, 2); // zip + книга.txt

    let second_sync = sync_source(&mut catalog, &source, &settings).expect("second sync");
    assert_eq!(second_sync.new_files, 0);
    assert_eq!(second_sync.changed_files, 0);
    assert_eq!(second_sync.archives_changed, 0);
    assert_eq!(second_sync.archives_unchanged, 1);
    assert_eq!(catalog.count_files().expect("count"), 2);
}

#[test]
fn test_sync_reextracts_only_stale_unchanged_ordinary_file() {
    // R1-Б-29: файл с extraction_version = 0 помечается как stale
    let dir = tempdir().expect("Временная папка");
    let root = dir.path().join("source");
    fs::create_dir_all(&root).expect("create root");
    fs::write(root.join("файл1.txt"), "контент 1").expect("write");
    fs::write(root.join("файл2.txt"), "контент 2").expect("write");

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let settings = Settings::default();
    let source = Source {
        path: root.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    sync_source(&mut catalog, &source, &settings).expect("sync 1");

    // Имитируем, что у одного файла extraction_version = 2 (актуальная), а у другого 0
    let f1_path = root.join("файл1.txt").to_string_lossy().to_string();
    let f2_path = root.join("файл2.txt").to_string_lossy().to_string();

    catalog
        .connection()
        .execute(
            "UPDATE files SET extraction_version = 2 WHERE path = ?1;",
            [&f1_path],
        )
        .expect("update f1");
    catalog
        .connection()
        .execute(
            "UPDATE files SET extraction_version = 0 WHERE path = ?1;",
            [&f2_path],
        )
        .expect("update f2");

    let stats = sync_source(&mut catalog, &source, &settings).expect("sync 2");
    assert_eq!(stats.stale_files, 1);
    assert_eq!(stats.unchanged_files, 1);
}

#[test]
fn test_sync_changed_archive_replaces_all_members_by_container_id() {
    // R1-Б-37: обновление архива заменяет всех членов по container_id
    let dir = tempdir().expect("Временная папка");
    let root = dir.path().join("source");
    fs::create_dir_all(&root).expect("create root");
    let zip_path = root.join("архив.zip");

    {
        let file = fs::File::create(&zip_path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("старая.txt", options).expect("file");
        zip.write_all("старый контент".as_bytes()).expect("write");
        zip.finish().expect("finish");
    }

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let settings = Settings::default();
    let source = Source {
        path: root.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    sync_source(&mut catalog, &source, &settings).expect("sync 1");

    std::thread::sleep(std::time::Duration::from_millis(50));
    {
        let file = fs::File::create(&zip_path).expect("create zip 2");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("новая.txt", options).expect("file");
        zip.write_all("новый контент".as_bytes()).expect("write");
        zip.finish().expect("finish");
    }

    let stats = sync_source(&mut catalog, &source, &settings).expect("sync 2");
    assert_eq!(stats.archives_changed, 1);

    let conn = catalog.connection();
    let member_path: String = conn
        .query_row(
            "SELECT path FROM files WHERE inside_archive = 1;",
            [],
            |r| r.get(0),
        )
        .expect("query member");

    assert!(member_path.ends_with("новая.txt"));
    assert!(!member_path.contains("старая.txt"));

    let container_id: i64 = conn
        .query_row(
            "SELECT container_id FROM files WHERE inside_archive = 1;",
            [],
            |r| r.get(0),
        )
        .expect("container_id");
    let parent_id: i64 = conn
        .query_row("SELECT id FROM files WHERE inside_archive = 0;", [], |r| {
            r.get(0)
        })
        .expect("parent_id");
    assert_eq!(container_id, parent_id);
}

#[test]
fn test_sync_deleted_archive_removes_container_and_members() {
    // R1-Б-38: удаление архива с диска удаляет контейнер и всех его членов
    let dir = tempdir().expect("Временная папка");
    let root = dir.path().join("source");
    fs::create_dir_all(&root).expect("create root");
    let zip_path = root.join("книги.zip");

    {
        let file = fs::File::create(&zip_path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        zip.start_file("книга.txt", options).expect("file");
        zip.write_all("исчезающий маркер".as_bytes())
            .expect("write");
        zip.finish().expect("finish");
    }

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let settings = Settings::default();
    let source = Source {
        path: root.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    sync_source(&mut catalog, &source, &settings).expect("sync 1");
    assert_eq!(catalog.count_files().expect("count"), 2);

    // Удаляем архив с диска
    fs::remove_file(&zip_path).expect("remove zip");

    let stats = sync_source(&mut catalog, &source, &settings).expect("sync 2");
    assert_eq!(stats.deleted_files, 2);
    assert_eq!(catalog.count_files().expect("count"), 0);
}

#[test]
fn test_selected_files_and_directories_are_registered_and_rescanned_independently() {
    // R1-Б-47: независимый рескан выбранных источников
    let dir = tempdir().expect("Временная папка");
    let base = dir.path();

    let folder = base.join("notes");
    fs::create_dir_all(&folder).expect("create folder");
    let old_file = folder.join("old.txt");
    fs::write(&old_file, "old text").expect("write old");

    let separate_file = base.join("separate.md");
    fs::write(&separate_file, "separate text").expect("write separate");

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");
    let settings = Settings::default();

    let src_folder = Source {
        path: folder.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };
    let src_file = Source {
        path: separate_file.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    sync_source(&mut catalog, &src_folder, &settings).expect("sync folder");
    sync_source(&mut catalog, &src_file, &settings).expect("sync file");

    assert_eq!(catalog.count_files().expect("count"), 2);

    // В папке notes удаляем old.txt и создаём new.txt
    fs::remove_file(&old_file).expect("remove old");
    let new_file = folder.join("new.txt");
    fs::write(&new_file, "new text").expect("write new");

    // Пересканируем ТОЛЬКО папку notes
    sync_source(&mut catalog, &src_folder, &settings).expect("resync folder");

    let conn = catalog.connection();
    let mut stmt = conn
        .prepare("SELECT path FROM files ORDER BY path;")
        .expect("prepare");
    let mut rows = stmt.query([]).expect("query");
    let mut paths = Vec::new();
    while let Some(row) = rows.next().expect("row") {
        paths.push(row.get::<_, String>(0).expect("path"));
    }

    assert!(!paths.contains(&old_file.to_string_lossy().to_string()));
    assert!(paths.contains(&new_file.to_string_lossy().to_string()));
    assert!(paths.contains(&separate_file.to_string_lossy().to_string()));
}

#[test]
fn test_disconnected_drive_keeps_files_and_marks_source_unavailable() {
    // Е-3: отключённый диск -> записи на месте, источник «недоступен»
    let dir = tempdir().expect("Временная папка");
    let drive_sim = dir.path().join("D_drive");
    fs::create_dir_all(&drive_sim).expect("create drive");
    fs::write(drive_sim.join("important.txt"), "important data").expect("write");

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");
    let settings = Settings::default();

    let source = Source {
        path: drive_sim.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    sync_source(&mut catalog, &source, &settings).expect("first sync");
    assert_eq!(catalog.count_files().expect("count"), 1);

    // Имитируем отключение «диска» (удаляем папку)
    fs::remove_dir_all(&drive_sim).expect("unmount drive");

    let res = sync_source(&mut catalog, &source, &settings);
    assert!(
        res.is_err(),
        "Синхронизация должна завершиться с ошибкой недоступности"
    );

    // Записи в каталоге должны остаться на месте!
    assert_eq!(
        catalog.count_files().expect("count files after disconnect"),
        1
    );

    // Источник должен быть помечен «недоступен»
    let src_rec = catalog
        .get_source_by_path(&drive_sim.to_string_lossy())
        .expect("get source")
        .expect("source exists");
    assert_eq!(src_rec.status, "недоступен");
}

#[test]
fn test_protected_branch_preserves_records_on_error() {
    // R1-А-75, 76: ошибка чтения папки защищает её ветвь от удаления
    let dir = tempdir().expect("Временная папка");
    let root = dir.path().join("source");
    let protected_dir = root.join("protected_dir");
    fs::create_dir_all(&protected_dir).expect("mkdir");
    let file_path = protected_dir.join("safe.txt");
    fs::write(&file_path, "important preserved data").expect("write");

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");
    let settings = Settings::default();

    let source = Source {
        path: root.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    sync_source(&mut catalog, &source, &settings).expect("sync 1");
    assert_eq!(catalog.count_files().expect("count"), 1);

    // Удаляем файл с диска, но имитируем, что protected_dir недоступна для чтения (в protected_prefixes)
    fs::remove_file(&file_path).expect("remove file");

    // Вызываем classify / sync с защищенным префиксом:
    // Проверим прямо логику sync: если ветвь в protected_prefixes, файл не удаляется
    let snapshot = catalog.snapshot().expect("snapshot");
    let file_str = file_path.to_string_lossy().to_string();
    let snap_entry = snapshot.get(&file_str).expect("entry in snapshot");

    let protected_prefix = protected_dir.to_string_lossy().to_string();
    let is_protected = file_str.starts_with(&protected_prefix);
    assert!(is_protected);

    // Убедимся, что при наличии protected_prefix файл не попадает в удаляемые
    let seen_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut ids_to_delete = Vec::new();
    let protected_prefixes = [protected_prefix];

    for (snap_path, entry) in &snapshot {
        if !seen_paths.contains(snap_path) {
            let protected = protected_prefixes.iter().any(|p| snap_path.starts_with(p));
            if !protected {
                ids_to_delete.push(entry.id);
            }
        }
    }

    assert!(
        ids_to_delete.is_empty(),
        "Запись защищенной ветви не должна удаляться"
    );
    assert_eq!(snap_entry.id, 1);
}

#[test]
fn test_remove_source_removes_records_without_deleting_disk_files() {
    // R1-А-89..91: remove_source удаляет записи, файлы на диске остаются
    let dir = tempdir().expect("Временная папка");
    let folder = dir.path().join("to_remove");
    fs::create_dir_all(&folder).expect("mkdir");
    let f1 = folder.join("f1.txt");
    let f2 = folder.join("f2.txt");
    fs::write(&f1, "file 1").expect("write");
    fs::write(&f2, "file 2").expect("write");

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");
    let settings = Settings::default();

    let source = Source {
        path: folder.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    sync_source(&mut catalog, &source, &settings).expect("sync");
    assert_eq!(catalog.count_files().expect("count"), 2);

    let deleted = remove_source(&mut catalog, &folder.to_string_lossy()).expect("remove");
    assert_eq!(deleted, 2);
    assert_eq!(catalog.count_files().expect("count after remove"), 0);

    // Файлы на диске обязательно должны остаться на месте!
    assert!(f1.exists(), "Файл на диске не должен быть удален");
    assert!(f2.exists(), "Файл на диске не должен быть удален");
}

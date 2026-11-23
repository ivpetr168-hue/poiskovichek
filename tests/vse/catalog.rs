use std::time::Instant;
use tempfile::tempdir;

use poiskovichek::catalog::{Catalog, CatalogError, FileRecord};

#[test]
fn test_catalog_open_or_create_and_pragmas() {
    let dir = tempdir().expect("Временная папка");
    let db_path = dir.path().join("catalog.sqlite3");

    let catalog = Catalog::open_or_create(&db_path).expect("Создание каталога");
    let conn = catalog.connection();

    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
        .expect("journal_mode");
    assert_eq!(journal_mode.to_lowercase(), "wal");

    let sync_mode: i64 = conn
        .query_row("PRAGMA synchronous;", [], |r| r.get(0))
        .expect("synchronous");
    // NORMAL = 1
    assert_eq!(sync_mode, 1);

    let version: u32 = conn
        .query_row("PRAGMA user_version;", [], |r| r.get(0))
        .expect("user_version");
    assert_eq!(version, 1);
}

#[test]
fn test_incompatible_schema_version_returns_clear_error() {
    // R1-Б-2: открытие базы старой версии схемы -> понятная ошибка
    let dir = tempdir().expect("Временная папка");
    let db_path = dir.path().join("legacy.sqlite3");

    {
        let conn = rusqlite::Connection::open(&db_path).expect("open raw");
        conn.execute(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT);",
            [],
        )
        .expect("create legacy table");
        conn.execute("PRAGMA user_version = 0;", [])
            .expect("pragma user_version");
    }

    let err = Catalog::open(&db_path).expect_err("Должна быть ошибка версии схемы");
    match err {
        CatalogError::IncompatibleSchemaVersion(v) => {
            assert_eq!(v, 0);
            let msg = err.to_string();
            assert!(
                msg.contains("Версия схемы базы данных не поддерживается"),
                "Сообщение должно быть понятным: {msg}"
            );
        }
        other => panic!("Ожидалась IncompatibleSchemaVersion, получено: {other:?}"),
    }
}

#[test]
fn test_remove_directory_handles_more_than_sqlite_parameter_limit() {
    // R1-Б-36: удаление более 999 записей (1 100 файлов) через временную таблицу
    let dir = tempdir().expect("Временная папка");
    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let mut batch = Vec::with_capacity(1100);
    for i in 0..1100 {
        batch.push(FileRecord {
            id: None,
            path: format!(r"D:\много\файл-{i}.txt"),
            source_id: None,
            volume_id: None,
            rel_path: format!("файл-{i}.txt"),
            name: format!("файл-{i}.txt"),
            extension: ".txt".into(),
            size: i as u64,
            modified_ns: i as i64,
            created_ns: i as i64,
            category: "документ".into(),
            inside_archive: false,
            container_id: None,
            content_status: Some("pending".into()),
            extraction_version: 0,
            text_sha: None,
            quick_hash: None,
            sha256: None,
            error: None,
            scan_run: Some(1),
            available: true,
        });
    }

    let ids = catalog.upsert_files(&batch).expect("upsert 1100");
    assert_eq!(ids.len(), 1100);
    assert_eq!(catalog.count_files().expect("count"), 1100);

    let deleted = catalog.delete_files(&ids).expect("delete 1100 files");
    assert_eq!(deleted, 1100);
    assert_eq!(catalog.count_files().expect("count"), 0);
}

#[test]
fn test_identical_texts_share_one_blob_and_one_search_result() {
    // R1-Б-59: два файла с одинаковым текстом делят один блоб в texts
    let dir = tempdir().expect("Временная папка");
    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let files = vec![
        FileRecord {
            id: None,
            path: r"D:\первая.txt".into(),
            source_id: None,
            volume_id: None,
            rel_path: "первая.txt".into(),
            name: "первая.txt".into(),
            extension: ".txt".into(),
            size: 100,
            modified_ns: 10,
            created_ns: 10,
            category: "документ".into(),
            inside_archive: false,
            container_id: None,
            content_status: Some("pending".into()),
            extraction_version: 0,
            text_sha: None,
            quick_hash: None,
            sha256: None,
            error: None,
            scan_run: Some(1),
            available: true,
        },
        FileRecord {
            id: None,
            path: r"D:\вторая.txt".into(),
            source_id: None,
            volume_id: None,
            rel_path: "вторая.txt".into(),
            name: "вторая.txt".into(),
            extension: ".txt".into(),
            size: 100,
            modified_ns: 20,
            created_ns: 20,
            category: "документ".into(),
            inside_archive: false,
            container_id: None,
            content_status: Some("pending".into()),
            extraction_version: 0,
            text_sha: None,
            quick_hash: None,
            sha256: None,
            error: None,
            scan_run: Some(1),
            available: true,
        },
    ];

    let ids = catalog.upsert_files(&files).expect("upsert");
    assert_eq!(ids.len(), 2);

    let text = "общий уникальный текст книги для проверки дедупликации";
    let sha1 = catalog.store_text(ids[0], text).expect("store 1");
    let sha2 = catalog.store_text(ids[1], text).expect("store 2");

    assert_eq!(sha1, sha2);

    // Проверяем, что в texts ровно одна строка
    let count: i64 = catalog
        .connection()
        .query_row("SELECT COUNT(*) FROM texts;", [], |r| r.get(0))
        .expect("count texts");
    assert_eq!(count, 1);

    // Проверяем чтение текста обратно
    let decompressed = catalog.get_text(&sha1).expect("get_text");
    assert_eq!(decompressed.as_deref(), Some(text));
}

#[test]
fn test_deleting_one_copy_keeps_shared_blob_and_search() {
    // R1-Б-62: удаление одной из двух копий оставляет текст в texts
    let dir = tempdir().expect("Временная папка");
    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let files = vec![
        FileRecord {
            id: None,
            path: r"D:\copy1.txt".into(),
            source_id: None,
            volume_id: None,
            rel_path: "copy1.txt".into(),
            name: "copy1.txt".into(),
            extension: ".txt".into(),
            size: 50,
            modified_ns: 1,
            created_ns: 1,
            category: "документ".into(),
            inside_archive: false,
            container_id: None,
            content_status: Some("pending".into()),
            extraction_version: 0,
            text_sha: None,
            quick_hash: None,
            sha256: None,
            error: None,
            scan_run: Some(1),
            available: true,
        },
        FileRecord {
            id: None,
            path: r"D:\copy2.txt".into(),
            source_id: None,
            volume_id: None,
            rel_path: "copy2.txt".into(),
            name: "copy2.txt".into(),
            extension: ".txt".into(),
            size: 50,
            modified_ns: 1,
            created_ns: 1,
            category: "документ".into(),
            inside_archive: false,
            container_id: None,
            content_status: Some("pending".into()),
            extraction_version: 0,
            text_sha: None,
            quick_hash: None,
            sha256: None,
            error: None,
            scan_run: Some(1),
            available: true,
        },
    ];

    let ids = catalog.upsert_files(&files).expect("upsert");
    let text = "общий маркер текста";
    let sha = catalog.store_text(ids[0], text).expect("store 1");
    catalog.store_text(ids[1], text).expect("store 2");

    // Удаляем первую копию
    catalog.delete_files(&[ids[0]]).expect("delete first copy");

    // Блоб текста должен остаться на месте (ссылается copy2)
    let text_count: i64 = catalog
        .connection()
        .query_row("SELECT COUNT(*) FROM texts;", [], |r| r.get(0))
        .expect("count");
    assert_eq!(text_count, 1);

    let text_content = catalog.get_text(&sha).expect("get_text");
    assert_eq!(text_content.as_deref(), Some(text));

    // Очередь index_outbox пуста (удалений текста не было)
    let outbox: i64 = catalog
        .connection()
        .query_row("SELECT COUNT(*) FROM index_outbox;", [], |r| r.get(0))
        .expect("count outbox");
    assert_eq!(outbox, 0);
}

#[test]
fn test_deleting_last_copy_removes_blob_and_text_index() {
    // R1-Б-63: удаление последней копии удаляет текст из texts триггером и пишет в index_outbox
    let dir = tempdir().expect("Временная папка");
    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    let file = FileRecord {
        id: None,
        path: r"D:\последняя.txt".into(),
        source_id: None,
        volume_id: None,
        rel_path: "последняя.txt".into(),
        name: "последняя.txt".into(),
        extension: ".txt".into(),
        size: 50,
        modified_ns: 1,
        created_ns: 1,
        category: "документ".into(),
        inside_archive: false,
        container_id: None,
        content_status: Some("pending".into()),
        extraction_version: 0,
        text_sha: None,
        quick_hash: None,
        sha256: None,
        error: None,
        scan_run: Some(1),
        available: true,
    };

    let ids = catalog.upsert_files(&[file]).expect("upsert");
    let sha = catalog
        .store_text(ids[0], "последний маркер")
        .expect("store");

    // Удаляем последнюю копию
    catalog.delete_files(&[ids[0]]).expect("delete");

    // Таблица texts должна быть пуста (триггер удалил блоб)
    let text_count: i64 = catalog
        .connection()
        .query_row("SELECT COUNT(*) FROM texts;", [], |r| r.get(0))
        .expect("count texts");
    assert_eq!(text_count, 0);

    // Триггер texts_after_delete должен был добавить sha в index_outbox
    let outbox = catalog.drain_index_outbox().expect("drain outbox");
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0], (sha, "delete".to_string()));

    // Повторный drain возвращает пустой список
    let outbox_after = catalog.drain_index_outbox().expect("drain again");
    assert!(outbox_after.is_empty());
}

#[test]
#[ignore]
fn perf_snapshot() {
    // Замер скорости построения снимка на 200 000 записей (цель ≤ 1 с)
    let dir = tempdir().expect("Временная папка");
    let db_path = dir.path().join("perf_catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).expect("open");

    const TOTAL_FILES: usize = 200_000;
    const CHUNK_SIZE: usize = 5_000;

    println!("Генерация {TOTAL_FILES} записей в SQLite...");
    let start_gen = Instant::now();

    let vol_id = catalog
        .record_volume(0x12345678, "DATA", "D:")
        .expect("volume");
    let src_id = catalog
        .record_source(&poiskovichek::catalog::SourceRecord {
            id: None,
            path: r"D:\data".into(),
            mode: "Full".into(),
            max_depth: 0,
            enabled: true,
            volume_id: Some(vol_id),
            status: "ready".into(),
            last_indexed_at: None,
            details: None,
        })
        .expect("source");

    for chunk_idx in 0..(TOTAL_FILES / CHUNK_SIZE) {
        let mut batch = Vec::with_capacity(CHUNK_SIZE);
        for i in 0..CHUNK_SIZE {
            let num = chunk_idx * CHUNK_SIZE + i;
            batch.push(FileRecord {
                id: None,
                path: format!(r"D:\data\folder_{}\file_{}.txt", num / 1000, num),
                source_id: Some(src_id),
                volume_id: Some(vol_id),
                rel_path: format!(r"folder_{}\file_{}.txt", num / 1000, num),
                name: format!("file_{}.txt", num),
                extension: ".txt".into(),
                size: (num * 17) as u64,
                modified_ns: 1_700_000_000_000_000_000 + num as i64,
                created_ns: 1_600_000_000_000_000_000 + num as i64,
                category: "документ".into(),
                inside_archive: false,
                container_id: None,
                content_status: Some("pending".into()),
                extraction_version: 0,
                text_sha: None,
                quick_hash: None,
                sha256: None,
                error: None,
                scan_run: Some(1),
                available: true,
            });
        }
        catalog.upsert_files(&batch).expect("upsert chunk");
    }

    println!(
        "Записи созданы за {:.2} с",
        start_gen.elapsed().as_secs_f64()
    );

    // Замер снимка
    let start_snap = Instant::now();
    let snapshot = catalog.snapshot().expect("snapshot");
    let snap_duration = start_snap.elapsed();

    println!(
        "Снимок {TOTAL_FILES} записей построен за {:.3} с ({} мс)",
        snap_duration.as_secs_f64(),
        snap_duration.as_millis()
    );

    assert_eq!(snapshot.len(), TOTAL_FILES);
    assert!(
        snap_duration.as_secs_f64() <= 1.0,
        "Снимок 200 тыс. записей должен строиться ≤ 1 с (фактически: {:.3} с)",
        snap_duration.as_secs_f64()
    );
}

fn vstavit_fayl(catalog: &Catalog, path: &str) {
    catalog
        .connection()
        .execute(
            "INSERT INTO files (path, rel_path, name, extension, size, modified_ns, created_ns, category) \
             VALUES (?1, 'r', 'n', '.txt', 1, 1, 1, 'документ');",
            [path],
        )
        .expect("insert file");
}

fn istochnik_fayla(catalog: &Catalog, path: &str) -> (Option<i64>, Option<i64>) {
    catalog
        .connection()
        .query_row(
            "SELECT source_id, volume_id FROM files WHERE path = ?1;",
            [path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("select file")
}

fn zaregistrirovat(catalog: &mut Catalog, path: &str) -> i64 {
    catalog
        .record_source(&poiskovichek::catalog::SourceRecord {
            id: None,
            path: path.to_string(),
            mode: "Full".into(),
            max_depth: 0,
            enabled: true,
            volume_id: None,
            status: "ready".into(),
            last_indexed_at: None,
            details: None,
        })
        .expect("record_source")
}

/// №221: вложенные источники — файл у самого длинного совпадающего пути;
/// символы % и _ в пути — обычные символы; чужие папки с похожим началом не задеваются.
#[test]
fn test_assign_sources_to_files_nested_and_special_chars() {
    let dir = tempdir().expect("Временная папка");
    let mut catalog = Catalog::open_or_create(&dir.path().join("c.sqlite3")).expect("open");

    for path in [
        r"C:\a.txt",
        r"C:\Users\x\b.txt",
        r"C:\Users\xy\c.txt",
        r"C:\Users\x_y\d.txt",
        r"D:\50%\e.txt",
        r"D:\500\f.txt",
        r"D:\a.zip",
        "D:\\a.zip → внутри\\g.txt",
        r"E:\чужой\h.txt",
    ] {
        vstavit_fayl(&catalog, path);
    }

    let vol_c = catalog.record_volume(1, "C", "C:").expect("volume c");
    let vol_d = catalog.record_volume(2, "D", "D:").expect("volume d");
    let (s_c, s_x, s_pct, s_zip) = (
        zaregistrirovat(&mut catalog, r"C:\"),
        zaregistrirovat(&mut catalog, r"C:\Users\x"),
        zaregistrirovat(&mut catalog, r"D:\50%"),
        zaregistrirovat(&mut catalog, r"D:\a.zip"),
    );

    // Источники передаются в произвольном порядке
    let items = vec![
        (s_c, Some(vol_c), r"C:\".to_string()),
        (s_x, Some(vol_c), r"C:\Users\x".to_string()),
        (s_pct, Some(vol_d), r"D:\50%".to_string()),
        (s_zip, Some(vol_d), r"D:\a.zip".to_string()),
    ];
    let n = catalog
        .assign_sources_to_files(&items)
        .expect("assign_sources_to_files");
    assert_eq!(n, 7);

    let (c, x, pct, zip) = (
        (Some(s_c), Some(vol_c)),
        (Some(s_x), Some(vol_c)),
        (Some(s_pct), Some(vol_d)),
        (Some(s_zip), Some(vol_d)),
    );
    assert_eq!(istochnik_fayla(&catalog, r"C:\a.txt"), c);
    assert_eq!(istochnik_fayla(&catalog, r"C:\Users\x\b.txt"), x);
    // «x» не захватывает «xy» и «x_y»; они принадлежат диску C
    assert_eq!(istochnik_fayla(&catalog, r"C:\Users\xy\c.txt"), c);
    assert_eq!(istochnik_fayla(&catalog, r"C:\Users\x_y\d.txt"), c);
    // «%» в пути источника не работает как подстановка: «D:\500» не его
    assert_eq!(istochnik_fayla(&catalog, r"D:\50%\e.txt"), pct);
    assert_eq!(istochnik_fayla(&catalog, r"D:\500\f.txt"), (None, None));
    // источник-архив и его члены
    assert_eq!(istochnik_fayla(&catalog, r"D:\a.zip"), zip);
    assert_eq!(istochnik_fayla(&catalog, "D:\\a.zip → внутри\\g.txt"), zip);
    assert_eq!(istochnik_fayla(&catalog, r"E:\чужой\h.txt"), (None, None));

    // Повторный вызов ничего не меняет
    assert_eq!(catalog.assign_sources_to_files(&items).expect("again"), 0);
}

/// №221: уже привязанные файлы не переписываются, а список источников отдаёт id и путь как есть.
#[test]
fn test_assign_sources_keeps_existing_and_list_sources_returns_path() {
    let dir = tempdir().expect("Временная папка");
    let mut catalog = Catalog::open_or_create(&dir.path().join("c.sqlite3")).expect("open");
    let src = |path: &str| poiskovichek::catalog::SourceRecord {
        id: None,
        path: path.to_string(),
        mode: "Full".into(),
        max_depth: 0,
        enabled: true,
        volume_id: None,
        status: "ready".into(),
        last_indexed_at: None,
        details: None,
    };
    let id_a = catalog.record_source(&src(r"D:\a")).expect("source a");
    let id_b = catalog.record_source(&src(r"D:\b\")).expect("source b");

    vstavit_fayl(&catalog, r"D:\a\1.txt");
    vstavit_fayl(&catalog, r"D:\a\2.txt");
    catalog
        .connection()
        .execute(
            "UPDATE files SET source_id = ?1 WHERE path = ?2;",
            rusqlite::params![id_b, r"D:\a\1.txt"],
        )
        .expect("pre-assign");

    let n = catalog
        .assign_sources_to_files(&[(id_a, None, r"D:\a".to_string())])
        .expect("assign");
    assert_eq!(n, 1);
    assert_eq!(istochnik_fayla(&catalog, r"D:\a\1.txt").0, Some(id_b));
    assert_eq!(istochnik_fayla(&catalog, r"D:\a\2.txt").0, Some(id_a));

    let list = catalog.list_sources().expect("list_sources");
    let pairs: Vec<(Option<i64>, String)> = list.into_iter().map(|s| (s.id, s.path)).collect();
    assert_eq!(
        pairs,
        vec![
            (Some(id_a), r"D:\a".to_string()),
            (Some(id_b), r"D:\b\".to_string())
        ]
    );
}

/// №221: привязка 200 тысяч записей одной транзакцией — быстро.
#[test]
fn test_assign_sources_to_files_200k_is_fast() {
    let dir = tempdir().expect("Временная папка");
    let mut catalog = Catalog::open_or_create(&dir.path().join("c.sqlite3")).expect("open");
    catalog
        .connection()
        .execute(
            "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c WHERE n < 200000) \
             INSERT INTO files (path, rel_path, name, extension, size, modified_ns, created_ns, category) \
             SELECT 'C:\\data\\f' || n || '.txt', 'r', 'n', '.txt', 1, 1, 1, 'документ' FROM c;",
            [],
        )
        .expect("bulk insert");

    let vol = catalog.record_volume(1, "C", "C:").expect("volume");
    let items = vec![
        (
            zaregistrirovat(&mut catalog, r"C:\"),
            Some(vol),
            r"C:\".to_string(),
        ),
        (
            zaregistrirovat(&mut catalog, r"C:\data"),
            Some(vol),
            r"C:\data".to_string(),
        ),
    ];
    let start = Instant::now();
    let n = catalog.assign_sources_to_files(&items).expect("assign");
    let elapsed = start.elapsed();
    assert_eq!(n, 200_000);
    assert!(
        elapsed.as_secs_f64() <= 5.0,
        "Привязка 200 тыс. записей заняла {:.2} с",
        elapsed.as_secs_f64()
    );
}

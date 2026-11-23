use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::bounded;
use tempfile::tempdir;

use poiskovichek::catalog::Catalog;
use poiskovichek::index::reader;
use poiskovichek::pipeline::{
    CancelToken, IndexLock, PipelineError, PipelineOptions, Progress, index_sources_with_options,
    reextract_stale_with_settings, reindex_from_saved_texts_with_settings,
    remove_paths_with_settings, sync_all_with_options,
};
use poiskovichek::settings::{Settings, Source, SourceMode};
use tantivy::query::QueryParser;

fn create_test_settings(data_dir: &Path, source_dir: &Path) -> Settings {
    Settings {
        data_dir: data_dir.to_path_buf(),
        enable_morphology: false, // тесты надёжно работают без внешнего словаря
        sources: vec![Source {
            path: source_dir.to_path_buf(),
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        ..Default::default()
    }
}

/// 1. Ёмкость всех очередей 4, 2 000 файлов (половина с текстом) → завершение < 60 с,
///    в базе 2 000 записей, в индексе все тексты.
#[test]
fn test_pipeline_queue_cap_4_with_2000_files() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    // Создаём 2 000 файлов: 1 000 с текстом (.txt), 1 000 бинарных (.bin)
    for i in 0..1000 {
        let txt_path = source_dir.join(format!("doc_{:04}.txt", i));
        fs::write(
            txt_path,
            format!("Это текстовый документ номер {:04} для проверки поиска", i),
        )
        .expect("write txt");

        let bin_path = source_dir.join(format!("bin_{:04}.bin", i));
        fs::write(bin_path, [0u8; 32]).expect("write bin");
    }

    let settings = create_test_settings(&data_dir, &source_dir);
    let options = PipelineOptions {
        queue_capacity: 4,
        in_process_workers: true,
        ..Default::default()
    };

    let start = Instant::now();
    let summary = sync_all_with_options(&settings, options).expect("sync_all");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(60),
        "Завершение должно быть < 60 с, факт: {:?}",
        elapsed
    );
    assert_eq!(summary.added, 2000);

    // Проверяем количество записей в базе данных
    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    let count = catalog.count_files().expect("count files");
    assert_eq!(count, 2000, "В базе должно быть ровно 2000 записей");

    // Проверяем количество документов в индексе
    let rdr = reader(&data_dir.join("index"), None).expect("open index reader");
    let index_docs = rdr.searcher().num_docs();
    assert_eq!(
        index_docs, 1000,
        "В индексе должно быть ровно 1000 документов"
    );
}

/// 2. То же с отменой на середине → затем повторный запуск доводит до 2 000.
#[test]
fn test_pipeline_cancel_and_resume() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    for i in 0..1000 {
        let txt_path = source_dir.join(format!("doc_{:04}.txt", i));
        fs::write(
            txt_path,
            format!("Документ {:04} для проверки отмены конвейера", i),
        )
        .expect("write txt");

        let bin_path = source_dir.join(format!("bin_{:04}.bin", i));
        fs::write(bin_path, [0u8; 16]).expect("write bin");
    }

    let settings = create_test_settings(&data_dir, &source_dir);

    let cancel_token = CancelToken::new();
    let (prog_tx, prog_rx) = bounded::<Progress>(100);

    // Запускаем отмену, как только поток обхода обработал часть файлов
    let cancel_for_thread = cancel_token.clone();
    std::thread::spawn(move || {
        while let Ok(p) = prog_rx.recv() {
            if p.written >= 100 {
                cancel_for_thread.cancel();
                break;
            }
        }
    });

    let options1 = PipelineOptions {
        queue_capacity: 4,
        in_process_workers: true,
        cancel_token: Some(cancel_token),
        progress_tx: Some(prog_tx),
        ..Default::default()
    };

    let _summary1 = sync_all_with_options(&settings, options1).expect("first sync with cancel");

    let catalog1 = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    let count1 = catalog1.count_files().expect("count 1");
    assert!(
        count1 < 2000,
        "При отмене должно было записаться меньше 2000, факт: {count1}"
    );

    // Повторный запуск без отмены доводит до 2000
    let options2 = PipelineOptions {
        queue_capacity: 4,
        in_process_workers: true,
        ..Default::default()
    };

    let _summary2 = sync_all_with_options(&settings, options2).expect("second sync to completion");

    let catalog2 = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    let count2 = catalog2.count_files().expect("count 2");
    assert_eq!(
        count2, 2000,
        "Повторный запуск должен довести число записей до 2000"
    );

    let rdr = reader(&data_dir.join("index"), None).expect("reader");
    assert_eq!(
        rdr.searcher().num_docs(),
        1000,
        "В индексе должно быть 1000 текстов"
    );
}

/// 3. Изменённый файл: старое слово больше не находится, новое находится (R1-Б-34).
#[test]
fn test_pipeline_changed_file_replaces_search_text() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    let file_path = source_dir.join("file.txt");
    fs::write(&file_path, "первоначальное_редкое_слово_разборка").expect("write");

    let settings = create_test_settings(&data_dir, &source_dir);
    let options = PipelineOptions {
        in_process_workers: true,
        ..Default::default()
    };

    sync_all_with_options(&settings, options.clone()).expect("initial sync");

    // Проверяем, что старое слово ищется
    {
        let rdr = reader(&data_dir.join("index"), None).expect("reader");
        let searcher = rdr.searcher();
        let body_field = searcher.schema().get_field("body").expect("body");
        let parser = QueryParser::for_index(searcher.index(), vec![body_field]);
        let q = parser
            .parse_query("первоначальное_редкое_слово_разборка")
            .expect("parse");
        let count = searcher
            .search(&q, &tantivy::collector::Count)
            .expect("search");
        assert_eq!(count, 1);
    }

    // Изменяем содержимое файла и сдвигаем mtime
    std::thread::sleep(Duration::from_millis(50));
    fs::write(&file_path, "обновленное_уникальное_слово_ремонт").expect("overwrite");

    sync_all_with_options(&settings, options).expect("resync");

    // Проверяем: старое слово исчезло, новое появилось
    {
        let rdr = reader(&data_dir.join("index"), None).expect("reader");
        let searcher = rdr.searcher();
        let body_field = searcher.schema().get_field("body").expect("body");
        let parser = QueryParser::for_index(searcher.index(), vec![body_field]);

        let q_old = parser
            .parse_query("первоначальное_редкое_слово_разборка")
            .expect("parse old");
        let count_old = searcher
            .search(&q_old, &tantivy::collector::Count)
            .expect("search old");
        assert_eq!(count_old, 0, "Старое слово больше не должно находиться");

        let q_new = parser
            .parse_query("обновленное_уникальное_слово_ремонт")
            .expect("parse new");
        let count_new = searcher
            .search(&q_new, &tantivy::collector::Count)
            .expect("search new");
        assert_eq!(count_new, 1, "Новое слово должно находиться");
    }
}

/// 4. Удалённая последняя копия → документ индекса удалён; одна из двух копий → остался (R1-Б-62, 63).
#[test]
fn test_pipeline_delete_copies_keeps_shared_blob_and_index() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    let copy1 = source_dir.join("copy1.txt");
    let copy2 = source_dir.join("copy2.txt");
    let content = "общий_уникальный_текст_для_дедупликации_копий";
    fs::write(&copy1, content).expect("write 1");
    fs::write(&copy2, content).expect("write 2");

    let settings = create_test_settings(&data_dir, &source_dir);
    let options = PipelineOptions {
        in_process_workers: true,
        ..Default::default()
    };

    sync_all_with_options(&settings, options.clone()).expect("initial sync");

    // В индексе ровно 1 документ для двух копий
    {
        let rdr = reader(&data_dir.join("index"), None).expect("reader");
        let searcher = rdr.searcher();
        let body_field = searcher.schema().get_field("body").expect("body");
        let parser = QueryParser::for_index(searcher.index(), vec![body_field]);
        let q = parser
            .parse_query("общий_уникальный_текст_для_дедупликации_копий")
            .expect("parse");
        let count = searcher
            .search(&q, &tantivy::collector::Count)
            .expect("search");
        assert_eq!(count, 1);
    }

    // Удаляем первую копию (R1-Б-62)
    fs::remove_file(&copy1).expect("remove copy1");
    sync_all_with_options(&settings, options.clone()).expect("sync after removing 1");

    // Документ в индексе ОСТАЛСЯ
    {
        let rdr = reader(&data_dir.join("index"), None).expect("reader");
        let searcher = rdr.searcher();
        let body_field = searcher.schema().get_field("body").expect("body");
        let parser = QueryParser::for_index(searcher.index(), vec![body_field]);
        let q = parser
            .parse_query("общий_уникальный_текст_для_дедупликации_копий")
            .expect("parse");
        let count = searcher
            .search(&q, &tantivy::collector::Count)
            .expect("search");
        assert_eq!(
            count, 1,
            "Текст должен оставаться в индексе, пока жива вторая копия"
        );
    }

    // Удаляем последнюю копию (R1-Б-63)
    fs::remove_file(&copy2).expect("remove copy2");
    sync_all_with_options(&settings, options).expect("sync after removing 2");

    // Документ в индексе УДАЛЁН
    {
        let rdr = reader(&data_dir.join("index"), None).expect("reader");
        let searcher = rdr.searcher();
        let body_field = searcher.schema().get_field("body").expect("body");
        let parser = QueryParser::for_index(searcher.index(), vec![body_field]);
        let q = parser
            .parse_query("общий_уникальный_текст_для_дедупликации_копий")
            .expect("parse");
        let count = searcher
            .search(&q, &tantivy::collector::Count)
            .expect("search");
        assert_eq!(
            count, 0,
            "После удаления последней копии документ должен быть удалён из индекса"
        );
    }
}

/// 5. reindex_from_saved_texts при удалённых исходниках → индекс восстановлен (R1-Б-40).
#[test]
fn test_pipeline_reindex_from_saved_texts_without_reading_files() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    let f1 = source_dir.join("a.txt");
    let f2 = source_dir.join("b.txt");
    fs::write(&f1, "первый_сохраненный_документ_базы").expect("write a");
    fs::write(&f2, "второй_сохраненный_документ_базы").expect("write b");

    let settings = create_test_settings(&data_dir, &source_dir);
    let options = PipelineOptions {
        in_process_workers: true,
        ..Default::default()
    };

    sync_all_with_options(&settings, options.clone()).expect("sync");

    // Удаляем исходные файлы с диска полностью
    fs::remove_dir_all(&source_dir).expect("remove source dir");
    assert!(!source_dir.exists());

    // Очищаем каталог индекса tantivy на диске
    let index_dir = data_dir.join("index");
    fs::remove_dir_all(&index_dir).expect("remove index dir");
    fs::create_dir_all(&index_dir).expect("create empty index dir");

    // Вызываем reindex_from_saved_texts (файлы на диске не читаются!)
    let summary = reindex_from_saved_texts_with_settings(&settings, options).expect("reindex");
    assert_eq!(summary.added, 2);

    // Проверяем, что оба документа восстановились в индексе tantivy
    let rdr = reader(&index_dir, None).expect("reader");
    let searcher = rdr.searcher();
    let body_field = searcher.schema().get_field("body").expect("body");
    let parser = QueryParser::for_index(searcher.index(), vec![body_field]);

    let q1 = parser
        .parse_query("первый_сохраненный_документ_базы")
        .expect("q1");
    assert_eq!(
        searcher
            .search(&q1, &tantivy::collector::Count)
            .expect("search q1"),
        1
    );

    let q2 = parser
        .parse_query("второй_сохраненный_документ_базы")
        .expect("q2");
    assert_eq!(
        searcher
            .search(&q2, &tantivy::collector::Count)
            .expect("search q2"),
        1
    );
}

/// 6. Вторая копия книги 17 млн символов добавляется < 2 с (R1-Б-64).
#[test]
fn test_pipeline_second_copy_of_seventeen_million_book_faster_than_two_seconds() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    // Генерируем текст размером ~17 МБ (17 млн символов)
    let chunk = "Книга толщиной в семнадцать миллионов символов для замера дедупликации индекса. ";
    let repeat_count = 17_000_000 / chunk.len() + 1;
    let big_text = chunk.repeat(repeat_count);

    let book1 = source_dir.join("book1.txt");
    fs::write(&book1, &big_text).expect("write book 1");

    let settings = create_test_settings(&data_dir, &source_dir);
    let options = PipelineOptions {
        in_process_workers: true,
        ..Default::default()
    };

    // Первое добавление книги
    sync_all_with_options(&settings, options.clone()).expect("first sync");

    // Создаём вторую копию с идентичным содержанием
    let book2 = source_dir.join("book2.txt");
    fs::write(&book2, &big_text).expect("write book 2");

    // Замеряем время добавления второй копии
    let start = Instant::now();
    sync_all_with_options(&settings, options).expect("second sync");
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "Вторая копия книги 17 млн символов должна добавляться < 2 с, факт: {:?}",
        elapsed
    );

    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    assert_eq!(catalog.count_files().expect("count"), 2);

    let rdr = reader(&data_dir.join("index"), None).expect("reader");
    assert_eq!(
        rdr.searcher().num_docs(),
        1,
        "В tantivy должен быть ровно 1 документ"
    );
}

/// 7. Файл исчез между обходом и извлечением → запись удалена, без ошибки (R1-Б-44).
#[test]
fn test_pipeline_file_disappeared_during_extraction() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    let stay_file = source_dir.join("stay.txt");
    fs::write(&stay_file, "Нормальный файл").expect("write stay");

    let vanish_file = source_dir.join("vanish.txt");
    fs::write(&vanish_file, "Файл который исчезнет").expect("write vanish");

    let settings = create_test_settings(&data_dir, &source_dir);

    // Удаляем файл перед синхронизацией после подготовки
    fs::remove_file(&vanish_file).expect("remove vanish");

    let options = PipelineOptions {
        in_process_workers: true,
        ..Default::default()
    };

    let summary = sync_all_with_options(&settings, options).expect("sync");
    assert_eq!(summary.added, 1);

    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    assert_eq!(catalog.count_files().expect("count"), 1);
}

/// 8. Отключённый источник → записи и индекс на месте.
#[test]
fn test_pipeline_disconnected_source_preserves_records() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    let f = source_dir.join("doc.txt");
    fs::write(&f, "Документ для проверки отключенного источника").expect("write");

    let mut settings = create_test_settings(&data_dir, &source_dir);
    let options = PipelineOptions {
        in_process_workers: true,
        ..Default::default()
    };

    sync_all_with_options(&settings, options.clone()).expect("initial sync");

    // Симулируем отключение диска: путь к источнику больше не существует
    let disconnected_dir = dir.path().join("non_existent_drive_folder");
    settings.sources[0].path = disconnected_dir;

    let summary = sync_all_with_options(&settings, options).expect("sync with disconnected source");
    assert_eq!(summary.deleted, 0);

    // Записи в каталоге и индекс tantivy сохранены!
    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    assert_eq!(catalog.count_files().expect("count"), 1);

    let rdr = reader(&data_dir.join("index"), None).expect("reader");
    assert_eq!(rdr.searcher().num_docs(), 1);
}

/// 9. Второй index.lock → понятная ошибка.
#[test]
fn test_pipeline_index_lock_conflict() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");

    // Первый захват блокировки
    let lock1 = IndexLock::acquire(&data_dir).expect("first lock");

    // Второй захват в том же каталоге
    let lock2_res = IndexLock::acquire(&data_dir);
    assert!(lock2_res.is_err(), "Второй захват должен возвращать ошибку");

    if let Err(PipelineError::Locked(msg)) = lock2_res {
        assert!(
            msg.contains("База уже обслуживается другим окном ПоисКовичеКа"),
            "Сообщение должно быть понятным: {msg}"
        );
    } else {
        panic!("Ожидалась ошибка PipelineError::Locked");
    }

    drop(lock1);

    // После освобождения первый захват снова успешен
    let lock3 = IndexLock::acquire(&data_dir);
    assert!(lock3.is_ok(), "После освобождения блокировка доступна");
}

/// 10. «Зависший» извлекатель (заглушка) не останавливает конвейер.
#[test]
fn test_pipeline_hanging_extractor_does_not_halt_pipeline() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    let f1 = source_dir.join("fast.txt");
    fs::write(&f1, "Быстрый документ").expect("write f1");

    let mut settings = create_test_settings(&data_dir, &source_dir);
    settings.extract_timeout_seconds = 1; // маленький таймаут

    let options = PipelineOptions {
        in_process_workers: true,
        test_hook: Some("hang_10s".to_string()),
        ..Default::default()
    };

    let start = Instant::now();
    let res = sync_all_with_options(&settings, options);
    let elapsed = start.elapsed();

    assert!(res.is_ok(), "Конвейер должен завершиться успешно");
    // Таймаут сработал и конвейер завершился
    assert!(
        elapsed < Duration::from_secs(30),
        "Конвейер не должен зависать надолго: {:?}",
        elapsed
    );
}

/// Дополнительный тест: работа с реальными рабочими процессами (--worker).
#[test]
fn test_pipeline_with_real_worker_subprocesses() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");

    for i in 0..10 {
        let txt_path = source_dir.join(format!("subprocess_doc_{:02}.txt", i));
        fs::write(txt_path, format!("Документ {:02} через процесс worker", i)).expect("write");
    }

    let settings = create_test_settings(&data_dir, &source_dir);
    let options = PipelineOptions {
        in_process_workers: false, // Настоящие подпроцессы!
        num_workers: 2,
        ..Default::default()
    };

    let summary = sync_all_with_options(&settings, options).expect("sync with subprocesses");
    assert_eq!(summary.added, 10);

    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("catalog");
    assert_eq!(catalog.count_files().expect("count"), 10);

    let rdr = reader(&data_dir.join("index"), None).expect("reader");
    assert_eq!(rdr.searcher().num_docs(), 10);
}

/// 11. Тест-замер #[ignore] perf_index: 20 000 пробных документов (≈ 390 МБ русского текста),
///     печатает МБ/с и документов/с; цель ≥ 25 МБ/с. Предел времени 15 мин.
#[test]
#[ignore]
fn perf_index() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");

    // Генерируется один раз во временной папке, чтобы повторные прогоны не тратили время на диск
    let source_dir = std::env::temp_dir().join("poiskovichek_perf_source_20k");
    fs::create_dir_all(&source_dir).expect("create source_20k");

    let total_docs = 20_000;
    let existing_count = fs::read_dir(&source_dir).map(|r| r.count()).unwrap_or(0);

    if existing_count < total_docs {
        println!("Генерация 20 000 пробных документов (~390 МБ)...");
        let gen_start = Instant::now();

        // 20 000 документов по ~20 000 байт ≈ 390 МБ русского текста
        let sample_paragraph = "В тихом саду за старым бревенчатым домом шелестели сухие золотые листья яблонь и рябин. \
Холодный осенний ветер гнал серые тучи над крышами дачного поселка, предвещая скорый первый снег. \
Архивный поиск по каталогу документов находит каждое слово, словоформу и точную фразу мгновенно. ";

        let repeat_per_file = 20_000 / sample_paragraph.len() + 1;
        let base_text = sample_paragraph.repeat(repeat_per_file);

        use rayon::prelude::*;
        (0..total_docs).into_par_iter().for_each(|i| {
            let p = source_dir.join(format!("doc_{:05}.txt", i));
            if !p.exists() {
                let file_text = format!("Документ {:05}. {}", i, base_text);
                let _ = fs::write(&p, file_text.as_bytes());
            }
        });

        println!(
            "Генерация завершена за {:.2} с",
            gen_start.elapsed().as_secs_f64()
        );
    } else {
        println!(
            "Используется существующая папка с {} документами",
            existing_count
        );
    }

    let total_bytes: u64 = fs::read_dir(&source_dir)
        .expect("read_dir")
        .filter_map(|e| e.ok()?.metadata().ok().map(|m| m.len()))
        .sum();

    let settings = create_test_settings(&data_dir, &source_dir);
    let options = PipelineOptions {
        queue_capacity: 512,
        in_process_workers: true,
        ..Default::default()
    };

    println!("Запуск индексирования конвейером...");
    let start = Instant::now();
    let summary = sync_all_with_options(&settings, options).expect("sync_all");
    let elapsed = start.elapsed();

    let elapsed_secs = elapsed.as_secs_f64();
    let total_mb = total_bytes as f64 / 1_048_576.0;
    let mb_per_sec = total_mb / elapsed_secs;
    let docs_per_sec = total_docs as f64 / elapsed_secs;

    println!("\n================ ЗАМЕР PERF_INDEX ================");
    println!("Всего документов:  {}", total_docs);
    println!("Общий объём:       {:.2} МБ", total_mb);
    println!("Время индексации:  {:.2} с ({:?})", elapsed_secs, elapsed);
    println!("Скорость:          {:.2} МБ/с (цель ≥ 25 МБ/с)", mb_per_sec);
    println!("Темп:              {:.0} документов/с", docs_per_sec);
    println!("Добавлено в базу:  {}", summary.added);
    println!("==================================================\n");

    assert_eq!(summary.added, total_docs);
    assert!(
        mb_per_sec >= 25.0,
        "Скорость индексации ({:.2} МБ/с) ниже целевой планки 25 МБ/с",
        mb_per_sec
    );
}

// ---- Чтение членов ZIP с «неправильными» именами (№217–№219) ----

/// Контрольная сумма CRC-32 (для ручной сборки ZIP).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Минимальный писатель ZIP без сжатия: имена членов записываются сырыми байтами,
/// флаг UTF-8 не ставится (крейт zip сырые имена не принимает).
fn write_raw_zip(path: &Path, entries: &[(&[u8], &[u8])]) {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let (nlen, size) = (name.len() as u16, data.len() as u32);
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes()); // версия
        out.extend_from_slice(&0u16.to_le_bytes()); // флаги: без UTF-8
        out.extend_from_slice(&0u16.to_le_bytes()); // без сжатия
        out.extend_from_slice(&0u16.to_le_bytes()); // время
        out.extend_from_slice(&0x0021u16.to_le_bytes()); // дата 1980-01-01
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&nlen.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0x0021u16.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&nlen.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // комментарий
        central.extend_from_slice(&0u16.to_le_bytes()); // диск
        central.extend_from_slice(&0u16.to_le_bytes()); // внутренние атрибуты
        central.extend_from_slice(&0u32.to_le_bytes()); // внешние атрибуты
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&central_size.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    fs::write(path, out).expect("write raw zip");
}

/// Сколько документов индекса содержат слово.
fn index_hits(data_dir: &Path, word: &str) -> usize {
    let rdr = reader(&data_dir.join("index"), None).expect("reader");
    let searcher = rdr.searcher();
    let body_field = searcher.schema().get_field("body").expect("body");
    let parser = QueryParser::for_index(searcher.index(), vec![body_field]);
    let q = parser.parse_query(word).expect("query");
    searcher
        .search(&q, &tantivy::collector::Count)
        .expect("search")
}

/// Члены архивов в базе: (имя, статус, ошибка).
fn archive_members(data_dir: &Path) -> Vec<(String, Option<String>, Option<String>)> {
    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    let mut stmt = catalog
        .connection()
        .prepare(
            "SELECT name, content_status, error FROM files WHERE inside_archive = 1 ORDER BY name;",
        )
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
}

fn archive_test_dirs(dir: &Path) -> (std::path::PathBuf, Settings) {
    let source_dir = dir.join("source");
    fs::create_dir_all(&source_dir).expect("create source dir");
    let settings = create_test_settings(&dir.join("data"), &source_dir);
    (source_dir, settings)
}

fn in_process() -> PipelineOptions {
    PipelineOptions {
        in_process_workers: true,
        ..Default::default()
    }
}

/// №217: имена членов с обратной чертой (в том числе с ведущей) читаются.
#[test]
fn test_pipeline_zip_members_with_backslash_names_are_read() {
    let dir = tempdir().expect("tempdir");
    let (source_dir, settings) = archive_test_dirs(dir.path());
    write_raw_zip(
        &source_dir.join("обратные.zip"),
        &[
            (
                "тов.чек\\отчёт.txt".as_bytes(),
                "слово_обратнаячерта_один".as_bytes(),
            ),
            (
                "\\Shared\\info.txt".as_bytes(),
                "слово_ведущаячерта_два".as_bytes(),
            ),
        ],
    );

    sync_all_with_options(&settings, in_process()).expect("sync");

    assert_eq!(
        index_hits(&settings.data_dir, "слово_обратнаячерта_один"),
        1
    );
    assert_eq!(index_hits(&settings.data_dir, "слово_ведущаячерта_два"), 1);
    let members = archive_members(&settings.data_dir);
    assert_eq!(members.len(), 2, "{members:?}");
    for (name, status, error) in members {
        assert!(error.is_none(), "{name}: ошибка {error:?}");
        assert_eq!(status.as_deref(), Some("text"), "{name}");
    }
}

/// №217: имя члена в CP866 без флага UTF-8 читается.
#[test]
fn test_pipeline_zip_member_with_cp866_name_is_read() {
    let dir = tempdir().expect("tempdir");
    let (source_dir, settings) = archive_test_dirs(dir.path());
    let (name866, _, _) = encoding_rs::IBM866.encode("Прайс\\Цены.txt");
    write_raw_zip(
        &source_dir.join("прайсы.zip"),
        &[(&name866, "слово_кириллица_866".as_bytes())],
    );

    sync_all_with_options(&settings, in_process()).expect("sync");

    assert_eq!(index_hits(&settings.data_dir, "слово_кириллица_866"), 1);
    let members = archive_members(&settings.data_dir);
    assert_eq!(members.len(), 1, "{members:?}");
    assert_eq!(members[0].0, "Цены.txt");
    assert!(members[0].2.is_none(), "ошибка: {:?}", members[0].2);
}

/// №218: члена нет в архиве — в базе настоящая причина, а не «Отсутствует источник».
#[test]
fn test_reextract_missing_zip_member_records_real_reason() {
    let dir = tempdir().expect("tempdir");
    let (source_dir, settings) = archive_test_dirs(dir.path());
    write_raw_zip(
        &source_dir.join("a.zip"),
        &[("есть.txt".as_bytes(), "слово_есть".as_bytes())],
    );
    sync_all_with_options(&settings, in_process()).expect("sync");

    // Запись члена ссылается на имя, которого в архиве нет; версия извлечения устарела
    {
        let catalog =
            Catalog::open(&settings.data_dir.join("catalog.sqlite3")).expect("open catalog");
        let n = catalog
            .connection()
            .execute(
                "UPDATE files SET path = replace(path, 'есть.txt', 'нет.txt'), \
                 extraction_version = 0, error = NULL WHERE inside_archive = 1;",
                [],
            )
            .expect("update");
        assert_eq!(n, 1);
    }

    reextract_stale_with_settings(&settings, in_process()).expect("reextract");

    let members = archive_members(&settings.data_dir);
    assert_eq!(members.len(), 1, "{members:?}");
    assert_eq!(members[0].1.as_deref(), Some("failed"));
    let error = members[0].2.clone().expect("ошибка записана");
    assert!(error.contains("Файл из архива не прочитан"), "{error}");
    assert!(error.contains("нет.txt"), "{error}");
    assert!(!error.contains("Отсутствует источник"), "{error}");
}

// ---- Источники, тексты при сбое, удаление члена архива (№221, №222, №226) ----

fn source_of(path: &Path) -> Source {
    Source {
        path: path.to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    }
}

/// Файлы базы: (путь, source_id, volume_id).
fn file_links(data_dir: &Path) -> Vec<(String, Option<i64>, Option<i64>)> {
    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    let mut stmt = catalog
        .connection()
        .prepare("SELECT path, source_id, volume_id FROM files ORDER BY path;")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
}

/// Проверяет, что каждый файл привязан к источнику с самым длинным совпадающим путём
/// и к тому же тому, что записан у источника.
fn check_links(data_dir: &Path, sources: &[Source], expected_files: usize) {
    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
    let registered = catalog.list_sources().expect("list_sources");
    assert_eq!(registered.len(), sources.len(), "{registered:?}");
    for src in sources {
        let text = src.path.to_string_lossy().to_string();
        let rec = registered
            .iter()
            .find(|r| r.path == text)
            .unwrap_or_else(|| panic!("источник {text} не записан: {registered:?}"));
        assert!(rec.id.is_some());
        assert!(rec.volume_id.is_some(), "у источника {text} нет тома");
        assert_eq!(rec.status, "ready", "{text}");
    }

    let files = file_links(data_dir);
    assert_eq!(files.len(), expected_files, "{files:?}");
    for (path, source_id, volume_id) in files {
        let owner = registered
            .iter()
            .filter(|r| path.starts_with(&format!("{}\\", r.path)))
            .max_by_key(|r| r.path.len())
            .unwrap_or_else(|| panic!("файл {path} вне источников"));
        assert_eq!(source_id, owner.id, "source_id файла {path}");
        assert!(volume_id.is_some(), "у файла {path} нет тома");
        assert_eq!(volume_id, owner.volume_id, "volume_id файла {path}");
    }
}

/// №221: два источника и вложенный третий — каждый файл (и член архива) получает свой
/// source_id и volume_id; `list_sources` отдаёт путь источника так же, как в настройках.
#[test]
fn test_pipeline_files_get_source_and_volume() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("source");
    let (a, b) = (root.join("a"), root.join("b"));
    let deep = a.join("deep");
    fs::create_dir_all(&deep).expect("mkdir deep");
    fs::create_dir_all(&b).expect("mkdir b");
    fs::write(a.join("1.txt"), "слово_источник_один").expect("write");
    fs::write(deep.join("2.txt"), "слово_источник_два").expect("write");
    fs::write(b.join("3.txt"), "слово_источник_три").expect("write");
    write_raw_zip(
        &b.join("z.zip"),
        &[("в.txt".as_bytes(), "слово_источник_архив".as_bytes())],
    );

    let settings = create_test_settings(&dir.path().join("data"), &root);
    let sources = vec![source_of(&a), source_of(&b), source_of(&deep)];
    index_sources_with_options(&settings, &sources, in_process()).expect("index");

    check_links(&settings.data_dir, &sources, 5);
}

/// №221: база с файлами без source_id/volume_id после повторной сверки их получает,
/// хотя сами файлы не изменились и не перечитываются.
#[test]
fn test_pipeline_resync_assigns_source_to_unlinked_files() {
    let dir = tempdir().expect("tempdir");
    let (source_dir, settings) = archive_test_dirs(dir.path());
    fs::write(source_dir.join("a.txt"), "слово_привязка_один").expect("write");
    fs::create_dir_all(source_dir.join("sub")).expect("mkdir");
    fs::write(source_dir.join("sub").join("b.txt"), "слово_привязка_два").expect("write");
    let sources = vec![source_of(&source_dir)];
    index_sources_with_options(&settings, &sources, in_process()).expect("first index");
    check_links(&settings.data_dir, &sources, 2);

    // Состояние старой базы: у всех файлов привязка пуста, источников в таблице нет
    {
        let catalog =
            Catalog::open(&settings.data_dir.join("catalog.sqlite3")).expect("open catalog");
        catalog
            .connection()
            .execute_batch(
                "PRAGMA foreign_keys = OFF; \
                 UPDATE files SET source_id = NULL, volume_id = NULL; \
                 DELETE FROM sources;",
            )
            .expect("reset links");
    }
    assert!(file_links(&settings.data_dir).iter().all(|f| f.1.is_none()));

    let summary =
        index_sources_with_options(&settings, &sources, in_process()).expect("second index");
    assert_eq!(summary.added + summary.changed, 0, "файлы не менялись");
    check_links(&settings.data_dir, &sources, 2);
}

/// №222: разбор изменённого файла упал — прежний текст в `texts` остаётся, text_sha прежний,
/// ошибка записана; следующая успешная сверка заменяет текст.
#[test]
fn test_pipeline_failed_extraction_keeps_previous_text() {
    let dir = tempdir().expect("tempdir");
    let (source_dir, settings) = archive_test_dirs(dir.path());
    let file = source_dir.join("doc.txt");
    fs::write(&file, "слово_прежний_текст_222").expect("write");
    sync_all_with_options(&settings, in_process()).expect("first sync");

    let state = |data_dir: &Path| -> (Option<String>, Option<String>, Option<String>, i64) {
        let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("open catalog");
        let (sha, status, error) = catalog
            .connection()
            .query_row(
                "SELECT text_sha, content_status, error FROM files WHERE name = 'doc.txt';",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("file row");
        let texts: i64 = catalog
            .connection()
            .query_row("SELECT COUNT(*) FROM texts;", [], |r| r.get(0))
            .expect("texts");
        (sha, status, error, texts)
    };
    let (sha_before, status_before, _, texts_before) = state(&settings.data_dir);
    assert!(sha_before.is_some());
    assert_eq!(status_before.as_deref(), Some("text"));
    assert_eq!(texts_before, 1);

    // Файл изменился, а разбор падает
    std::thread::sleep(Duration::from_millis(50));
    fs::write(&file, "слово_новый_текст_222_длиннее").expect("overwrite");
    let failing = PipelineOptions {
        in_process_workers: true,
        test_hook: Some("abort".to_string()),
        ..Default::default()
    };
    sync_all_with_options(&settings, failing).expect("sync with failure");

    let (sha, status, error, texts) = state(&settings.data_dir);
    assert_eq!(sha, sha_before, "text_sha прежнего текста должен остаться");
    assert_eq!(texts, 1, "прежний текст из texts не удалён");
    assert_eq!(status.as_deref(), Some("failed"));
    assert!(error.is_some(), "ошибка должна быть записана");
    assert_eq!(index_hits(&settings.data_dir, "слово_прежний_текст_222"), 1);

    // Следующая сверка без сбоя читает файл заново и заменяет текст
    sync_all_with_options(&settings, in_process()).expect("sync after failure");
    let (sha_after, status_after, error_after, texts_after) = state(&settings.data_dir);
    assert_ne!(sha_after, sha_before);
    assert!(sha_after.is_some());
    assert_eq!(status_after.as_deref(), Some("text"));
    assert!(error_after.is_none());
    assert_eq!(texts_after, 1);
    assert_eq!(index_hits(&settings.data_dir, "слово_прежний_текст_222"), 0);
    assert_eq!(
        index_hits(&settings.data_dir, "слово_новый_текст_222_длиннее"),
        1
    );
}

/// №226: путь члена архива «архив → папка\файл» удаляет только эту запись и её текст;
/// путь архива удаляет архив вместе с оставшимися членами.
#[test]
fn test_remove_paths_archive_member_removes_only_that_record() {
    let dir = tempdir().expect("tempdir");
    let (source_dir, settings) = archive_test_dirs(dir.path());
    fs::write(source_dir.join("сосед.txt"), "слово_сосед_226").expect("write");
    let zip_path = source_dir.join("набор.zip");
    write_raw_zip(
        &zip_path,
        &[
            ("папка\\один.txt".as_bytes(), "слово_один_226".as_bytes()),
            ("папка\\два.txt".as_bytes(), "слово_два_226".as_bytes()),
        ],
    );
    sync_all_with_options(&settings, in_process()).expect("sync");
    assert_eq!(file_links(&settings.data_dir).len(), 4);
    {
        // Члены архива связаны с архивом, даже если записаны одной порцией с ним
        let catalog =
            Catalog::open(&settings.data_dir.join("catalog.sqlite3")).expect("open catalog");
        let without_container: i64 = catalog
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM files WHERE inside_archive = 1 AND container_id IS NULL;",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(without_container, 0, "члены архива без container_id");
    }

    let member_path = file_links(&settings.data_dir)
        .into_iter()
        .map(|f| f.0)
        .find(|p| p.ends_with("один.txt"))
        .expect("член архива в базе");
    assert!(
        member_path.contains(poiskovichek::rules::ARCHIVE_SEPARATOR),
        "{member_path}"
    );

    let summary = remove_paths_with_settings(&settings, &[PathBuf::from(&member_path)])
        .expect("удаление члена");
    assert_eq!(summary.deleted, 1);
    let rest: Vec<String> = file_links(&settings.data_dir)
        .into_iter()
        .map(|f| f.0)
        .collect();
    assert_eq!(rest.len(), 3, "{rest:?}");
    assert!(!rest.contains(&member_path));
    assert!(rest.iter().any(|p| p.ends_with("набор.zip")));
    assert!(rest.iter().any(|p| p.ends_with("два.txt")));
    assert_eq!(index_hits(&settings.data_dir, "слово_один_226"), 0);
    assert_eq!(index_hits(&settings.data_dir, "слово_два_226"), 1);

    // Архив не менялся — удалённый член сверкой не возвращается
    sync_all_with_options(&settings, in_process()).expect("resync");
    let after = file_links(&settings.data_dir);
    assert_eq!(after.len(), 3, "{after:?}");

    // Путь архива — удаляется архив и оставшийся член
    let summary = remove_paths_with_settings(&settings, std::slice::from_ref(&zip_path))
        .expect("удаление архива");
    assert_eq!(summary.deleted, 2);
    let rest = file_links(&settings.data_dir);
    assert_eq!(rest.len(), 1, "{rest:?}");
    assert!(rest[0].0.ends_with("сосед.txt"));
    assert_eq!(index_hits(&settings.data_dir, "слово_два_226"), 0);
    assert_eq!(index_hits(&settings.data_dir, "слово_сосед_226"), 1);
}

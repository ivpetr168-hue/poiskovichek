use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::tempdir;

use poiskovichek::catalog::{Catalog, FileRecord};
use poiskovichek::duplicates::{
    DuplicateError, DuplicateGroup, DuplicateOptions, DuplicateProgress, DuplicateStage,
    QUICK_HASH_CHUNK, find_duplicates, find_duplicates_simple, find_duplicates_with_options,
    find_duplicates_with_stats,
};
use poiskovichek::pipeline::CancelToken;

fn make_record(id: i64, path: &Path, size: u64, mtime_ns: i64, inside_archive: bool) -> FileRecord {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("file")
        .to_string();
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();

    FileRecord {
        id: Some(id),
        path: path.to_string_lossy().to_string(),
        source_id: None,
        volume_id: None,
        rel_path: name.clone(),
        name,
        extension: ext,
        size,
        modified_ns: mtime_ns,
        created_ns: mtime_ns,
        category: "other".to_string(),
        inside_archive,
        container_id: None,
        content_status: None,
        extraction_version: 0,
        text_sha: None,
        quick_hash: None,
        sha256: None,
        error: None,
        scan_run: Some(1),
        available: true,
    }
}

fn file_mtime_ns(path: &Path) -> i64 {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[test]
fn test_duplicates_require_identical_content() {
    // R1-Б-13: два файла с одинаковым содержимым same-content и третий с other-content
    // -> 1 группа дубликатов из 2 файлов с точным reclaimable_bytes = len(b"same-content")
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("files");
    fs::create_dir_all(&root).expect("create_dir");

    let a_path = root.join("a.bin");
    let b_path = root.join("b.bin");
    let c_path = root.join("c.bin");

    let content_same = b"same-content";
    let content_other = b"other-content"; // как в test_duplicates_require_identical_content в Python

    fs::write(&a_path, content_same).unwrap();
    fs::write(&b_path, content_same).unwrap();
    fs::write(&c_path, content_other).unwrap();

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let rec_a = make_record(
        1,
        &a_path,
        content_same.len() as u64,
        file_mtime_ns(&a_path),
        false,
    );
    let rec_b = make_record(
        2,
        &b_path,
        content_same.len() as u64,
        file_mtime_ns(&b_path),
        false,
    );
    let rec_c = make_record(
        3,
        &c_path,
        content_other.len() as u64,
        file_mtime_ns(&c_path),
        false,
    );

    catalog.upsert_files(&[rec_a, rec_b, rec_c]).unwrap();
    drop(catalog);

    let groups = find_duplicates_simple(&db_path).unwrap();

    assert_eq!(groups.len(), 1, "Должна быть ровно 1 группа дубликатов");
    assert_eq!(groups[0].file_count, 2, "В группе должно быть 2 файла");
    assert_eq!(
        groups[0].total_bytes,
        (content_same.len() * 2) as u64,
        "Общий объем равен сумме размеров двух копий"
    );
    assert_eq!(
        groups[0].reclaimable_bytes,
        content_same.len() as u64,
        "Высвобождаемый объем равен размеру одной лишней копии (SUM - MAX)"
    );
}

#[test]
fn test_archive_members_are_excluded_from_duplicates() {
    // R1-Б-26: одинаковый контент в обычном файле и внутри архива (inside_archive=1)
    // -> в дубликаты не входят, группа дубликатов пуста
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("files");
    fs::create_dir_all(&root).expect("create_dir");

    let ord_path = root.join("ordinary.bin");
    let content = b"same-content";
    fs::write(&ord_path, content).unwrap();

    let member_path = root.join("archive.zip -> copy.txt");

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let rec_ord = make_record(
        1,
        &ord_path,
        content.len() as u64,
        file_mtime_ns(&ord_path),
        false,
    );
    let rec_member = make_record(
        2,
        &member_path,
        content.len() as u64,
        file_mtime_ns(&ord_path),
        true,
    );

    catalog.upsert_files(&[rec_ord, rec_member]).unwrap();
    drop(catalog);

    let groups = find_duplicates_simple(&db_path).unwrap();
    assert!(
        groups.is_empty(),
        "Внутренние файлы архивов не должны попадать в дубликаты"
    );
}

#[test]
fn test_same_size_different_content_are_not_duplicates() {
    // Одинаковый размер, разное содержимое -> не дубликаты
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("files");
    fs::create_dir_all(&root).expect("create_dir");

    let p1 = root.join("f1.bin");
    let p2 = root.join("f2.bin");

    fs::write(&p1, b"AAAA1111").unwrap();
    fs::write(&p2, b"BBBB2222").unwrap();

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let r1 = make_record(1, &p1, 8, file_mtime_ns(&p1), false);
    let r2 = make_record(2, &p2, 8, file_mtime_ns(&p2), false);

    catalog.upsert_files(&[r1, r2]).unwrap();
    drop(catalog);

    let groups = find_duplicates_simple(&db_path).unwrap();
    assert!(
        groups.is_empty(),
        "Файлы с разным содержимым не должны быть дубликатами"
    );
}

#[test]
fn test_large_files_difference_in_middle_not_duplicates_by_full_hash() {
    // Файл 1 МБ, отличие в середине (быстрый хэш совпал) -> не дубликаты по полному хэшу
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("files");
    fs::create_dir_all(&root).expect("create_dir");

    let file_size: usize = 1024 * 1024; // 1 МБ (> 256 КБ)
    let chunk_size = QUICK_HASH_CHUNK as usize; // 128 КБ

    let mut data1 = vec![0xAAu8; file_size];
    let mut data2 = vec![0xAAu8; file_size];

    // Первые 128 КБ и последние 128 КБ идентичны
    // В середине (например, на позиции 300 КБ) делаем отличие:
    let mid_pos = 300 * 1024;
    assert!(mid_pos > chunk_size && mid_pos < file_size - chunk_size);

    data1[mid_pos] = 0x11;
    data2[mid_pos] = 0x22;

    let p1 = root.join("large1.bin");
    let p2 = root.join("large2.bin");

    fs::write(&p1, &data1).unwrap();
    fs::write(&p2, &data2).unwrap();

    // Проверяем, что их быстрый хэш действительно совпадает
    let qh1 = poiskovichek::duplicates::compute_quick_hash(&p1, file_size as u64).unwrap();
    let qh2 = poiskovichek::duplicates::compute_quick_hash(&p2, file_size as u64).unwrap();
    assert_eq!(
        qh1, qh2,
        "Быстрый хэш должен совпасть, так как начало и хвост одинаковы"
    );

    // Проверяем, что полный SHA-256 различается
    let full1 = poiskovichek::duplicates::compute_full_hash(&p1).unwrap();
    let full2 = poiskovichek::duplicates::compute_full_hash(&p2).unwrap();
    assert_ne!(full1, full2, "Полный SHA-256 должен различаться");

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let r1 = make_record(1, &p1, file_size as u64, file_mtime_ns(&p1), false);
    let r2 = make_record(2, &p2, file_size as u64, file_mtime_ns(&p2), false);

    catalog.upsert_files(&[r1, r2]).unwrap();
    drop(catalog);

    let groups = find_duplicates_simple(&db_path).unwrap();
    assert!(
        groups.is_empty(),
        "При различии в середине файлы не должны попасть в группу дубликатов по полному хэшу"
    );
}

#[test]
fn test_repeat_duplicate_search_does_not_read_files_when_unchanged() {
    // Повторный поиск дубликатов без изменений файлов не читает файлы (счётчик чтений 0)
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("files");
    fs::create_dir_all(&root).expect("create_dir");

    let p1 = root.join("dup1.bin");
    let p2 = root.join("dup2.bin");

    let content = vec![0x55u8; 300 * 1024]; // 300 КБ (> 256 КБ)
    fs::write(&p1, &content).unwrap();
    fs::write(&p2, &content).unwrap();

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let r1 = make_record(1, &p1, content.len() as u64, file_mtime_ns(&p1), false);
    let r2 = make_record(2, &p2, content.len() as u64, file_mtime_ns(&p2), false);

    catalog.upsert_files(&[r1, r2]).unwrap();
    drop(catalog);

    // Первый проход: вычисляет хэши и читает файлы с диска
    let (groups1, stats1) =
        find_duplicates_with_stats(&db_path, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(groups1.len(), 1);
    assert!(
        stats1.files_read > 0,
        "В первый раз файлы должны быть прочитаны: {}",
        stats1.files_read
    );

    // Второй проход: файлы не менялись, их quick_hash и sha256 уже в базе
    let (groups2, stats2) =
        find_duplicates_with_stats(&db_path, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(groups2.len(), 1);
    assert_eq!(
        stats2.files_read, 0,
        "Повторный поиск дубликатов без изменений файлов не должен читать файлы с диска: {}",
        stats2.files_read
    );
}

#[test]
fn test_duplicate_search_cancellation() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("catalog.sqlite3");
    let _ = Catalog::open_or_create(&db_path).unwrap();

    let cancel = CancelToken::new();
    cancel.cancel(); // Сразу отменяем

    let res = find_duplicates(&db_path, &cancel, |_| {});
    assert!(res.is_err(), "Отмененный поиск должен возвращать ошибку");
}

#[test]
fn test_single_file_read_error_is_skipped_not_failed() {
    // Ошибка чтения одного файла — пропуск, не провал
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("files");
    fs::create_dir_all(&root).expect("create_dir");

    let p1 = root.join("valid1.bin");
    let p2 = root.join("valid2.bin");
    let p3 = root.join("missing.bin");

    let content = b"duplicate-content";
    fs::write(&p1, content).unwrap();
    fs::write(&p2, content).unwrap();
    // p3 намеренно не создаем на диске

    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let r1 = make_record(1, &p1, content.len() as u64, file_mtime_ns(&p1), false);
    let r2 = make_record(2, &p2, content.len() as u64, file_mtime_ns(&p2), false);
    let r3 = make_record(3, &p3, content.len() as u64, 0, false);

    catalog.upsert_files(&[r1, r2, r3]).unwrap();
    drop(catalog);

    // Должно успешно отработать, пропустив p3
    let groups = find_duplicates_simple(&db_path).unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].file_count, 2);
}

// ---------------------------------------------------------------------------
// №238: ход работы, сохранение по частям, пересчёт изменённого файла
// ---------------------------------------------------------------------------

const SET_FILE_SIZE: usize = 300 * 1024; // > 256 КБ: у файлов есть и быстрый, и полный отпечаток

/// Каталог из `pairs` пар одинаковых файлов одного размера (пары между собой различаются).
/// Возвращает путь к базе и пути файлов в порядке id (1..=2*pairs).
fn make_pairs_catalog(dir: &Path, pairs: usize) -> (PathBuf, Vec<PathBuf>) {
    let root = dir.join("files");
    fs::create_dir_all(&root).unwrap();
    let db_path = dir.join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let mut paths = Vec::new();
    let mut records = Vec::new();
    for pair in 0..pairs {
        let mut content = vec![0x33u8; SET_FILE_SIZE];
        content[..4].copy_from_slice(&(pair as u32).to_le_bytes());
        for copy in 0..2 {
            let path = root.join(format!("p{pair:03}_{copy}.bin"));
            fs::write(&path, &content).unwrap();
            let id = (paths.len() + 1) as i64;
            records.push(make_record(
                id,
                &path,
                SET_FILE_SIZE as u64,
                file_mtime_ns(&path),
                false,
            ));
            paths.push(path);
        }
    }
    catalog.upsert_files(&records).unwrap();
    (db_path, paths)
}

fn count_hashes(db_path: &Path) -> (usize, usize) {
    let conn = rusqlite::Connection::open(db_path).unwrap();
    let quick: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE quick_hash IS NOT NULL;",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let sha: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE sha256 IS NOT NULL;",
            [],
            |r| r.get(0),
        )
        .unwrap();
    (quick as usize, sha as usize)
}

/// Настройки для тестов: сообщать о каждом результате, очередь короткая: после отмены
/// рабочие потоки не успевают убежать далеко вперёд.
fn test_options() -> DuplicateOptions {
    DuplicateOptions {
        progress_interval: Duration::ZERO,
        queue_len: 4,
        ..DuplicateOptions::default()
    }
}

fn sorted_shas(groups: &[DuplicateGroup]) -> Vec<String> {
    let mut v: Vec<String> = groups.iter().map(|g| g.sha256.clone()).collect();
    v.sort();
    v
}

#[test]
fn test_duplicate_progress_counts_files_and_never_decreases() {
    let dir = tempdir().expect("tempdir");
    let (db_path, paths) = make_pairs_catalog(dir.path(), 20);
    let total = paths.len(); // 40 файлов

    let mut seen: Vec<DuplicateProgress> = Vec::new();
    let (groups, _) =
        find_duplicates_with_options(&db_path, &CancelToken::new(), &test_options(), |p| {
            seen.push(p)
        })
        .unwrap();
    assert_eq!(groups.len(), 20);

    // Этапы идут по порядку: быстрые, полные, сборка
    let stage_order = |s: DuplicateStage| match s {
        DuplicateStage::QuickHash => 0,
        DuplicateStage::FullHash => 1,
        DuplicateStage::Finishing => 2,
    };
    for w in seen.windows(2) {
        assert!(stage_order(w[0].stage) <= stage_order(w[1].stage));
        if w[0].stage == w[1].stage {
            assert!(w[0].files_done <= w[1].files_done, "файлов не убывает");
            assert!(w[0].bytes_done <= w[1].bytes_done, "байтов не убывает");
        }
    }

    let quick: Vec<_> = seen
        .iter()
        .filter(|p| p.stage == DuplicateStage::QuickHash)
        .collect();
    assert_eq!(quick.first().unwrap().files_done, 0);
    assert!(quick.iter().all(|p| p.files_total == total));
    assert_eq!(quick.last().unwrap().files_done, total);
    assert!(
        quick
            .iter()
            .any(|p| p.files_done > 0 && p.files_done < total),
        "должны быть промежуточные значения"
    );

    let full: Vec<_> = seen
        .iter()
        .filter(|p| p.stage == DuplicateStage::FullHash)
        .collect();
    assert!(full.iter().all(|p| p.files_total == total));
    assert_eq!(full.last().unwrap().files_done, total);
    assert!(
        full.iter()
            .any(|p| p.files_done > 0 && p.files_done < total),
        "промежуточные значения полных отпечатков"
    );
    let last_full = full.last().unwrap();
    assert_eq!(last_full.bytes_total, (total * SET_FILE_SIZE) as u64);
    assert_eq!(last_full.bytes_done, last_full.bytes_total);
    assert_eq!(seen.last().unwrap().stage, DuplicateStage::Finishing);
}

#[test]
fn test_duplicate_progress_is_throttled_by_time() {
    // Со стандартным интервалом на быстрой работе сообщений мало: старт, конец этапов, сборка
    let dir = tempdir().expect("tempdir");
    let (db_path, paths) = make_pairs_catalog(dir.path(), 20);
    let mut count = 0usize;
    let mut last_quick = 0usize;
    find_duplicates_with_stats(&db_path, &CancelToken::new(), |p| {
        count += 1;
        if p.stage == DuplicateStage::QuickHash {
            last_quick = p.files_done;
        }
    })
    .unwrap();
    assert_eq!(last_quick, paths.len());
    assert!(
        count < paths.len(),
        "сообщений не по одному на файл: {count}"
    );
}

#[test]
fn test_duplicates_cancel_in_quick_stage_saves_and_resumes() {
    let dir = tempdir().expect("tempdir");
    let (db_path, paths) = make_pairs_catalog(dir.path(), 20);
    let total = paths.len();

    let cancel = CancelToken::new();
    let cancel_in_cb = cancel.clone();
    let res = find_duplicates_with_options(&db_path, &cancel, &test_options(), |p| {
        if p.stage == DuplicateStage::QuickHash && p.files_done >= 10 {
            cancel_in_cb.cancel();
        }
    });
    assert!(matches!(res, Err(DuplicateError::Cancelled)));

    let (saved_quick, saved_sha) = count_hashes(&db_path);
    assert!(
        saved_quick >= 10 && saved_quick < total,
        "в базе часть быстрых отпечатков: {saved_quick} из {total}"
    );
    assert_eq!(saved_sha, 0);

    // Повтор читает только оставшиеся: быстрые отпечатки недостающих + полные всех
    let (groups, stats) =
        find_duplicates_with_stats(&db_path, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(stats.files_read, (total - saved_quick) + total);
    assert_eq!(groups.len(), 20);

    // Тот же результат, что у непрерывного прогона
    let dir2 = tempdir().expect("tempdir");
    let (db2, _) = make_pairs_catalog(dir2.path(), 20);
    let (groups_ref, stats_ref) =
        find_duplicates_with_stats(&db2, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(stats_ref.files_read, total * 2);
    assert_eq!(sorted_shas(&groups), sorted_shas(&groups_ref));
    assert_eq!(count_hashes(&db_path), count_hashes(&db2));
}

#[test]
fn test_duplicates_cancel_in_full_stage_saves_and_resumes() {
    let dir = tempdir().expect("tempdir");
    let (db_path, paths) = make_pairs_catalog(dir.path(), 20);
    let total = paths.len();

    let cancel = CancelToken::new();
    let cancel_in_cb = cancel.clone();
    let res = find_duplicates_with_options(&db_path, &cancel, &test_options(), |p| {
        if p.stage == DuplicateStage::FullHash && p.files_done >= 5 {
            cancel_in_cb.cancel();
        }
    });
    assert!(matches!(res, Err(DuplicateError::Cancelled)));

    let (saved_quick, saved_sha) = count_hashes(&db_path);
    assert_eq!(saved_quick, total, "все быстрые отпечатки уже сохранены");
    assert!(
        saved_sha >= 5 && saved_sha < total,
        "в базе часть полных отпечатков: {saved_sha} из {total}"
    );

    let (groups, stats) =
        find_duplicates_with_stats(&db_path, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(
        stats.files_read,
        total - saved_sha,
        "читаются только непосчитанные"
    );
    assert_eq!(groups.len(), 20);
    assert_eq!(count_hashes(&db_path), (total, total));
}

#[test]
fn test_duplicates_saved_in_portions_during_run() {
    // Порция в 8 файлов: запись идёт во время работы, а не в конце
    let dir = tempdir().expect("tempdir");
    let (db_path, paths) = make_pairs_catalog(dir.path(), 20);
    let total = paths.len();
    let options = DuplicateOptions {
        flush_files: 8,
        ..test_options()
    };

    let mut max_saved_midway = 0usize;
    let db_for_cb = db_path.clone();
    find_duplicates_with_options(&db_path, &CancelToken::new(), &options, |p| {
        if p.stage == DuplicateStage::QuickHash && p.files_done < total {
            max_saved_midway = max_saved_midway.max(count_hashes(&db_for_cb).0);
        }
    })
    .unwrap();
    assert!(
        max_saved_midway >= 8,
        "до конца этапа в базе уже есть отпечатки: {max_saved_midway}"
    );
}

#[test]
fn test_changed_file_is_rehashed_and_stale_hash_is_dropped() {
    let dir = tempdir().expect("tempdir");
    let (db_path, paths) = make_pairs_catalog(dir.path(), 2); // 4 файла, 2 группы

    let (groups1, stats1) =
        find_duplicates_with_stats(&db_path, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(groups1.len(), 2);
    assert_eq!(stats1.files_read, 8);

    // Без изменений ничего не читается
    let (_, stats_same) =
        find_duplicates_with_stats(&db_path, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(stats_same.files_read, 0);

    // Меняем содержимое одного файла (размер тот же) и сдвигаем время изменения
    let changed = &paths[0];
    let mut content = fs::read(changed).unwrap();
    content[0] ^= 0xFF;
    fs::write(changed, &content).unwrap();
    let file = fs::OpenOptions::new().write(true).open(changed).unwrap();
    file.set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    drop(file);

    let (groups2, stats2) =
        find_duplicates_with_stats(&db_path, &CancelToken::new(), |_| {}).unwrap();
    assert_eq!(stats2.files_read, 1, "пересчитан только изменённый файл");
    assert_eq!(
        groups2.len(),
        1,
        "старый полный отпечаток изменённого файла не должен давать ложную группу"
    );

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let sha: Option<String> = conn
        .query_row("SELECT sha256 FROM files WHERE id = 1;", [], |r| r.get(0))
        .unwrap();
    assert!(sha.is_none(), "полный отпечаток изменённого файла сброшен");
}

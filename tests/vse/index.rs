use std::path::Path;
use tempfile::tempdir;

use poiskovichek::index::{
    TOKENIZER_NAME, WRITER_MEMORY_BUDGET_BYTES, create_in_ram, default_writer_threads,
    open_or_create, open_or_create_with_options, reader,
};
use poiskovichek::morph::Morph;
use tantivy::Term;
use tantivy::collector::TopDocs;
use tantivy::query::TermQuery;
use tantivy::schema::{FieldType, IndexRecordOption};

#[test]
fn test_index_schema_and_fields() {
    let handle = create_in_ram(None).expect("create ram index");
    let schema = handle.index().schema();

    let sha_field = schema.get_field("sha").expect("sha field");
    let body_field = schema.get_field("body").expect("body field");

    let sha_entry = schema.get_field_entry(sha_field);
    let body_entry = schema.get_field_entry(body_field);

    assert!(sha_entry.is_stored(), "sha поле должно быть хранимым");
    assert!(!body_entry.is_stored(), "body поле НЕ должно храниться");

    if let FieldType::Str(options) = body_entry.field_type() {
        let body_indexing = options.get_indexing_options().expect("body indexing");
        assert_eq!(body_indexing.tokenizer(), TOKENIZER_NAME);
        assert_eq!(
            body_indexing.index_option(),
            IndexRecordOption::WithFreqsAndPositions
        );
    } else {
        panic!("body field type must be Str");
    }
}

#[test]
fn test_index_add_delete_and_search() {
    let mut handle = create_in_ram(None).expect("create ram index");

    // Добавляем 2 документа
    handle
        .add("sha1", "Сахалинский архив поисковых документов")
        .expect("add doc 1");
    handle
        .add("sha2", "Быстрый поиск текстов на компьютере")
        .expect("add doc 2");
    handle.commit().expect("commit");

    let rdr = handle.reader().expect("reader");
    let searcher = rdr.searcher();
    assert_eq!(searcher.num_docs(), 2);

    // Удаляем один документ
    handle.delete("sha1").expect("delete sha1");
    handle.commit().expect("commit delete");

    let rdr2 = handle.reader().expect("reader after delete");
    let searcher2 = rdr2.searcher();
    assert_eq!(searcher2.num_docs(), 1);

    // Документ sha2 на месте
    let term = Term::from_field_text(handle.sha_field(), "sha2");
    let tq = TermQuery::new(term, IndexRecordOption::Basic);
    let top = searcher2
        .search(&tq, &TopDocs::with_limit(10).order_by_score())
        .expect("search");
    assert_eq!(top.len(), 1);

    // Документ sha1 удалён
    let term1 = Term::from_field_text(handle.sha_field(), "sha1");
    let tq1 = TermQuery::new(term1, IndexRecordOption::Basic);
    let top1 = searcher2
        .search(&tq1, &TopDocs::with_limit(10).order_by_score())
        .expect("search");
    assert_eq!(top1.len(), 0);
}

#[test]
fn test_index_open_or_create_disk() {
    let dir = tempdir().expect("temp dir");
    let index_dir = dir.path().join("index");

    // 1. Создание нового индекса
    {
        let mut handle = open_or_create(&index_dir, None).expect("open new");
        handle.add("sha_test", "Первый документ").expect("add");
        handle.commit().expect("commit");
    }

    // 2. Открытие существующего индекса
    {
        let handle = open_or_create(&index_dir, None).expect("reopen");
        let rdr = handle.reader().expect("reader");
        let searcher = rdr.searcher();
        assert_eq!(searcher.num_docs(), 1);
    }

    // 3. Открытие только для чтения через reader()
    {
        let rdr = reader(&index_dir, None).expect("open reader");
        let searcher = rdr.searcher();
        assert_eq!(searcher.num_docs(), 1);
    }
}

#[test]
fn test_index_with_morphology_dictionary() {
    let morph_path = Path::new("assets/morph.fst");
    if !morph_path.exists() {
        eprintln!("Пропуск теста: assets/morph.fst отсутствует");
        return;
    }
    let morph = Morph::open(morph_path).expect("open morph");

    let mut handle = create_in_ram(Some(morph)).expect("create index with morph");
    handle
        .add("sha_book", "Маленькая девочка спешила в школу")
        .expect("add");
    handle.commit().expect("commit");

    let rdr = handle.reader().expect("reader");
    let searcher = rdr.searcher();
    assert_eq!(searcher.num_docs(), 1);
}

#[test]
fn test_index_delete_all() {
    let mut handle = create_in_ram(None).expect("create ram");
    handle.add("sha1", "Текст один").expect("add 1");
    handle.add("sha2", "Текст два").expect("add 2");
    handle.commit().expect("commit");

    let rdr = handle.reader().expect("reader");
    assert_eq!(rdr.searcher().num_docs(), 2);

    handle.delete_all().expect("delete all");
    handle.commit().expect("commit");

    let rdr2 = handle.reader().expect("reader after delete all");
    assert_eq!(rdr2.searcher().num_docs(), 0);
}

#[test]
fn test_writer_threads_and_memory_budget() {
    let threads = default_writer_threads();
    assert!((1..=4).contains(&threads));
    assert_eq!(WRITER_MEMORY_BUDGET_BYTES, 256 * 1024 * 1024);

    let dir = tempdir().expect("tempdir");
    let handle = open_or_create_with_options(&dir.path().join("idx"), None, 50_000_000, 2)
        .expect("open with options");
    assert_eq!(handle.reader().expect("reader").searcher().num_docs(), 0);
}

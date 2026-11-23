use std::time::Instant;

use poiskovichek::morph::{Morph, build_morph_fst};
use poiskovichek::query::{QueryWordMatcher, parse_query, word_matches};
use tempfile::tempdir;

#[test]
fn empty_whitespace_and_mixed_tokens_are_safe() {
    let query_empty = parse_query("", None, true);
    assert!(query_empty.words.is_empty());

    let query_spaces = parse_query("   \t\n  ", None, true);
    assert!(query_spaces.words.is_empty());

    let mixed = parse_query("hello мир", None, true);
    assert_eq!(mixed.words.len(), 2);
    assert_eq!(mixed.words[0].original, "hello");
    assert_eq!(mixed.words[1].original, "мир");

    let hyphen = parse_query("по-русски", None, true);
    assert_eq!(hyphen.words.len(), 2);
    assert_eq!(hyphen.words[0].original, "по");
    assert_eq!(hyphen.words[1].original, "русски");

    let num = parse_query("fb2 2024", None, true);
    assert_eq!(num.words.len(), 2);
    assert_eq!(num.words[0].original, "fb2");
    assert_eq!(num.words[1].original, "2024");

    let case = parse_query("Сельские Мужики", None, true);
    assert_eq!(case.words.len(), 2);
    assert_eq!(case.words[0].original, "Сельские");
    assert_eq!(case.words[1].original, "Мужики");
}

#[test]
fn special_syntax_and_keywords_are_data() {
    for input in [
        "foo\"bar", "foo*", "foo -bar", "a (b)", "x:y", "AND", "OR", "NOT", "NEAR",
    ] {
        let parsed = parse_query(input, None, true);
        assert!(
            !parsed.words.is_empty(),
            "запрос {input} не должен быть пустым"
        );
    }

    let and_query = parse_query("AND", None, true);
    assert_eq!(and_query.words.len(), 1);
    assert_eq!(and_query.words[0].original, "AND");
    assert_eq!(
        and_query.words[0].matcher,
        QueryWordMatcher::Prefix("and".to_string())
    );

    let symbols_only = parse_query("***", None, true);
    assert!(symbols_only.words.is_empty());

    let quotes_only = parse_query("\"\"\" \"", None, true);
    assert!(quotes_only.words.is_empty());
}

#[test]
fn short_and_known_words_rule() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test_morph.sqlite3");
    let fst_path = dir.path().join("test_morph.fst");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE word_primary(
            word TEXT NOT NULL,
            lexeme_id INTEGER NOT NULL,
            PRIMARY KEY(word, lexeme_id)
        ) WITHOUT ROWID;

        INSERT INTO word_primary(word, lexeme_id) VALUES
            ('месту', 42);",
    )
    .unwrap();
    drop(conn);

    build_morph_fst(&db_path, &fst_path).unwrap();
    let morph = Morph::open(&fst_path).unwrap();

    // 1-2 символа -> Exact([форма]) (никакого префикса, R2-А-3, R2-В-17)
    let q = parse_query("к месту", Some(&morph), true);
    assert_eq!(q.words.len(), 2);

    assert_eq!(q.words[0].original, "к");
    assert_eq!(
        q.words[0].matcher,
        QueryWordMatcher::Exact(vec!["к".to_string()])
    );

    assert_eq!(q.words[1].original, "месту");
    assert_eq!(
        q.words[1].matcher,
        QueryWordMatcher::Exact(vec!["#42".to_string()])
    );

    // Без словаря или use_forms=false: короткое "к" остаётся Exact, а длинное становится Prefix
    let q_no_dict = parse_query("к месту", None, true);
    assert_eq!(
        q_no_dict.words[0].matcher,
        QueryWordMatcher::Exact(vec!["к".to_string()])
    );
    assert_eq!(
        q_no_dict.words[1].matcher,
        QueryWordMatcher::Prefix("месту".to_string())
    );

    let q_no_forms = parse_query("к месту", Some(&morph), false);
    assert_eq!(
        q_no_forms.words[0].matcher,
        QueryWordMatcher::Exact(vec!["к".to_string()])
    );
    assert_eq!(
        q_no_forms.words[1].matcher,
        QueryWordMatcher::Prefix("месту".to_string())
    );
}

#[test]
fn single_unknown_word_uses_prefix() {
    let q = parse_query("неизвестноеслово", None, true);
    assert_eq!(q.words.len(), 1);
    assert_eq!(
        q.words[0].matcher,
        QueryWordMatcher::Prefix("неизвестноеслово".to_string())
    );
}

#[test]
fn phrase_words_give_one_query_word_each() {
    let q = parse_query("спешившую к месту съемочную группу", None, true);
    assert_eq!(q.words.len(), 5);
    assert_eq!(q.words[0].original, "спешившую");
    assert_eq!(q.words[1].original, "к");
    assert_eq!(q.words[2].original, "месту");
    assert_eq!(q.words[3].original, "съемочную");
    assert_eq!(q.words[4].original, "группу");
}

#[test]
fn very_long_query_faster_than_one_second() {
    let query_text = std::iter::repeat_n("слово", 10_000)
        .collect::<Vec<_>>()
        .join(" ");

    let started = Instant::now();
    let q = parse_query(&query_text, None, true);
    let elapsed = started.elapsed();

    assert_eq!(q.words.len(), 10_000);
    assert!(
        elapsed.as_secs_f64() < 1.0,
        "запрос 10 000 слов должен разбираться быстрее 1 с, заняло: {elapsed:?}"
    );
}

#[test]
fn word_matches_behavior() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test_morph.sqlite3");
    let fst_path = dir.path().join("test_morph.fst");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE word_primary(
            word TEXT NOT NULL,
            lexeme_id INTEGER NOT NULL,
            PRIMARY KEY(word, lexeme_id)
        ) WITHOUT ROWID;

        INSERT INTO word_primary(word, lexeme_id) VALUES
            ('сельские', 5),
            ('сельскими', 5),
            ('мужики', 6),
            ('мужиками', 6);",
    )
    .unwrap();
    drop(conn);

    build_morph_fst(&db_path, &fst_path).unwrap();
    let morph = Morph::open(&fst_path).unwrap();

    let query = parse_query("сельскими мужиками", Some(&morph), true);
    assert_eq!(query.words.len(), 2);

    // 1. Совпадение по словоформе через лексему
    assert!(word_matches("Сельские", &query.words[0], Some(&morph)));
    assert!(word_matches("мужики", &query.words[1], Some(&morph)));

    // Несовпадение другого слова
    assert!(!word_matches("городские", &query.words[0], Some(&morph)));
    assert!(!word_matches("бабы", &query.words[1], Some(&morph)));

    // 2. Префиксное совпадение
    let prefix_query = parse_query("dvd", None, true);
    assert!(word_matches("DVD", &prefix_query.words[0], None));
    assert!(word_matches("dvdrom", &prefix_query.words[0], None));
    assert!(!word_matches("cdrom", &prefix_query.words[0], None));

    // 3. Точное короткое совпадение
    let short_query = parse_query("к", None, true);
    assert!(word_matches("К", &short_query.words[0], None));
    assert!(!word_matches("ко", &short_query.words[0], None));
}

// Задача №239: режим «точно как набрано» разбирает каждое слово в целое слово
#[test]
fn exact_forms_mode_parses_whole_words() {
    use poiskovichek::query::{WordFormsMode, parse_query_forms};

    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test_morph.sqlite3");
    let fst_path = dir.path().join("test_morph.fst");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE word_primary(
            word TEXT NOT NULL,
            lexeme_id INTEGER NOT NULL,
            PRIMARY KEY(word, lexeme_id)
        ) WITHOUT ROWID;
        INSERT INTO word_primary(word, lexeme_id) VALUES
            ('книга', 7), ('книги', 7), ('книгами', 7);",
    )
    .unwrap();
    drop(conn);
    build_morph_fst(&db_path, &fst_path).unwrap();
    let morph = Morph::open(&fst_path).unwrap();

    // Все формы: лексема (прежнее поведение)
    let all = parse_query_forms("книга", Some(&morph), true, WordFormsMode::AllForms);
    assert_eq!(
        all.words[0].matcher,
        QueryWordMatcher::Exact(vec!["#7".to_string()])
    );
    assert_eq!(all, parse_query("книга", Some(&morph), true));

    // Точно: одна нормализованная форма, без лексем и без префикса
    let exact = parse_query_forms("Книга", Some(&morph), true, WordFormsMode::Exact);
    assert_eq!(
        exact.words[0].matcher,
        QueryWordMatcher::Exact(vec!["книга".to_string()])
    );
    // Результат не зависит ни от словаря, ни от use_forms
    for (m, use_forms) in [(None, true), (Some(&morph), false), (None, false)] {
        let q = parse_query_forms("Книга", m, use_forms, WordFormsMode::Exact);
        assert_eq!(q, exact);
    }

    // Латиница, цифры и «ё»: тоже целое слово в нормализованном виде
    let mixed = parse_query_forms("Report 2024 Ёлка", None, true, WordFormsMode::Exact);
    let matchers: Vec<_> = mixed.words.iter().map(|w| w.matcher.clone()).collect();
    assert_eq!(
        matchers,
        vec![
            QueryWordMatcher::Exact(vec!["report".to_string()]),
            QueryWordMatcher::Exact(vec!["2024".to_string()]),
            QueryWordMatcher::Exact(vec!["елка".to_string()]),
        ]
    );

    // Сопоставление слова из текста: форма совпадает, другая форма и продолжение — нет
    let word = &exact.words[0];
    assert!(word_matches("книга", word, Some(&morph)));
    assert!(word_matches("КНИГА", word, Some(&morph)));
    assert!(!word_matches("книги", word, Some(&morph)));
    assert!(!word_matches("книгами", word, Some(&morph)));
    assert!(!word_matches("книгалово", word, Some(&morph)));
    assert!(word_matches("книги", &all.words[0], Some(&morph)));
}

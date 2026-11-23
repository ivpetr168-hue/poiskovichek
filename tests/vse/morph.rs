use std::path::Path;
use std::time::Instant;

use poiskovichek::morph::{Morph, build_morph_fst, normalize};
use poiskovichek::tokens::PoiskTokenizer;
use tantivy_tokenizer_api::{Token, TokenStream, Tokenizer};
use tempfile::tempdir;

#[test]
fn dictionary_built_from_small_sqlite() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("test_morph.sqlite3");
    let fst_path = dir.path().join("test_morph.fst");

    // Создаём базу SQLite той же схемы
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE word_primary(
            word TEXT NOT NULL,
            lexeme_id INTEGER NOT NULL,
            PRIMARY KEY(word, lexeme_id)
        ) WITHOUT ROWID;

        INSERT INTO word_primary(word, lexeme_id) VALUES
            ('стали', 1),
            ('стали', 2),
            ('все', 3),
            ('всё', 3),
            ('спешившая', 4),
            ('спешившую', 4),
            ('сельские', 5),
            ('сельскими', 5),
            ('мужики', 6),
            ('мужиками', 6);",
    )
    .unwrap();
    drop(conn);

    let stats = build_morph_fst(&db_path, &fst_path).unwrap();
    assert_eq!(stats.form_count, 8); // стали, все, спешившая, спешившую, сельские, сельскими, мужики, мужиками
    assert_eq!(stats.lexeme_count, 6);
    assert_eq!(stats.multi_lexeme_forms, 1); // "стали" has lexemes 1 and 2

    let morph = Morph::open(&fst_path).unwrap();

    // Омоним "стали" -> обе лексемы 1 и 2
    assert_eq!(morph.lexemes("стали"), &[1, 2]);

    // Пары все / всё -> одна лексема 3
    assert_eq!(morph.lexemes("все"), &[3]);
    assert_eq!(morph.lexemes(&normalize("всё")), &[3]);

    // Словоформы спешившая / спешившую
    assert_eq!(morph.lexemes("спешившая"), &[4]);
    assert_eq!(morph.lexemes("спешившую"), &[4]);

    // Неизвестное слово
    assert_eq!(morph.lexemes("неизвестное"), &[] as &[u32]);
}

#[test]
fn missing_and_corrupted_morph_rejected_clearly() {
    let dir = tempdir().unwrap();

    // Отсутствующий файл
    let missing_path = dir.path().join("non_existent_morph.fst");
    let err = Morph::open(&missing_path).unwrap_err();
    let err_str = err.to_string();
    assert!(
        err_str.contains("не найден"),
        "ошибка должна сообщать о ненайденном файле: {err_str}"
    );
    assert!(
        err_str.contains(missing_path.to_str().unwrap()),
        "ошибка должна содержать путь: {err_str}"
    );

    // Файл с мусором [0xde, 0xad, 0xbe, 0xef]
    let garbage_path = dir.path().join("garbage.fst");
    std::fs::write(&garbage_path, [0xde, 0xad, 0xbe, 0xef]).unwrap();
    let err = Morph::open(&garbage_path).unwrap_err();
    let err_str = err.to_string();
    assert!(
        err_str.contains("повреждён") || err_str.contains("меньше минимального размера"),
        "ошибка должна быть понятной: {err_str}"
    );

    // Файл правильного размера (64 байта), но с неверной магией
    let bad_magic_path = dir.path().join("bad_magic.fst");
    let mut bad_header = vec![0u8; 64];
    bad_header[0..8].copy_from_slice(b"BADMAGIC");
    std::fs::write(&bad_magic_path, &bad_header).unwrap();
    let err = Morph::open(&bad_magic_path).unwrap_err();
    let err_str = err.to_string();
    assert!(
        err_str.contains("неверная сигнатура") || err_str.contains("магия"),
        "ошибка должна указывать на сигнатуру/магию: {err_str}"
    );
}

#[test]
fn real_morph_speshivshuyu_and_selskie_muzhiki() {
    let fst_path = Path::new("assets/morph.fst");
    if !fst_path.exists() {
        eprintln!("Пропуск теста: assets/morph.fst ещё не собран");
        return;
    }

    let morph = Morph::open(fst_path).expect("словарь должен открываться");

    // «спешившую» и «спешившая» дают общую лексему
    let l1 = morph.lexemes(&normalize("спешившую"));
    let l2 = morph.lexemes(&normalize("спешившая"));
    assert!(!l1.is_empty(), "слово спешившую должно быть в словаре");
    assert!(!l2.is_empty(), "слово спешившая должно быть в словаре");
    assert!(
        l1.iter().any(|id| l2.contains(id)),
        "«спешившую» и «спешившая» должны иметь общую лексему: {l1:?} vs {l2:?}"
    );

    // «сельскими мужиками» и «сельские мужики» — общие лексемы по каждому слову
    let s1 = morph.lexemes(&normalize("сельскими"));
    let s2 = morph.lexemes(&normalize("сельские"));
    assert!(
        s1.iter().any(|id| s2.contains(id)),
        "сельскими и сельские должны иметь общую лексему"
    );

    let m1 = morph.lexemes(&normalize("мужиками"));
    let m2 = morph.lexemes(&normalize("мужики"));
    assert!(
        m1.iter().any(|id| m2.contains(id)),
        "мужиками и мужики должны иметь общую лексему"
    );

    // все / всё
    let v1 = morph.lexemes(&normalize("все"));
    let v2 = morph.lexemes(&normalize("всё"));
    assert_eq!(
        v1, v2,
        "«все» и «всё» должны давать одинаковый набор лексем"
    );
}

fn collect_tokens(tokenizer: &mut PoiskTokenizer, text: &str) -> Vec<Token> {
    let mut stream = tokenizer.token_stream(text);
    let mut tokens = Vec::new();
    while stream.advance() {
        tokens.push(stream.token().clone());
    }
    tokens
}

#[test]
fn tokenizer_behavior() {
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
            ('стали', 10),
            ('стали', 20),
            ('сельские', 30),
            ('мужики', 40),
            ('привод', 50);",
    )
    .unwrap();
    drop(conn);

    build_morph_fst(&db_path, &fst_path).unwrap();
    let morph = Morph::open(&fst_path).unwrap();

    let mut tokenizer = PoiskTokenizer::with_morph(morph.clone());

    // 1. «DVD-привод» -> две позиции: 0 ("dvd"), 1 ("привод" и "#50")
    let tokens = collect_tokens(&mut tokenizer, "DVD-привод");
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0].text, "dvd");
    assert_eq!(tokens[0].position, 0);

    assert_eq!(tokens[1].text, "привод");
    assert_eq!(tokens[1].position, 1);
    assert_eq!(tokens[2].text, "#50");
    assert_eq!(tokens[2].position, 1);

    // 2. «Сельские Мужики» -> нижний регистр
    let tokens = collect_tokens(&mut tokenizer, "Сельские Мужики");
    assert_eq!(tokens[0].text, "сельские");
    assert_eq!(tokens[0].position, 0);
    assert_eq!(tokens[1].text, "#30");
    assert_eq!(tokens[1].position, 0);

    assert_eq!(tokens[2].text, "мужики");
    assert_eq!(tokens[2].position, 1);
    assert_eq!(tokens[3].text, "#40");
    assert_eq!(tokens[3].position, 1);

    // 3. Омоним «стали» — оба номера лексем на одной позиции
    let tokens = collect_tokens(&mut tokenizer, "стали");
    assert_eq!(tokens.len(), 3);
    assert_eq!(tokens[0].text, "стали");
    assert_eq!(tokens[0].position, 0);
    assert_eq!(tokens[1].text, "#10");
    assert_eq!(tokens[1].position, 0);
    assert_eq!(tokens[2].text, "#20");
    assert_eq!(tokens[2].position, 0);

    // 4. Без словаря — только формы
    let mut no_morph_tok = PoiskTokenizer::without_morph();
    let tokens = collect_tokens(&mut no_morph_tok, "Сельские Мужики стали DVD-привод");
    for t in &tokens {
        assert!(
            !t.text.starts_with('#'),
            "без словаря лексемы не должны выдаваться: {}",
            t.text
        );
    }
    let words: Vec<&str> = tokens.iter().map(|t| t.text.as_str()).collect();
    assert_eq!(words, &["сельские", "мужики", "стали", "dvd", "привод"]);

    // 5. Слово длиннее 64 символов пропускается, но позиция занимается
    let long_word = "а".repeat(65);
    let input = format!("раз {long_word} два");
    let tokens = collect_tokens(&mut no_morph_tok, &input);
    assert_eq!(tokens.len(), 2);
    assert_eq!(tokens[0].text, "раз");
    assert_eq!(tokens[0].position, 0);
    assert_eq!(tokens[1].text, "два");
    assert_eq!(tokens[1].position, 2); // 1 занята пропущенным словом
}

#[test]
#[ignore]
fn perf_tokenizer() {
    let fst_path = Path::new("assets/morph.fst");
    let morph = if fst_path.exists() {
        Some(Morph::open(fst_path).unwrap())
    } else {
        None
    };

    let mut tokenizer = PoiskTokenizer::new(morph);

    // Создаём 50 МБ русского текста
    let sample = "В тихом омуте черти водятся, а сельские мужики спешившую группу встречали у реки. Были и DVD-приводы, и книги, и старые записи. ";
    let repeats = (50 * 1024 * 1024) / sample.len() + 1;
    let big_text = sample.repeat(repeats);
    let bytes_len = big_text.len();

    // Прогрев
    let _ = collect_tokens(&mut tokenizer, sample);

    let started = Instant::now();
    let mut stream = tokenizer.token_stream(&big_text);
    let mut count = 0;
    while stream.advance() {
        count += 1;
    }
    let elapsed = started.elapsed();
    let speed_mb_s = (bytes_len as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();

    println!(
        "\n--- Замер токенизатора ---\nОбработано: {:.2} МБ\nТокенов: {}\nВремя: {:.3} с\nСкорость: {:.1} МБ/с\n",
        bytes_len as f64 / (1024.0 * 1024.0),
        count,
        elapsed.as_secs_f64(),
        speed_mb_s
    );

    #[cfg(not(debug_assertions))]
    assert!(
        speed_mb_s >= 60.0,
        "скорость токенизатора в выпуске ({speed_mb_s:.1} МБ/с) должна быть >= 60 МБ/с"
    );
}

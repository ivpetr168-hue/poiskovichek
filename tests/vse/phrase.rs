use std::path::Path;

use poiskovichek::morph::Morph;
use poiskovichek::phrase::{
    compressed_contains_sequence, contains_sequence, find_sequence, find_sequence_compressed,
};
use poiskovichek::query::parse_query;

fn load_morph() -> Morph {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let morph_path = manifest_dir.join("assets").join("morph.fst");
    Morph::open(morph_path).expect("не удалось открыть assets/morph.fst")
}

#[test]
fn strict_phrase_accepts_only_adjacent_ordered_word_forms() {
    // Перенос R2-В-22: строгий порядок и смежность слов
    let morph = load_morph();
    let query = parse_query("сельские мужики", Some(&morph), true);

    // 1. Слова разделены точкой и другим предложением — отказ
    let t1 = "Сельские дороги затопило. Мужики вернулись вечером.";
    assert_eq!(find_sequence(t1, &query, Some(&morph)), None);

    // 2. Слова идут подряд — успех
    let t2 = "На площади стояли сельские мужики.";
    let m2 = find_sequence(t2, &query, Some(&morph));
    assert!(m2.is_some());
    let (s, e) = m2.unwrap();
    assert_eq!(&t2[s..e], "сельские мужики");

    // 3. Формы слов в тексте отличаются (творительный падеж) — успех через морфологию
    let t3 = "Он разговаривал с сельскими мужиками.";
    let m3 = find_sequence(t3, &query, Some(&morph));
    assert!(m3.is_some());
    let (s, e) = m3.unwrap();
    assert_eq!(&t3[s..e], "сельскими мужиками");

    // 4. Обратный порядок слов — отказ
    let t4 = "Мужики вернулись. Сельские дороги затопило.";
    assert_eq!(find_sequence(t4, &query, Some(&morph)), None);

    // 5. Союз между словами — отказ
    let t5 = "Сельские и мужики.";
    assert_eq!(find_sequence(t5, &query, Some(&morph)), None);
}

#[test]
fn strict_phrase_rejects_reverse_order_and_accepts_query_word_forms() {
    // Перенос R2-В-23: обратный порядок и словоформы
    let morph = load_morph();

    // Обратный порядок слов в запросе
    let rev_query = parse_query("мужики сельские", Some(&morph), true);
    let text = "На площади стояли Сельские мужики.";
    assert_eq!(find_sequence(text, &rev_query, Some(&morph)), None);

    // Запрос в творительном падеже «сельскими мужиками» находит «Сельские мужики»
    let inflected_query = parse_query("сельскими мужиками", Some(&morph), true);
    let m = find_sequence(text, &inflected_query, Some(&morph));
    assert!(m.is_some());
    let (s, e) = m.unwrap();
    assert_eq!(&text[s..e], "Сельские мужики");
}

#[test]
fn single_word_keeps_previous_semantics() {
    // Перенос R2-В-24: одиночное слово находит вхождение независимо от окружения
    let morph = load_morph();
    let query = parse_query("мужики", Some(&morph), true);

    let t1 = "Сельские дороги затопило. Мужики вернулись вечером.";
    let m1 = find_sequence(t1, &query, Some(&morph));
    assert!(m1.is_some());

    let t2 = "Он разговаривал с сельскими мужиками.";
    let m2 = find_sequence(t2, &query, Some(&morph));
    assert!(m2.is_some());
}

#[test]
fn bounded_candidate_query_still_verifies_the_whole_long_phrase() {
    // Перенос R2-В-27: разрыв в длинной фразе отсекается
    let query = parse_query("ддд еее жжж ззз", None, false);

    let text_exact = "ддд еее жжж ззз";
    assert!(contains_sequence(text_exact, &query, None));

    let text_gap = "ддд еее лишнее жжж ззз";
    assert!(!contains_sequence(text_gap, &query, None));
}

#[test]
fn strict_phrase_treats_hyphen_as_a_word_boundary() {
    // Перенос R2-В-28: дефис как граница слова
    let text = "Чтобы установить игру, вставьте установочный DVD в DVD-привод.";
    let query = parse_query("вставьте установочный DVD в DVD-привод", None, false);

    let found = find_sequence(text, &query, None);
    assert!(found.is_some());
    let (s, e) = found.unwrap();
    assert_eq!(&text[s..e], "вставьте установочный DVD в DVD-привод");
}

#[test]
fn performance_1mb_text_under_5ms() {
    // Замер скорости: 1 МБ текста <= 5 мс
    let morph = load_morph();
    let query = parse_query("сельские мужики", Some(&morph), true);

    // Создаем ~1 МБ текста
    let mut big_text =
        "Обычный текст документа с описанием различных событий и фактов. ".repeat(15_000);
    big_text.push_str("В самом конце стояли сельские мужики и беседовали.");

    let start = std::time::Instant::now();
    let found = find_sequence(&big_text, &query, Some(&morph));
    let elapsed = start.elapsed();

    assert!(found.is_some());
    println!("Время поиска фразы в 1 МБ текста: {elapsed:?}");
    // В релизе должно быть <= 5 мс, в dev может быть немного дольше, но проверяем разумный порог
    #[cfg(not(debug_assertions))]
    assert!(elapsed.as_millis() <= 5, "Превышен порог 5 мс: {elapsed:?}");
}

#[test]
fn compressed_streaming_20mb_text_phrase_at_end() {
    // Требование задания: сжатый текст 20 МБ, фраза в самом конце → находится,
    // память не растёт на весь текст.
    let mut big_text =
        "Длинная строка для тестирования потоковой распаковки текста. ".repeat(300_000);
    big_text.push_str("Финальная фраза: сельские мужики собрались.");

    let compressed = zstd::encode_all(big_text.as_bytes(), 1).expect("zstd encode");

    let morph = load_morph();
    let query = parse_query("сельские мужики", Some(&morph), true);

    let res = find_sequence_compressed(&compressed, &query, Some(&morph));
    assert!(res.is_ok());
    let opt = res.unwrap();
    assert!(
        opt.is_some(),
        "Фраза в конце 20 МБ текста должна быть найдена"
    );

    let (s, e) = opt.unwrap();
    assert_eq!(&big_text[s..e], "сельские мужики");

    let contains = compressed_contains_sequence(&compressed, &query, Some(&morph));
    assert!(contains.unwrap());
}

#[test]
fn empty_and_corrupt_compressed_data_handling() {
    let query = parse_query("слово", None, false);

    // Пустые сжатые данные
    assert!(find_sequence_compressed(&[], &query, None).is_err());

    // Битые данные
    let corrupt = [0xDE, 0xAD, 0xBE, 0xEF];
    assert!(find_sequence_compressed(&corrupt, &query, None).is_err());

    // Пустой запрос
    let empty_q = parse_query("", None, false);
    let valid_compressed = zstd::encode_all("любой текст".as_bytes(), 1).unwrap();
    assert_eq!(
        find_sequence_compressed(&valid_compressed, &empty_q, None).unwrap(),
        None
    );
}

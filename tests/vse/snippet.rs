use std::path::Path;

use poiskovichek::morph::Morph;
use poiskovichek::query::{QueryWord, QueryWordMatcher, parse_query};
use poiskovichek::snippet::{build_snippet, build_snippet_compressed, make_snippet};

fn load_morph() -> Morph {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let morph_path = manifest_dir.join("assets").join("morph.fst");
    Morph::open(morph_path).expect("не удалось открыть assets/morph.fst")
}

fn compress_text(text: &str) -> Vec<u8> {
    zstd::encode_all(text.as_bytes(), 1).expect("zstd compression")
}

#[test]
fn snippet_is_anchored_on_the_real_phrase() {
    // Перенос R2-В-25: сниппет центрируется на фразе
    let morph = load_morph();
    let query = parse_query("сельские мужики", Some(&morph), true);

    let text = format!(
        "{} На площади стояли сельские мужики и громко обсуждали новости. {}",
        "далеко ".repeat(40),
        "дальше ".repeat(40)
    );

    let snippet = build_snippet(&text, &query, Some(&morph), 140, 300, true);
    let marked = snippet.to_marked_string();
    assert!(marked.contains("⟦сельские⟧ ⟦мужики⟧"));
}

#[test]
fn phrase_and_anchor_select_expected_fragment() {
    // Перенос R2-В-39: выбор фрагмента по фразе или якорю
    let morph = load_morph();
    let query = parse_query("сельские мужики", Some(&morph), true);

    let body = compress_text(&format!("{} сельские мужики конец", "далеко ".repeat(80)));

    let phrase_snippet = make_snippet(&body, &query, Some(&morph), true).unwrap();
    assert!(phrase_snippet.contains("⟦сельские⟧ ⟦мужики⟧"));

    let anchor_snippet = make_snippet(&body, &query, Some(&morph), false).unwrap();
    assert!(anchor_snippet.contains("⟦сельские⟧"));
}

#[test]
fn boundaries_utf8_and_short_text_are_safe() {
    // Перенос R2-В-40: границы UTF-8 и короткие тексты
    let morph = load_morph();
    for text in [
        "слово в начале",
        "конец содержит слово",
        "ёжик слово 日本語",
        "короткое слово",
    ] {
        let query = parse_query("слово", Some(&morph), true);
        let snippet = make_snippet(&compress_text(text), &query, Some(&morph), false).unwrap();
        assert!(snippet.is_char_boundary(snippet.len()));
        assert!(snippet.contains("⟦слово⟧"));
    }
}

#[test]
fn all_matches_in_window_are_marked_and_whitespace_collapses() {
    // Перенос R2-В-41: множественные совпадения и схлопывание пробелов
    let morph = load_morph();
    let query = parse_query("слово", Some(&morph), true);

    let compressed = compress_text("слово\n\n\nслово\tслово");
    let snippet = make_snippet(&compressed, &query, Some(&morph), false).unwrap();

    assert_eq!(snippet.matches("⟦слово⟧").count(), 3);
    assert!(!snippet.contains('\n'));
    assert!(!snippet.contains('\t'));
    assert!(!snippet.contains("  "));
}

#[test]
fn prefix_marks_each_form() {
    // Перенос R2-В-42: префикс подсвечивает все подходящие слова
    let query = poiskovichek::query::ParsedQuery {
        words: vec![QueryWord {
            original: "спеш".to_string(),
            matcher: QueryWordMatcher::Prefix("спеш".to_string()),
        }],
    };

    let snippet =
        make_snippet(&compress_text("спешившая и спешивший"), &query, None, false).unwrap();
    assert!(snippet.contains("⟦спешившая⟧") && snippet.contains("⟦спешивший⟧"));
}

#[test]
fn invalid_empty_and_no_groups_do_not_panic() {
    // Перенос R2-В-43: битые данные, пустые данные и пустой запрос
    let query = parse_query("слово", None, false);

    assert!(make_snippet(&[0xDE, 0xAD, 0xBE, 0xEF], &query, None, false).is_err());
    assert!(make_snippet(&[], &query, None, false).is_err());

    let empty_q = parse_query("", None, false);
    assert_eq!(
        make_snippet(&compress_text(""), &empty_q, None, false).unwrap(),
        ""
    );
}

#[test]
fn highlight_real_word_forms_morph() {
    // Требование: подсветка: «⟦спешившая⟧» при запросе «спешившую»
    let morph = load_morph();
    let query = parse_query("спешившую", Some(&morph), true);

    let text = "На перроне стояла спешившая на утренний поезд группа людей.";
    let snippet = build_snippet(text, &query, Some(&morph), 140, 300, false);
    let marked = snippet.to_marked_string();

    assert!(
        marked.contains("⟦спешившая⟧"),
        "Словоформа 'спешившая' должна быть подсвечена при запросе 'спешившую': {marked}"
    );
}

#[test]
fn shared_text_blob_snippet_consistency() {
    // Перенос R1-Б-67: для одинакового сжатого текста сниппет строится идентично
    let morph = load_morph();
    let query = parse_query("сельские мужики", Some(&morph), true);
    let text =
        "Это текст, который может быть связан с несколькими файлами. В нём есть сельские мужики.";
    let compressed = compress_text(text);

    let s1 = make_snippet(&compressed, &query, Some(&morph), true).unwrap();
    let s2 = make_snippet(&compressed, &query, Some(&morph), true).unwrap();

    assert_eq!(s1, s2);
    assert!(s1.contains("⟦сельские⟧ ⟦мужики⟧"));

    // Прямой вызов build_snippet_compressed
    let snippet =
        build_snippet_compressed(&compressed, &query, Some(&morph), 140, 300, true).unwrap();
    assert!(!snippet.is_empty());
    assert!(snippet.segments.iter().any(|(s, m)| *m && s == "сельские"));
    assert!(snippet.segments.iter().any(|(s, m)| *m && s == "мужики"));
}

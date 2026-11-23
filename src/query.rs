use crate::morph::{Morph, normalize};
pub use crate::settings::WordFormsMode;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryWordMatcher {
    Exact(Vec<String>),
    Prefix(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryWord {
    pub original: String,
    pub matcher: QueryWordMatcher,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub words: Vec<QueryWord>,
}

/// Разбирает запрос в режиме «все формы» (прежнее поведение).
pub fn parse_query(text: &str, morph: Option<&Morph>, use_forms: bool) -> ParsedQuery {
    parse_query_forms(text, morph, use_forms, WordFormsMode::AllForms)
}

/// Разбирает запрос с выбором режима словоформ.
///
/// В режиме `Exact` каждое слово — целое слово ровно в набранной форме
/// (после той же нормализации регистра и «ё», что и при индексации): ни лексем, ни префикса.
pub fn parse_query_forms(
    text: &str,
    morph: Option<&Morph>,
    use_forms: bool,
    forms: WordFormsMode,
) -> ParsedQuery {
    let mut words = Vec::new();
    let mut byte_idx = 0;

    while byte_idx < text.len() {
        // Пропускаем не-буквы и не-цифры (пунктуация, кавычки, спецсимволы, пробелы)
        let remaining = &text[byte_idx..];
        let mut word_start = None;
        for (rel_byte_idx, c) in remaining.char_indices() {
            if c.is_alphanumeric() {
                word_start = Some(byte_idx + rel_byte_idx);
                break;
            }
        }

        let Some(start) = word_start else {
            break;
        };

        // Ищем конец непрерывной последовательности букв и цифр
        let from_start = &text[start..];
        let mut word_end = start + from_start.len();
        for (rel_byte_idx, c) in from_start.char_indices() {
            if !c.is_alphanumeric() {
                word_end = start + rel_byte_idx;
                break;
            }
        }

        byte_idx = word_end;
        let original_word = &text[start..word_end];
        let norm = normalize(original_word);
        let char_count = norm.chars().count();
        let is_all_cyrillic = norm.chars().all(|c| matches!(c, 'а'..='я'));

        let matcher = if forms == WordFormsMode::Exact || char_count <= 2 {
            // Exact: целое слово как набрано (одна нормализованная форма, без «#лексем» и префикса)
            // 1-2 символа -> Exact([форма]) (никакого префикса, R2-А-3, R2-В-17)
            QueryWordMatcher::Exact(vec![norm])
        } else if is_all_cyrillic && use_forms {
            // Русское >= 3 букв, известное словарю и use_forms -> Exact(["#лексема", ...])
            if let Some(m) = morph {
                let lexemes = m.lexemes(&norm);
                if !lexemes.is_empty() {
                    let tokens = lexemes.iter().map(|id| format!("#{id}")).collect();
                    QueryWordMatcher::Exact(tokens)
                } else {
                    QueryWordMatcher::Prefix(norm)
                }
            } else {
                QueryWordMatcher::Prefix(norm)
            }
        } else {
            // Иначе (неизвестное, латиница, цифры, смешанное) >= 3 символов -> Prefix(форма)
            QueryWordMatcher::Prefix(norm)
        };

        words.push(QueryWord {
            original: original_word.to_string(),
            matcher,
        });
    }

    ParsedQuery { words }
}

pub fn word_matches(word_in_text: &str, query_word: &QueryWord, morph: Option<&Morph>) -> bool {
    let norm_text = normalize(word_in_text);
    if norm_text.is_empty() {
        return false;
    }

    match &query_word.matcher {
        QueryWordMatcher::Exact(tokens) => {
            // 1. Точное совпадение с нормализованной формой (например, для коротких слов "к")
            if tokens.iter().any(|t| t == &norm_text) {
                return true;
            }
            // 2. Совпадение по лексемам (если токен вида "#<id>")
            if let Some(m) = morph {
                let text_lexemes = m.lexemes(&norm_text);
                for &lid in text_lexemes {
                    let token_candidate = format!("#{lid}");
                    if tokens.iter().any(|t| t == &token_candidate) {
                        return true;
                    }
                }
            }
            false
        }
        QueryWordMatcher::Prefix(prefix) => norm_text.starts_with(prefix),
    }
}

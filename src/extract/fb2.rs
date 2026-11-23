//! Извлечение текста из файлов формата FB2 (FictionBook).
//! Извлечение элементов body и запасной путь для повреждённого XML (R1-А-113, R1-Б-22).

use super::text::decode_text;

/// Рекурсивный обход узла дерева roxmltree со сбором текста и переносами на блочных элементах.
pub fn node_text(node: roxmltree::Node<'_, '_>) -> String {
    enum Step<'a, 'input> {
        Enter(roxmltree::Node<'a, 'input>),
        Close,
    }
    let mut result = String::new();
    let mut stack = vec![Step::Enter(node)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Close => result.push('\n'),
            Step::Enter(current) if current.is_text() => {
                result.push_str(current.text().unwrap_or(""));
            }
            Step::Enter(current) => {
                if current.is_element()
                    && matches!(
                        current.tag_name().name(),
                        "p" | "br"
                            | "div"
                            | "tr"
                            | "tc"
                            | "row"
                            | "table-row"
                            | "table-cell"
                            | "h1"
                            | "h2"
                            | "h3"
                            | "h4"
                            | "h5"
                            | "h6"
                            | "section"
                            | "title"
                            | "subtitle"
                            | "epigraph"
                            | "cite"
                            | "empty-line"
                    )
                {
                    stack.push(Step::Close);
                }
                stack.extend(current.children().rev().map(Step::Enter));
            }
        }
    }
    result
}

/// Запасной путь для повреждённого FB2 XML: вырезает теги <binary>...</binary>
/// и удаляет все оставшиеся XML-теги (R1-А-113).
fn fallback_broken_xml(data: &[u8]) -> String {
    let decoded = decode_text(data);

    // Удаляем теги <binary...>...</binary>
    let mut clean_text = String::with_capacity(decoded.len());
    let mut chars = decoded.char_indices().peekable();
    let lower = decoded.to_ascii_lowercase();

    let mut skip_until_binary_close = false;

    while let Some((idx, ch)) = chars.next() {
        if skip_until_binary_close {
            if ch == '<' && lower[idx..].starts_with("</binary>") {
                // Пропускаем </binary>
                for _ in 0.."</binary>".len() - 1 {
                    chars.next();
                }
                skip_until_binary_close = false;
            }
            continue;
        }

        if ch == '<' && lower[idx..].starts_with("<binary") {
            skip_until_binary_close = true;
            continue;
        }

        clean_text.push(ch);
    }

    // Удаляем все оставшиеся теги <...>
    let mut result = String::with_capacity(clean_text.len());
    let mut in_tag = false;
    for ch in clean_text.chars() {
        if ch == '<' {
            in_tag = true;
            result.push(' ');
        } else if ch == '>' && in_tag {
            in_tag = false;
            result.push(' ');
        } else if !in_tag {
            result.push(ch);
        }
    }

    // Нормализация пробелов
    let words: Vec<&str> = result.split_whitespace().collect();
    words.join(" ")
}

/// Извлекает текст из книги FB2.
pub fn extract_fb2_text(data: &[u8], limit: usize) -> String {
    let decoded = decode_text(data);

    // 1. Попытка разобрать как валидный XML через roxmltree
    if let Ok(doc) = roxmltree::Document::parse(&decoded) {
        let mut parts = Vec::new();
        for node in doc.descendants() {
            if node.is_element() && node.tag_name().name() == "body" {
                let text = node_text(node);
                let normalized: Vec<String> = text
                    .lines()
                    .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|l| !l.is_empty())
                    .collect();
                if !normalized.is_empty() {
                    parts.push(normalized.join("\n"));
                }
            }
        }
        if !parts.is_empty() {
            let combined = parts.join("\n");
            let count = combined.chars().count();
            if count > limit {
                return combined.chars().take(limit).collect();
            }
            return combined;
        }
    }

    // 2. Запасной путь для битого XML (R1-А-113)
    let fallback = fallback_broken_xml(data);
    let count = fallback.chars().count();
    if count > limit {
        fallback.chars().take(limit).collect()
    } else {
        fallback
    }
}

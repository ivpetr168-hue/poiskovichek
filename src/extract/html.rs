//! Извлечение видимого текста из HTML/HTM без тегов script, style, noscript, template.
//! Учёт кодировки из meta charset и переводы строк на блочных тегах.

use super::text::decode_text;

/// Извлекает кодировку из первых байтов HTML (meta charset или meta http-equiv).
fn detect_html_charset(data: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    let sample = &data[..data.len().min(4096)];
    let sample_ascii = sample
        .iter()
        .map(|&b| if b.is_ascii() { b as char } else { ' ' })
        .collect::<String>()
        .to_ascii_lowercase();

    // 1. <meta charset="...">
    if let Some(pos) = sample_ascii.find("charset=") {
        let after = &sample_ascii[pos + 8..];
        let trimmed = after.trim_start_matches(['"', '\'']);
        let charset_name = trimmed
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect::<String>();
        if !charset_name.is_empty()
            && let Some(enc) = encoding_rs::Encoding::for_label(charset_name.as_bytes())
        {
            return Some(enc);
        }
    }
    None
}

/// Декодирует HTML данные в строку с учётом meta charset или эвристики кодировок.
pub fn decode_html(data: &[u8]) -> String {
    if let Some(encoding) = detect_html_charset(data) {
        encoding.decode(data).0.into_owned()
    } else {
        decode_text(data)
    }
}

/// Декодирует основные HTML-сущности.
fn decode_entities(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '&' {
            let mut entity = String::new();
            let mut matched = false;
            while let Some(&next_ch) = chars.peek() {
                if next_ch == ';' {
                    chars.next(); // consume ';'
                    matched = true;
                    break;
                } else if next_ch.is_alphanumeric() || next_ch == '#' {
                    entity.push(chars.next().unwrap());
                    if entity.len() > 10 {
                        break;
                    }
                } else {
                    break;
                }
            }

            if matched {
                match entity.as_str() {
                    "nbsp" => result.push(' '),
                    "amp" => result.push('&'),
                    "lt" => result.push('<'),
                    "gt" => result.push('>'),
                    "quot" => result.push('"'),
                    "apos" => result.push('\''),
                    _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                        if let Ok(code) = u32::from_str_radix(&entity[2..], 16)
                            && let Some(c) = char::from_u32(code)
                        {
                            result.push(c);
                            continue;
                        }
                        result.push('&');
                        result.push_str(&entity);
                        result.push(';');
                    }
                    _ if entity.starts_with('#') => {
                        if let Ok(code) = entity[1..].parse::<u32>()
                            && let Some(c) = char::from_u32(code)
                        {
                            result.push(c);
                            continue;
                        }
                        result.push('&');
                        result.push_str(&entity);
                        result.push(';');
                    }
                    _ => {
                        result.push('&');
                        result.push_str(&entity);
                        result.push(';');
                    }
                }
            } else {
                result.push('&');
                result.push_str(&entity);
            }
        } else {
            result.push(ch);
        }
    }

    result
}

/// Извлекает видимый текст из HTML (R1-А-104..106, R1-Б-19).
pub fn extract_html_text(html: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut current_part = String::new();
    let mut in_tag = false;
    let mut tag_buf = String::new();
    let mut hidden_depth = 0usize;

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
        } else if ch == '>' && in_tag {
            in_tag = false;
            let tag_content = tag_buf.trim();
            let is_closing = tag_content.starts_with('/');
            let tag_name = tag_content
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();

            if matches!(
                tag_name.as_str(),
                "script" | "style" | "noscript" | "template"
            ) {
                if is_closing {
                    hidden_depth = hidden_depth.saturating_sub(1);
                } else if !tag_content.ends_with('/') {
                    hidden_depth += 1;
                }
            } else if hidden_depth == 0
                && matches!(
                    tag_name.as_str(),
                    "br" | "p"
                        | "div"
                        | "li"
                        | "tr"
                        | "h1"
                        | "h2"
                        | "h3"
                        | "h4"
                        | "h5"
                        | "h6"
                        | "section"
                        | "article"
                        | "table"
                )
                && !current_part.trim().is_empty()
            {
                parts.push(current_part.trim().to_string());
                current_part.clear();
            }
        } else if in_tag {
            tag_buf.push(ch);
        } else if hidden_depth == 0 {
            current_part.push(ch);
        }
    }

    if !current_part.trim().is_empty() {
        parts.push(current_part.trim().to_string());
    }

    // Декодируем сущности и нормализуем пробелы каждого фрагмента
    let mut lines = Vec::new();
    for part in parts {
        let decoded = decode_entities(&part);
        let normalized = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() {
            lines.push(normalized);
        }
    }

    lines.join("\n")
}

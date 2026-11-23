use std::io::Read;

use crate::morph::Morph;
use crate::phrase::{WordIter, find_sequence};
use crate::query::{ParsedQuery, word_matches};

/// Фрагмент текста с разметкой совпадений.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snippet {
    pub segments: Vec<(String, bool)>,
}

impl Snippet {
    pub fn new(segments: Vec<(String, bool)>) -> Self {
        Self { segments }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty() || self.segments.iter().all(|(s, _)| s.is_empty())
    }

    /// Преобразует фрагмент в строку с подсветкой скобками «⟦слово⟧».
    pub fn to_marked_string(&self) -> String {
        let mut s = String::new();
        for (seg, matched) in &self.segments {
            if *matched {
                s.push('⟦');
                s.push_str(seg);
                s.push('⟧');
            } else {
                s.push_str(seg);
            }
        }
        s
    }

    /// Возвращает чистый текст фрагмента без отметок.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for (seg, _) in &self.segments {
            s.push_str(seg);
        }
        s
    }
}

/// Строит фрагмент текста вокруг найденного совпадения.
///
/// Место: найденная фраза (если `prefer_phrase` и слов > 1), иначе самое длинное
/// совпавшее слово запроса (R2-А-31, 32).
/// Границы окна выравниваются по символам UTF-8 (R2-В-40).
/// Пробелы и переводы строк схлопываются в одиночные пробелы (R2-В-41).
/// Подсвечиваются ВСЕ совпавшие слова внутри окна (R2-В-41, 42).
pub fn build_snippet(
    text: &str,
    query: &ParsedQuery,
    morph: Option<&Morph>,
    before: usize,
    after: usize,
    prefer_phrase: bool,
) -> Snippet {
    if query.words.is_empty() || text.is_empty() {
        return Snippet::default();
    }

    let mut match_range = None;

    // 1. Сначала пробуем найти точную фразу, если задано prefer_phrase и запрос многословный
    if prefer_phrase && query.words.len() > 1 {
        match_range = find_sequence(text, query, morph);
    }

    // 2. Если фраза не найдена или не запрашивалась — центрируемся по якорному слову
    if match_range.is_none() {
        // Сортируем слова запроса по длине (по убыванию), чтобы предпочесть самое длинное
        let mut sorted_indices: Vec<usize> = (0..query.words.len()).collect();
        sorted_indices.sort_by(|&a, &b| {
            let len_a = query.words[a].original.chars().count();
            let len_b = query.words[b].original.chars().count();
            len_b.cmp(&len_a).then_with(|| a.cmp(&b))
        });

        for &q_idx in &sorted_indices {
            let q_word = &query.words[q_idx];
            for word in WordIter::new(text) {
                if word_matches(word.text, q_word, morph) {
                    match_range = Some((word.start, word.end));
                    break;
                }
            }
            if match_range.is_some() {
                break;
            }
        }
    }

    let Some((match_start, match_end)) = match_range else {
        return Snippet::default();
    };

    // 3. Вычисляем безопасные границы окна по символам UTF-8
    let start_byte = text[..match_start]
        .char_indices()
        .rev()
        .nth(before.saturating_sub(1))
        .map(|(idx, _)| idx)
        .unwrap_or(0);

    let end_byte = text[match_end..]
        .char_indices()
        .nth(after)
        .map(|(idx, _)| match_end + idx)
        .unwrap_or(text.len());

    let window_raw = &text[start_byte..end_byte];

    // 4. Схлопываем пробельные символы (множественные пробелы, \t, \n, \r -> один пробел)
    let mut compact = String::with_capacity(window_raw.len());
    let mut in_whitespace = false;
    for c in window_raw.chars() {
        if c.is_whitespace() {
            if !in_whitespace {
                compact.push(' ');
                in_whitespace = true;
            }
        } else {
            compact.push(c);
            in_whitespace = false;
        }
    }

    // 5. Размечаем сегменты: подсвечиваем ВСЕ совпавшие слова в окне
    let mut segments = Vec::new();
    let mut cursor = 0;

    for word in WordIter::new(&compact) {
        if word.start > cursor {
            let sep = &compact[cursor..word.start];
            add_or_merge_unmatched(&mut segments, sep);
        }

        let is_match = query
            .words
            .iter()
            .any(|qw| word_matches(word.text, qw, morph));

        if is_match {
            segments.push((word.text.to_string(), true));
        } else {
            add_or_merge_unmatched(&mut segments, word.text);
        }

        cursor = word.end;
    }

    if cursor < compact.len() {
        let tail = &compact[cursor..];
        add_or_merge_unmatched(&mut segments, tail);
    }

    Snippet { segments }
}

#[inline]
fn add_or_merge_unmatched(segments: &mut Vec<(String, bool)>, text: &str) {
    if text.is_empty() {
        return;
    }
    if let Some((last_text, matched)) = segments.last_mut()
        && !*matched
    {
        last_text.push_str(text);
        return;
    }
    segments.push((text.to_string(), false));
}

/// Потоковое построение фрагмента из сжатого zstd-текста без распаковки всего документа.
/// Повреждённые сжатые данные или пустой срез возвращают `Err` (R2-В-43).
pub fn build_snippet_compressed(
    compressed: &[u8],
    query: &ParsedQuery,
    morph: Option<&Morph>,
    before: usize,
    after: usize,
    prefer_phrase: bool,
) -> Result<Snippet, std::io::Error> {
    if compressed.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "пустые сжатые данные",
        ));
    }

    if query.words.is_empty() {
        let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
        let mut probe = [0u8; 1];
        let _ = decoder.read(&mut probe)?;
        return Ok(Snippet::default());
    }

    // 1. Если prefer_phrase и слов > 1, ищем сначала фразу в потоке
    if prefer_phrase && query.words.len() > 1 {
        let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
        let mut chunk_buf = [0u8; 64 * 1024];
        let mut pending_bytes = Vec::new();
        let mut window = String::new();

        const OVERLAP_CHARS: usize = 600;

        loop {
            let n = decoder.read(&mut chunk_buf)?;
            if n == 0 {
                if !pending_bytes.is_empty()
                    && let Ok(s) = std::str::from_utf8(&pending_bytes)
                {
                    window.push_str(s);
                }
                if let Some((match_start, match_end)) = find_sequence(&window, query, morph) {
                    return Ok(build_snippet_from_window(
                        &window,
                        match_start,
                        match_end,
                        query,
                        morph,
                        before,
                        after,
                    ));
                }
                break;
            }

            pending_bytes.extend_from_slice(&chunk_buf[..n]);
            let valid_up_to = match std::str::from_utf8(&pending_bytes) {
                Ok(_) => pending_bytes.len(),
                Err(e) => e.valid_up_to(),
            };

            if valid_up_to > 0 {
                if let Ok(s) = std::str::from_utf8(&pending_bytes[..valid_up_to]) {
                    window.push_str(s);
                }
                pending_bytes.drain(..valid_up_to);
            }

            if let Some((match_start, match_end)) = find_sequence(&window, query, morph) {
                return Ok(build_snippet_from_window(
                    &window,
                    match_start,
                    match_end,
                    query,
                    morph,
                    before,
                    after,
                ));
            }

            let char_count = window.chars().count();
            if char_count > OVERLAP_CHARS * 2 {
                let cut_char = char_count - OVERLAP_CHARS;
                let cut_byte = window
                    .char_indices()
                    .nth(cut_char)
                    .map(|(idx, _)| idx)
                    .unwrap_or(window.len());

                let mut word_boundary = cut_byte;
                let remaining = &window[cut_byte..];
                for (rel_idx, c) in remaining.char_indices() {
                    if c.is_alphanumeric() {
                        word_boundary = cut_byte + rel_idx;
                        break;
                    }
                }
                if word_boundary > 0 && word_boundary < window.len() {
                    window.drain(..word_boundary);
                }
            }
        }
    }

    // 2. Ищем якорное слово в потоке (или фраза не найдена, откат к слову R1-А-139 / R2-А-31)
    let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
    let mut chunk_buf = [0u8; 64 * 1024];
    let mut pending_bytes = Vec::new();
    let mut window = String::new();

    const OVERLAP_CHARS: usize = 600;

    let mut sorted_indices: Vec<usize> = (0..query.words.len()).collect();
    sorted_indices.sort_by(|&a, &b| {
        let len_a = query.words[a].original.chars().count();
        let len_b = query.words[b].original.chars().count();
        len_b.cmp(&len_a).then_with(|| a.cmp(&b))
    });

    loop {
        let n = decoder.read(&mut chunk_buf)?;
        if n == 0 {
            if !pending_bytes.is_empty()
                && let Ok(s) = std::str::from_utf8(&pending_bytes)
            {
                window.push_str(s);
            }
            if let Some((match_start, match_end)) =
                find_anchor_in_window(&window, query, &sorted_indices, morph)
            {
                return Ok(build_snippet_from_window(
                    &window,
                    match_start,
                    match_end,
                    query,
                    morph,
                    before,
                    after,
                ));
            }
            return Ok(Snippet::default());
        }

        pending_bytes.extend_from_slice(&chunk_buf[..n]);
        let valid_up_to = match std::str::from_utf8(&pending_bytes) {
            Ok(_) => pending_bytes.len(),
            Err(e) => e.valid_up_to(),
        };

        if valid_up_to > 0 {
            if let Ok(s) = std::str::from_utf8(&pending_bytes[..valid_up_to]) {
                window.push_str(s);
            }
            pending_bytes.drain(..valid_up_to);
        }

        if let Some((match_start, match_end)) =
            find_anchor_in_window(&window, query, &sorted_indices, morph)
        {
            return Ok(build_snippet_from_window(
                &window,
                match_start,
                match_end,
                query,
                morph,
                before,
                after,
            ));
        }

        let char_count = window.chars().count();
        if char_count > OVERLAP_CHARS * 2 {
            let cut_char = char_count - OVERLAP_CHARS;
            let cut_byte = window
                .char_indices()
                .nth(cut_char)
                .map(|(idx, _)| idx)
                .unwrap_or(window.len());

            let mut word_boundary = cut_byte;
            let remaining = &window[cut_byte..];
            for (rel_idx, c) in remaining.char_indices() {
                if c.is_alphanumeric() {
                    word_boundary = cut_byte + rel_idx;
                    break;
                }
            }
            if word_boundary > 0 && word_boundary < window.len() {
                window.drain(..word_boundary);
            }
        }
    }
}

fn find_anchor_in_window(
    window: &str,
    query: &ParsedQuery,
    sorted_indices: &[usize],
    morph: Option<&Morph>,
) -> Option<(usize, usize)> {
    for &q_idx in sorted_indices {
        let q_word = &query.words[q_idx];
        for word in WordIter::new(window) {
            if word_matches(word.text, q_word, morph) {
                return Some((word.start, word.end));
            }
        }
    }
    None
}

fn build_snippet_from_window(
    window: &str,
    match_start: usize,
    match_end: usize,
    query: &ParsedQuery,
    morph: Option<&Morph>,
    before: usize,
    after: usize,
) -> Snippet {
    let start_byte = window[..match_start]
        .char_indices()
        .rev()
        .nth(before.saturating_sub(1))
        .map(|(idx, _)| idx)
        .unwrap_or(0);

    let end_byte = window[match_end..]
        .char_indices()
        .nth(after)
        .map(|(idx, _)| match_end + idx)
        .unwrap_or(window.len());

    let window_raw = &window[start_byte..end_byte];

    let mut compact = String::with_capacity(window_raw.len());
    let mut in_whitespace = false;
    for c in window_raw.chars() {
        if c.is_whitespace() {
            if !in_whitespace {
                compact.push(' ');
                in_whitespace = true;
            }
        } else {
            compact.push(c);
            in_whitespace = false;
        }
    }

    let mut segments = Vec::new();
    let mut cursor = 0;

    for word in WordIter::new(&compact) {
        if word.start > cursor {
            let sep = &compact[cursor..word.start];
            add_or_merge_unmatched(&mut segments, sep);
        }

        let is_match = query
            .words
            .iter()
            .any(|qw| word_matches(word.text, qw, morph));

        if is_match {
            segments.push((word.text.to_string(), true));
        } else {
            add_or_merge_unmatched(&mut segments, word.text);
        }

        cursor = word.end;
    }

    if cursor < compact.len() {
        let tail = &compact[cursor..];
        add_or_merge_unmatched(&mut segments, tail);
    }

    Snippet { segments }
}

/// Создает фрагмент с подсветкой в виде строки со скобками `⟦...⟧`.
/// Совместимо по интерфейсу с прежним Rust-клиентом.
#[inline]
pub fn make_snippet(
    compressed: &[u8],
    query: &ParsedQuery,
    morph: Option<&Morph>,
    prefer_phrase: bool,
) -> Result<String, std::io::Error> {
    let snippet = build_snippet_compressed(compressed, query, morph, 140, 300, prefer_phrase)?;
    Ok(snippet.to_marked_string())
}

use std::io::Read;

use crate::morph::Morph;
use crate::query::{ParsedQuery, word_matches};

#[derive(Clone, Copy, Debug)]
pub struct WordSpan<'a> {
    pub start: usize,
    pub end: usize,
    pub text: &'a str,
}

pub struct WordIter<'a> {
    text: &'a str,
    cursor: usize,
}

impl<'a> WordIter<'a> {
    #[inline]
    pub fn new(text: &'a str) -> Self {
        Self { text, cursor: 0 }
    }
}

impl<'a> Iterator for WordIter<'a> {
    type Item = WordSpan<'a>;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let remaining = &self.text[self.cursor..];
        let mut start_rel = None;
        for (rel_idx, c) in remaining.char_indices() {
            if c.is_alphanumeric() {
                start_rel = Some(rel_idx);
                break;
            }
        }
        let start_rel = start_rel?;
        let start = self.cursor + start_rel;

        let from_start = &self.text[start..];
        let mut end_rel = from_start.len();
        for (rel_idx, c) in from_start.char_indices() {
            if !c.is_alphanumeric() {
                end_rel = rel_idx;
                break;
            }
        }
        let end = start + end_rel;
        self.cursor = end;
        Some(WordSpan {
            start,
            end,
            text: &self.text[start..end],
        })
    }
}

/// Находит первое место, где слова запроса идут ПОДРЯД в порядке запроса
/// (между ними только не-буквы; любое слово текста — граница),
/// каждое слово совпадает через `word_matches` (форма, лексема или префикс).
///
/// Возвращает `Some((start_byte, end_byte))` в `text`.
pub fn find_sequence(
    text: &str,
    query: &ParsedQuery,
    morph: Option<&Morph>,
) -> Option<(usize, usize)> {
    let k = query.words.len();
    if k == 0 || text.is_empty() {
        return None;
    }

    if k == 1 {
        let q_word = &query.words[0];
        for word in WordIter::new(text) {
            if word_matches(word.text, q_word, morph) {
                return Some((word.start, word.end));
            }
        }
        return None;
    }

    let mut ring: Vec<WordSpan> = vec![
        WordSpan {
            start: 0,
            end: 0,
            text: ""
        };
        k
    ];
    let mut count = 0usize;
    let mut head = 0usize;

    for word in WordIter::new(text) {
        ring[head] = word;
        head = (head + 1) % k;
        count += 1;

        if count >= k {
            let mut all_match = true;
            for i in 0..k {
                let slot = (head + i) % k;
                if !word_matches(ring[slot].text, &query.words[i], morph) {
                    all_match = false;
                    break;
                }
            }
            if all_match {
                let start = ring[head].start;
                let end = ring[(head + k - 1) % k].end;
                return Some((start, end));
            }
        }
    }

    None
}

/// Проверяет, содержится ли строгая фраза в тексте.
#[inline]
pub fn contains_sequence(text: &str, query: &ParsedQuery, morph: Option<&Morph>) -> bool {
    find_sequence(text, query, morph).is_some()
}

/// Потоковый поиск фразы в сжатом тексте zstd без полной распаковки всего текста в память.
/// Использует скользящее окно с нахлёстом (R1-А-139).
///
/// Возвращает `Some((start_byte, end_byte))` в распакованном потоке, либо `Err` при битых данных.
pub fn find_sequence_compressed(
    compressed: &[u8],
    query: &ParsedQuery,
    morph: Option<&Morph>,
) -> Result<Option<(usize, usize)>, std::io::Error> {
    if compressed.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "пустые сжатые данные",
        ));
    }

    if query.words.is_empty() {
        // Проверяем валидность сжатых данных попыткой декодирования первого блока
        let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
        let mut probe = [0u8; 1];
        let _ = decoder.read(&mut probe)?;
        return Ok(None);
    }

    let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
    let mut chunk_buf = [0u8; 64 * 1024];
    let mut pending_bytes = Vec::new();
    let mut window = String::new();
    let mut global_window_start = 0usize;

    const OVERLAP_CHARS: usize = 600;

    loop {
        let n = decoder.read(&mut chunk_buf)?;
        if n == 0 {
            // Конец потока
            if !pending_bytes.is_empty() {
                if let Ok(s) = std::str::from_utf8(&pending_bytes) {
                    window.push_str(s);
                }
                pending_bytes.clear();
            }
            if let Some((start, end)) = find_sequence(&window, query, morph) {
                return Ok(Some((
                    global_window_start + start,
                    global_window_start + end,
                )));
            }
            return Ok(None);
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

        if let Some((start, end)) = find_sequence(&window, query, morph) {
            return Ok(Some((
                global_window_start + start,
                global_window_start + end,
            )));
        }

        // Сохраняем нахлёст окна на границе слова
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
                global_window_start += word_boundary;
                window.drain(..word_boundary);
            }
        }
    }
}

/// Потоковая проверка наличия фразы в сжатом тексте.
#[inline]
pub fn compressed_contains_sequence(
    compressed: &[u8],
    query: &ParsedQuery,
    morph: Option<&Morph>,
) -> Result<bool, std::io::Error> {
    find_sequence_compressed(compressed, query, morph).map(|res| res.is_some())
}

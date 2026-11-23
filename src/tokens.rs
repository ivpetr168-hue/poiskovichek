use tantivy_tokenizer_api::{Token, TokenStream, Tokenizer};

use crate::morph::Morph;

#[derive(Clone, Default)]
pub struct PoiskTokenizer {
    morph: Option<Morph>,
}

impl PoiskTokenizer {
    pub fn new(morph: Option<Morph>) -> Self {
        Self { morph }
    }

    pub fn with_morph(morph: Morph) -> Self {
        Self { morph: Some(morph) }
    }

    pub fn without_morph() -> Self {
        Self { morph: None }
    }
}

impl Tokenizer for PoiskTokenizer {
    type TokenStream<'a> = PoiskTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        PoiskTokenStream::new(text, self.morph.as_ref())
    }
}

const CACHE_SIZE: usize = 128;

#[derive(Clone)]
struct CacheEntry<'a> {
    hash: u64,
    word: String,
    lexemes: &'a [u32],
}

pub struct PoiskTokenStream<'a> {
    bytes: &'a [u8],
    byte_idx: usize,
    morph: Option<&'a Morph>,
    token: Token,
    current_position: usize,
    pending_lexemes: &'a [u32],
    pending_idx: usize,
    current_word_from: usize,
    current_word_to: usize,
    current_word_pos: usize,
    cache: Vec<CacheEntry<'a>>,
}

impl<'a> PoiskTokenStream<'a> {
    pub fn new(text: &'a str, morph: Option<&'a Morph>) -> Self {
        let mut cache = Vec::with_capacity(CACHE_SIZE);
        for _ in 0..CACHE_SIZE {
            cache.push(CacheEntry {
                hash: 0,
                word: String::with_capacity(32),
                lexemes: &[],
            });
        }
        Self {
            bytes: text.as_bytes(),
            byte_idx: 0,
            morph,
            token: Token::default(),
            current_position: 0,
            pending_lexemes: &[],
            pending_idx: 0,
            current_word_from: 0,
            current_word_to: 0,
            current_word_pos: 0,
            cache,
        }
    }
}

#[inline(always)]
fn next_char_boundary(bytes: &[u8], mut idx: usize) -> usize {
    idx += 1;
    while idx < bytes.len() && (bytes[idx] & 0xC0) == 0x80 {
        idx += 1;
    }
    idx
}

#[inline(always)]
fn push_u32_prefixed(buf: &mut String, prefix: u8, mut val: u32) {
    buf.clear();
    let mut temp = [0u8; 11];
    temp[0] = prefix;
    if val == 0 {
        temp[1] = b'0';
        unsafe {
            buf.as_mut_vec().extend_from_slice(&temp[..2]);
        }
        return;
    }
    let mut digits = [0u8; 10];
    let mut d_len = 0;
    while val > 0 {
        digits[d_len] = b'0' + (val % 10) as u8;
        val /= 10;
        d_len += 1;
    }
    let mut out_len = 1;
    while d_len > 0 {
        d_len -= 1;
        temp[out_len] = digits[d_len];
        out_len += 1;
    }
    unsafe {
        buf.as_mut_vec().extend_from_slice(&temp[..out_len]);
    }
}

impl<'a> TokenStream for PoiskTokenStream<'a> {
    fn advance(&mut self) -> bool {
        // 1. Если для текущего слова ещё есть невыданные лексемы
        if self.pending_idx < self.pending_lexemes.len() {
            let lexeme_id = self.pending_lexemes[self.pending_idx];
            push_u32_prefixed(&mut self.token.text, b'#', lexeme_id);
            self.token.position = self.current_word_pos;
            self.token.offset_from = self.current_word_from;
            self.token.offset_to = self.current_word_to;
            self.token.position_length = 1;
            self.pending_idx += 1;
            return true;
        }

        self.pending_lexemes = &[];
        self.pending_idx = 0;

        let bytes = self.bytes;
        let mut idx = self.byte_idx;

        while idx < bytes.len() {
            // Пропускаем символы, не являющиеся буквами или цифрами
            let b = bytes[idx];
            if b < 128 {
                if !b.is_ascii_alphanumeric() {
                    idx += 1;
                    continue;
                }
            } else if (b == 0xD0 || b == 0xD1) && idx + 1 < bytes.len() {
                let b2 = bytes[idx + 1];
                let is_cyr = (b == 0xD0 && (matches!(b2, 0x90..=0xBF | 0x81)))
                    || (b == 0xD1 && (matches!(b2, 0x80..=0x8F | 0x91)));
                if !is_cyr {
                    let end = next_char_boundary(bytes, idx);
                    if let Ok(s) = std::str::from_utf8(&bytes[idx..end]) {
                        if !s.chars().next().is_some_and(|c| c.is_alphanumeric()) {
                            idx = end;
                            continue;
                        }
                    } else {
                        idx = end;
                        continue;
                    }
                }
            } else {
                let end = next_char_boundary(bytes, idx);
                if let Ok(s) = std::str::from_utf8(&bytes[idx..end]) {
                    if !s.chars().next().is_some_and(|c| c.is_alphanumeric()) {
                        idx = end;
                        continue;
                    }
                } else {
                    idx = end;
                    continue;
                }
            }

            // Найдено начало слова!
            let word_start = idx;
            self.token.text.clear();
            let mut char_count = 0;
            let mut is_all_cyrillic = true;

            // Сканируем и нормализуем слово за один проход
            while idx < bytes.len() {
                let b = bytes[idx];
                if b < 128 {
                    if !b.is_ascii_alphanumeric() {
                        break;
                    }
                    is_all_cyrillic = false;
                    char_count += 1;
                    let lower_b = if b.is_ascii_uppercase() { b + 32 } else { b };
                    unsafe {
                        self.token.text.as_mut_vec().push(lower_b);
                    }
                    idx += 1;
                } else if b == 0xD0 && idx + 1 < bytes.len() {
                    let b2 = bytes[idx + 1];
                    if b2 == 0x81 {
                        char_count += 1;
                        unsafe {
                            let vec = self.token.text.as_mut_vec();
                            vec.push(0xD0);
                            vec.push(0xB5);
                        }
                        idx += 2;
                    } else if (0x90..=0x9F).contains(&b2) {
                        char_count += 1;
                        unsafe {
                            let vec = self.token.text.as_mut_vec();
                            vec.push(0xD0);
                            vec.push(b2 + 0x20);
                        }
                        idx += 2;
                    } else if (0xA0..=0xAF).contains(&b2) {
                        char_count += 1;
                        unsafe {
                            let vec = self.token.text.as_mut_vec();
                            vec.push(0xD1);
                            vec.push(b2 - 0x20);
                        }
                        idx += 2;
                    } else if (0xB0..=0xBF).contains(&b2) {
                        char_count += 1;
                        unsafe {
                            let vec = self.token.text.as_mut_vec();
                            vec.push(0xD0);
                            vec.push(b2);
                        }
                        idx += 2;
                    } else {
                        let end = next_char_boundary(bytes, idx);
                        if let Ok(s) = std::str::from_utf8(&bytes[idx..end]) {
                            if let Some(c) = s.chars().next() {
                                if !c.is_alphanumeric() {
                                    break;
                                }
                                is_all_cyrillic = false;
                                char_count += 1;
                                for lc in c.to_lowercase() {
                                    self.token.text.push(if lc == 'ё' { 'е' } else { lc });
                                }
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                        idx = end;
                    }
                } else if b == 0xD1 && idx + 1 < bytes.len() {
                    let b2 = bytes[idx + 1];
                    if b2 == 0x91 {
                        char_count += 1;
                        unsafe {
                            let vec = self.token.text.as_mut_vec();
                            vec.push(0xD0);
                            vec.push(0xB5);
                        }
                        idx += 2;
                    } else if (0x80..=0x8F).contains(&b2) {
                        char_count += 1;
                        unsafe {
                            let vec = self.token.text.as_mut_vec();
                            vec.push(0xD1);
                            vec.push(b2);
                        }
                        idx += 2;
                    } else {
                        let end = next_char_boundary(bytes, idx);
                        if let Ok(s) = std::str::from_utf8(&bytes[idx..end]) {
                            if let Some(c) = s.chars().next() {
                                if !c.is_alphanumeric() {
                                    break;
                                }
                                is_all_cyrillic = false;
                                char_count += 1;
                                for lc in c.to_lowercase() {
                                    self.token.text.push(if lc == 'ё' { 'е' } else { lc });
                                }
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                        idx = end;
                    }
                } else {
                    let end = next_char_boundary(bytes, idx);
                    if let Ok(s) = std::str::from_utf8(&bytes[idx..end]) {
                        if let Some(c) = s.chars().next() {
                            if !c.is_alphanumeric() {
                                break;
                            }
                            is_all_cyrillic = false;
                            char_count += 1;
                            for lc in c.to_lowercase() {
                                self.token.text.push(if lc == 'ё' { 'е' } else { lc });
                            }
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                    idx = end;
                }
            }

            let word_end = idx;
            self.byte_idx = word_end;

            // Если слово длиннее 64 символов, пропускаем его, но позиция всё равно занимается
            if char_count > 64 {
                self.current_position += 1;
                continue;
            }

            let pos = self.current_position;
            self.current_position += 1;

            self.current_word_pos = pos;
            self.current_word_from = word_start;
            self.current_word_to = word_end;

            self.token.position = pos;
            self.token.offset_from = word_start;
            self.token.offset_to = word_end;
            self.token.position_length = 1;

            if char_count >= 3
                && is_all_cyrillic
                && let Some(morph) = self.morph
            {
                // Быстрый хэш FNV-1a для кэша
                let mut h = 0xcbf29ce484222325u64;
                for &b in self.token.text.as_bytes() {
                    h = (h ^ (b as u64)).wrapping_mul(0x100000001b3);
                }
                let slot = (h as usize) & (CACHE_SIZE - 1);
                if self.cache[slot].hash == h && self.cache[slot].word == self.token.text {
                    let lexemes = self.cache[slot].lexemes;
                    if !lexemes.is_empty() {
                        self.pending_lexemes = lexemes;
                        self.pending_idx = 0;
                    }
                } else {
                    let lexemes = morph.lexemes(&self.token.text);
                    self.cache[slot].hash = h;
                    self.cache[slot].word.clear();
                    self.cache[slot].word.push_str(&self.token.text);
                    self.cache[slot].lexemes = lexemes;
                    if !lexemes.is_empty() {
                        self.pending_lexemes = lexemes;
                        self.pending_idx = 0;
                    }
                }
            }

            return true;
        }

        self.byte_idx = bytes.len();
        false
    }

    fn token(&self) -> &Token {
        &self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        &mut self.token
    }
}

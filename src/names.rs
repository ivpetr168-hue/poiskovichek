use std::collections::HashMap;

use rayon::prelude::*;
use rusqlite::Connection;

use crate::catalog::{Catalog, FileRecord};
use crate::journal::{self, Level};
use crate::morph::{Morph, normalize};
use crate::phrase::WordIter;
use crate::query::{ParsedQuery, QueryWord, QueryWordMatcher};

/// Название категории, в которую попадают файлы, когда таблица категорий переполнена.
const OVERFLOW_CATEGORY: &str = "прочее";
/// Число обычных идентификаторов категорий (id 0..=254); id 255 занят общей категорией «прочее».
const CATEGORY_NORMAL_LIMIT: usize = u8::MAX as usize;
/// Число обычных идентификаторов расширений (id 0..=65534); id 65535 занят общей записью «без расширения».
const EXTENSION_NORMAL_LIMIT: usize = u16::MAX as usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SearchMode {
    #[default]
    StrictPhrase,
    AllWords,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NameFilter {
    pub source_id: Option<i64>,
    pub volume_id: Option<i64>,
    pub category: Option<String>,
    pub extension: Option<String>,
    pub min_size: Option<u64>,
    pub available_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NameMatch {
    pub file_index: usize,
    pub id: i64,
    pub in_name: bool,
    pub in_path: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameHit {
    pub id: i64,
    pub path: String,
    pub name: String,
    pub extension: String,
    pub size: u64,
    pub modified_ns: i64,
    pub category: String,
    pub text_sha: Option<String>,
    pub source_id: Option<i64>,
    pub volume_id: Option<i64>,
    pub available: bool,
    pub in_name: bool,
    pub in_path: bool,
}

#[derive(Clone, Debug, Default)]
pub struct TokenTable {
    pub words: Vec<String>,
    pub word_to_id: HashMap<String, u32>,
    pub word_lexemes: Vec<Vec<u32>>,
}

impl TokenTable {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_insert(&mut self, word: &str, morph: Option<&Morph>) -> u32 {
        if let Some(&id) = self.word_to_id.get(word) {
            return id;
        }

        let id = self.words.len() as u32;
        let mut lexemes = Vec::new();
        if let Some(m) = morph {
            let count = word.chars().count();
            let is_cyr = word.chars().all(|c| matches!(c, 'а'..='я'));
            if count >= 3 && is_cyr {
                lexemes.extend_from_slice(m.lexemes(word));
            }
        }

        self.words.push(word.to_string());
        self.word_lexemes.push(lexemes);
        self.word_to_id.insert(word.to_string(), id);
        id
    }
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct CompactFile {
    pub id: i64,
    pub size: u64,
    pub modified_ns: i64,
    pub source_id: i64, // -1 если None
    pub volume_id: i64, // -1 если None
    pub path_offset: u32,
    pub path_len: u32,
    pub name_offset: u32,
    pub name_len: u32,
    pub name_words_offset: u32,
    pub name_words_len: u16,
    pub path_words_offset: u32,
    pub path_words_len: u16,
    pub category_id: u8,
    pub ext_id: u16,
    pub flags: u8,   // bit 0: available, bit 1: deleted
    pub sha_id: u32, // u32::MAX если нет
}

impl CompactFile {
    #[inline(always)]
    pub fn is_available(&self) -> bool {
        (self.flags & 1) != 0
    }

    #[inline(always)]
    pub fn is_deleted(&self) -> bool {
        (self.flags & 2) != 0
    }

    #[inline(always)]
    pub fn set_deleted(&mut self, val: bool) {
        if val {
            self.flags |= 2;
        } else {
            self.flags &= !2;
        }
    }
}

pub struct NamesIndex {
    pub files: Vec<CompactFile>,
    pub id_to_index: HashMap<i64, usize>,
    pub tokens: TokenTable,
    pub path_bytes: Vec<u8>,
    pub name_bytes: Vec<u8>,
    pub words_pool: Vec<u32>,
    pub categories: Vec<String>,
    pub category_to_id: HashMap<String, u8>,
    pub extensions: Vec<String>,
    pub ext_to_id: HashMap<String, u16>,
    pub shas: Vec<String>,
    pub sha_to_id: HashMap<String, u32>,
}

impl Default for NamesIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl NamesIndex {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            id_to_index: HashMap::new(),
            tokens: TokenTable::new(),
            path_bytes: Vec::new(),
            name_bytes: Vec::new(),
            words_pool: Vec::new(),
            categories: Vec::new(),
            category_to_id: HashMap::new(),
            extensions: Vec::new(),
            ext_to_id: HashMap::new(),
            shas: Vec::new(),
            sha_to_id: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.files.iter().filter(|f| !f.is_deleted()).count()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Идентификатор категории. Когда обычные идентификаторы исчерпаны (255 категорий),
    /// новые категории получают общий идентификатор категории «прочее».
    fn intern_category(&mut self, cat: &str) -> u8 {
        let norm = cat.to_lowercase();
        if let Some(&id) = self.category_to_id.get(&norm) {
            return id;
        }
        if self.categories.len() >= CATEGORY_NORMAL_LIMIT {
            return self.overflow_category_id();
        }
        let id = self.categories.len() as u8;
        self.categories.push(cat.to_string());
        self.category_to_id.insert(norm, id);
        id
    }

    fn overflow_category_id(&mut self) -> u8 {
        if let Some(&id) = self.category_to_id.get(OVERFLOW_CATEGORY) {
            return id;
        }
        // Последний свободный идентификатор (255) резервируется под «прочее».
        let id = self.categories.len().min(u8::MAX as usize) as u8;
        if (id as usize) == self.categories.len() {
            self.categories.push(OVERFLOW_CATEGORY.to_string());
            self.category_to_id
                .insert(OVERFLOW_CATEGORY.to_string(), id);
        }
        id
    }

    /// Идентификатор расширения. Когда обычные идентификаторы исчерпаны (65535 расширений),
    /// новые расширения получают общий идентификатор записи «без расширения».
    fn intern_extension(&mut self, ext: &str) -> u16 {
        let norm = ext.trim_start_matches('.').to_lowercase();
        if let Some(&id) = self.ext_to_id.get(&norm) {
            return id;
        }
        if self.extensions.len() >= EXTENSION_NORMAL_LIMIT {
            return self.overflow_extension_id();
        }
        let id = self.extensions.len() as u16;
        self.extensions.push(ext.to_string());
        self.ext_to_id.insert(norm, id);
        id
    }

    fn overflow_extension_id(&mut self) -> u16 {
        if let Some(&id) = self.ext_to_id.get("") {
            return id;
        }
        // Последний свободный идентификатор (65535) резервируется под пустое расширение.
        let id = self.extensions.len().min(u16::MAX as usize) as u16;
        if (id as usize) == self.extensions.len() {
            self.extensions.push(String::new());
            self.ext_to_id.insert(String::new(), id);
        }
        id
    }

    fn intern_sha(&mut self, sha: Option<&str>) -> u32 {
        let Some(sha) = sha else {
            return u32::MAX;
        };
        if sha.is_empty() {
            return u32::MAX;
        }
        if let Some(&id) = self.sha_to_id.get(sha) {
            return id;
        }
        // u32::MAX означает «текста нет»: при переполнении таблицы файл остаётся без текста.
        if self.shas.len() >= u32::MAX as usize {
            return u32::MAX;
        }
        let id = self.shas.len() as u32;
        self.shas.push(sha.to_string());
        self.sha_to_id.insert(sha.to_string(), id);
        id
    }

    pub fn add_file(&mut self, file: &FileRecord, morph: Option<&Morph>) {
        let Some(id) = file.id else {
            return;
        };

        // Смещения хранятся в u32: если пулы не вмещают запись, файл пропускается,
        // уже записанные данные не портятся.
        let words_reserve = 2 * u16::MAX as usize;
        let fits = u32::try_from(file.path.len()).is_ok()
            && u32::try_from(file.name.len()).is_ok()
            && u32::try_from(self.path_bytes.len() + file.path.len()).is_ok()
            && u32::try_from(self.name_bytes.len() + file.name.len()).is_ok()
            && u32::try_from(self.words_pool.len() + words_reserve).is_ok()
            && self.tokens.words.len() < u32::MAX as usize - words_reserve
            && self.files.len() < u32::MAX as usize;
        if !fits {
            journal::log(
                Level::Warn,
                "names",
                &format!(
                    "Файл пропущен в именах из-за переполнения таблиц: {}",
                    file.path
                ),
            );
            return;
        }

        // Токенизируем имя (слов в имени не больше u16::MAX; остальные не учитываются)
        let name_words_offset = self.words_pool.len() as u32;
        let mut name_words_len = 0u16;
        for word in WordIter::new(&file.name) {
            let norm = normalize(word.text);
            if !norm.is_empty() {
                let Some(next_len) = name_words_len.checked_add(1) else {
                    break;
                };
                let wid = self.tokens.get_or_insert(&norm, morph);
                self.words_pool.push(wid);
                name_words_len = next_len;
            }
        }

        // Токенизируем путь (слов в пути не больше u16::MAX; остальные не учитываются)
        let path_words_offset = self.words_pool.len() as u32;
        let mut path_words_len = 0u16;
        for word in WordIter::new(&file.path) {
            let norm = normalize(word.text);
            if !norm.is_empty() {
                let Some(next_len) = path_words_len.checked_add(1) else {
                    break;
                };
                let wid = self.tokens.get_or_insert(&norm, morph);
                self.words_pool.push(wid);
                path_words_len = next_len;
            }
        }

        let path_offset = self.path_bytes.len() as u32;
        self.path_bytes.extend_from_slice(file.path.as_bytes());
        let path_len = file.path.len() as u32;

        let name_offset = self.name_bytes.len() as u32;
        self.name_bytes.extend_from_slice(file.name.as_bytes());
        let name_len = file.name.len() as u32;

        let category_id = self.intern_category(&file.category);
        let ext_id = self.intern_extension(&file.extension);
        let sha_id = self.intern_sha(file.text_sha.as_deref());

        let mut flags = 0u8;
        if file.available {
            flags |= 1;
        }

        let compact = CompactFile {
            id,
            size: file.size,
            modified_ns: file.modified_ns,
            source_id: file.source_id.unwrap_or(-1),
            volume_id: file.volume_id.unwrap_or(-1),
            category_id,
            ext_id,
            flags,
            sha_id,
            path_offset,
            path_len,
            name_offset,
            name_len,
            name_words_offset,
            name_words_len,
            path_words_offset,
            path_words_len,
        };

        let index = self.files.len();
        self.files.push(compact);
        self.id_to_index.insert(id, index);
    }

    pub fn update_file(&mut self, file: &FileRecord, morph: Option<&Morph>) {
        let Some(id) = file.id else {
            return;
        };
        if let Some(&idx) = self.id_to_index.get(&id) {
            self.files[idx].set_deleted(true);
        }
        self.add_file(file, morph);
    }

    pub fn remove_file(&mut self, id: i64) {
        if let Some(&idx) = self.id_to_index.get(&id) {
            self.files[idx].set_deleted(true);
            self.id_to_index.remove(&id);
        }
    }

    /// Загружает все записи файлов из базы данных `catalog.sqlite3`.
    pub fn from_catalog(catalog: &Catalog, morph: Option<&Morph>) -> Result<Self, rusqlite::Error> {
        let conn = catalog.connection();
        Self::from_connection(conn, morph)
    }

    /// Загружает все записи файлов напрямую через SQLite-соединение.
    pub fn from_connection(
        conn: &Connection,
        morph: Option<&Morph>,
    ) -> Result<Self, rusqlite::Error> {
        let mut stmt = conn.prepare(
            "SELECT id, path, name, extension, size, modified_ns, category, text_sha, source_id, volume_id, available FROM files WHERE available != -1;",
        )?;

        let mut rows = stmt.query([])?;
        let mut index = Self::new();

        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let path: String = row.get(1)?;
            let name: String = row.get(2)?;
            let extension: String = row.get(3)?;
            let size: i64 = row.get(4)?;
            let modified_ns: i64 = row.get(5)?;
            let category: String = row.get(6)?;
            let text_sha: Option<String> = row.get(7)?;
            let source_id: Option<i64> = row.get(8)?;
            let volume_id: Option<i64> = row.get(9)?;
            let available: i64 = row.get(10)?;

            let record = FileRecord {
                id: Some(id),
                path,
                name,
                extension,
                size: size as u64,
                modified_ns,
                created_ns: 0,
                category,
                inside_archive: false,
                container_id: None,
                content_status: None,
                extraction_version: 0,
                text_sha,
                quick_hash: None,
                sha256: None,
                error: None,
                scan_run: None,
                source_id,
                volume_id,
                available: available != 0,
                rel_path: String::new(),
            };

            index.add_file(&record, morph);
        }

        Ok(index)
    }

    /// Параллельный проход (`rayon`) по записям с фильтрацией.
    /// Возвращает компактные совпадения `NameMatch` с нулевыми аллокациями строк.
    pub fn search_matches(
        &self,
        query: &ParsedQuery,
        filter: &NameFilter,
        mode: SearchMode,
    ) -> Vec<NameMatch> {
        let target_cat_id = filter
            .category
            .as_ref()
            .and_then(|cat| self.category_to_id.get(&cat.to_lowercase()).copied());

        if filter.category.is_some() && target_cat_id.is_none() {
            return Vec::new();
        }

        let target_ext_matches: Option<Vec<bool>> = filter.extension.as_ref().map(|ext_filter| {
            let target = ext_filter.trim_start_matches('.').to_lowercase();
            self.extensions
                .iter()
                .map(|e| e.trim_start_matches('.').to_lowercase() == target)
                .collect()
        });

        if let Some(ref ext_matches) = target_ext_matches
            && !ext_matches.iter().any(|&m| m)
        {
            return Vec::new();
        }

        if query.words.is_empty() {
            return self
                .files
                .par_iter()
                .enumerate()
                .filter(|(_, file)| !file.is_deleted())
                .filter(|(_, file)| !filter.available_only || file.is_available())
                .filter(|(_, file)| {
                    if let Some(src) = filter.source_id
                        && file.source_id != src
                    {
                        return false;
                    }
                    if let Some(vol) = filter.volume_id
                        && file.volume_id != vol
                    {
                        return false;
                    }
                    if let Some(min_size) = filter.min_size
                        && file.size < min_size
                    {
                        return false;
                    }
                    if let Some(cat_id) = target_cat_id
                        && file.category_id != cat_id
                    {
                        return false;
                    }
                    if let Some(ref ext_matches) = target_ext_matches
                        && !ext_matches[file.ext_id as usize]
                    {
                        return false;
                    }
                    true
                })
                .map(|(idx, file)| NameMatch {
                    file_index: idx,
                    id: file.id,
                    in_name: true,
                    in_path: true,
                })
                .collect();
        }

        let mut match_tables: Vec<Vec<bool>> = Vec::with_capacity(query.words.len());
        for q_word in &query.words {
            let table = resolve_query_word(&self.tokens, q_word);
            if !table.iter().any(|&b| b) {
                return Vec::new();
            }
            match_tables.push(table);
        }

        self.files
            .par_iter()
            .enumerate()
            .filter_map(|(idx, file)| {
                if file.is_deleted() {
                    return None;
                }
                if filter.available_only && !file.is_available() {
                    return None;
                }
                if let Some(src) = filter.source_id
                    && file.source_id != src
                {
                    return None;
                }
                if let Some(vol) = filter.volume_id
                    && file.volume_id != vol
                {
                    return None;
                }
                if let Some(min_size) = filter.min_size
                    && file.size < min_size
                {
                    return None;
                }
                if let Some(cat_id) = target_cat_id
                    && file.category_id != cat_id
                {
                    return None;
                }
                if let Some(ref ext_matches) = target_ext_matches
                    && !ext_matches[file.ext_id as usize]
                {
                    return None;
                }

                let name_words = &self.words_pool[file.name_words_offset as usize
                    ..(file.name_words_offset as usize + file.name_words_len as usize)];
                let path_words = &self.words_pool[file.path_words_offset as usize
                    ..(file.path_words_offset as usize + file.path_words_len as usize)];

                let in_name = match mode {
                    SearchMode::StrictPhrase => check_strict_phrase(name_words, &match_tables),
                    SearchMode::AllWords => check_all_words(name_words, &match_tables),
                };

                let in_path = match mode {
                    SearchMode::StrictPhrase => check_strict_phrase(path_words, &match_tables),
                    SearchMode::AllWords => check_all_words(path_words, &match_tables),
                };

                if in_name || in_path {
                    Some(NameMatch {
                        file_index: idx,
                        id: file.id,
                        in_name,
                        in_path,
                    })
                } else {
                    None
                }
            })
            .collect()
    }

    /// Полнотекстовый поиск по имени и путям с возвратом `Vec<NameHit>`.
    pub fn search(
        &self,
        query: &ParsedQuery,
        filter: &NameFilter,
        mode: SearchMode,
    ) -> Vec<NameHit> {
        let matches = self.search_matches(query, filter, mode);
        matches
            .into_iter()
            .map(|m| self.build_hit(&self.files[m.file_index], m.in_name, m.in_path))
            .collect()
    }

    /// Поиск с ограничением числа выдаваемых результатов (страничная выдача).
    pub fn search_limited(
        &self,
        query: &ParsedQuery,
        filter: &NameFilter,
        mode: SearchMode,
        limit: usize,
    ) -> Vec<NameHit> {
        let matches = self.search_matches(query, filter, mode);
        matches
            .into_iter()
            .take(limit)
            .map(|m| self.build_hit(&self.files[m.file_index], m.in_name, m.in_path))
            .collect()
    }

    fn build_hit(&self, file: &CompactFile, in_name: bool, in_path: bool) -> NameHit {
        let path = std::str::from_utf8(
            &self.path_bytes
                [file.path_offset as usize..(file.path_offset + file.path_len) as usize],
        )
        .unwrap_or("")
        .to_string();

        let name = std::str::from_utf8(
            &self.name_bytes
                [file.name_offset as usize..(file.name_offset + file.name_len) as usize],
        )
        .unwrap_or("")
        .to_string();

        let category = self.categories[file.category_id as usize].clone();
        let extension = self.extensions[file.ext_id as usize].clone();

        let text_sha = if file.sha_id != u32::MAX && (file.sha_id as usize) < self.shas.len() {
            Some(self.shas[file.sha_id as usize].clone())
        } else {
            None
        };

        let source_id = if file.source_id >= 0 {
            Some(file.source_id)
        } else {
            None
        };
        let volume_id = if file.volume_id >= 0 {
            Some(file.volume_id)
        } else {
            None
        };

        NameHit {
            id: file.id,
            path,
            name,
            extension,
            size: file.size,
            modified_ns: file.modified_ns,
            category,
            text_sha,
            source_id,
            volume_id,
            available: file.is_available(),
            in_name,
            in_path,
        }
    }
}

#[inline(always)]
fn resolve_query_word(tokens: &TokenTable, q_word: &QueryWord) -> Vec<bool> {
    let mut table = vec![false; tokens.words.len()];
    for (i, w) in tokens.words.iter().enumerate() {
        if word_matches_query(w, &tokens.word_lexemes[i], q_word) {
            table[i] = true;
        }
    }
    table
}

#[inline(always)]
fn word_matches_query(word_text: &str, word_lexemes: &[u32], q_word: &QueryWord) -> bool {
    match &q_word.matcher {
        QueryWordMatcher::Exact(tokens) => {
            if tokens.iter().any(|t| t == word_text) {
                return true;
            }
            for &lid in word_lexemes {
                for t in tokens {
                    if let Some(num_str) = t.strip_prefix('#')
                        && let Ok(id) = num_str.parse::<u32>()
                        && id == lid
                    {
                        return true;
                    }
                }
            }
            false
        }
        QueryWordMatcher::Prefix(prefix) => word_text.starts_with(prefix),
    }
}

#[inline(always)]
fn check_strict_phrase(words: &[u32], match_tables: &[Vec<bool>]) -> bool {
    let k = match_tables.len();
    if words.len() < k {
        return false;
    }
    for i in 0..=(words.len() - k) {
        let mut matched = true;
        for j in 0..k {
            let wid = words[i + j] as usize;
            if !match_tables[j][wid] {
                matched = false;
                break;
            }
        }
        if matched {
            return true;
        }
    }
    false
}

#[inline(always)]
fn check_all_words(words: &[u32], match_tables: &[Vec<bool>]) -> bool {
    for match_table in match_tables {
        if !words.iter().any(|&wid| match_table[wid as usize]) {
            return false;
        }
    }
    true
}

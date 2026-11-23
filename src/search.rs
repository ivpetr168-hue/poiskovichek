//! Модуль поиска документов по тексту, именам и путям (раздел 2.5 ТЗ).
//!
//! Реализует `SearchEngine`:
//! - читатель индекса tantivy (с автоматической перезагрузкой);
//! - чтение SQLite базы только для чтения (`query_only`, отдельное соединение на поток);
//! - быстрый поиск по именам и путям в оперативной памяти (`NamesIndex`);
//! - группировка по `text_sha` (файлы без текста — по своему id);
//! - сортировка: точная фраза -> BM25 tantivy -> детерминированный ключ;
//! - фильтры и фрагменты текста (`snippet_for`).

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use rusqlite::{Connection, OpenFlags};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::{Index, IndexReader, ReloadPolicy, Searcher, TantivyDocument, Term};

use crate::index::{IndexError, PovtornyiKatalog, create_schema, register_poisk_tokenizer};
use crate::morph::Morph;
use crate::names::{CompactFile, NameFilter, NamesIndex, SearchMode};
use crate::paths::AppPaths;
use crate::phrase::compressed_contains_sequence;
use crate::query::{ParsedQuery, QueryWordMatcher, parse_query_forms};
use crate::settings::Settings;
pub use crate::settings::{MultiWordMode, SortMode, WordFormsMode};
use crate::snippet::{Snippet, build_snippet_compressed};

/// Ошибки подсистемы поиска.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    #[error("База данных каталога не найдена: {0}")]
    DatabaseNotFound(String),
    #[error("Ошибка SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Ошибка Tantivy: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("Ошибка открытия каталога индекса: {0}")]
    OpenDirectory(#[from] tantivy::directory::error::OpenDirectoryError),
    #[error("Ошибка чтения каталога индекса: {0}")]
    OpenRead(#[from] tantivy::directory::error::OpenReadError),
    #[error("Ошибка поискового индекса: {0}")]
    Index(#[from] IndexError),
    #[error("Ошибка ввода-вывода: {0}")]
    Io(#[from] std::io::Error),
}

/// Фильтры поиска по метаданным.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub source_id: Option<i64>,
    pub volume_id: Option<i64>,
    pub category: Option<String>,
    pub extension: Option<String>,
    pub min_size: Option<u64>,
    pub duplicates_only: bool,
    pub available_only: bool,
}

impl SearchFilters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.source_id.is_none()
            && self.volume_id.is_none()
            && self.category.is_none()
            && self.extension.is_none()
            && self.min_size.is_none()
            && !self.duplicates_only
            && !self.available_only
    }
}

impl From<&SearchFilters> for NameFilter {
    fn from(f: &SearchFilters) -> Self {
        NameFilter {
            source_id: f.source_id,
            volume_id: f.volume_id,
            category: f.category.clone(),
            extension: f.extension.clone(),
            min_size: f.min_size,
            available_only: f.available_only,
        }
    }
}

/// Параметры поискового запроса.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchRequest {
    pub text: String,
    pub filters: SearchFilters,
    pub sort: SortMode,
    pub limit: usize,
    pub mode: MultiWordMode,
    pub forms: WordFormsMode,
}

impl Default for SearchRequest {
    fn default() -> Self {
        Self {
            text: String::new(),
            filters: SearchFilters::default(),
            sort: SortMode::Relevance,
            limit: 200,
            mode: MultiWordMode::StrictPhrase,
            forms: WordFormsMode::AllForms,
        }
    }
}

impl SearchRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Default::default()
        }
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    pub fn with_filters(mut self, filters: SearchFilters) -> Self {
        self.filters = filters;
        self
    }

    pub fn with_sort(mut self, sort: SortMode) -> Self {
        self.sort = sort;
        self
    }

    pub fn with_mode(mut self, mode: MultiWordMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn with_forms(mut self, forms: WordFormsMode) -> Self {
        self.forms = forms;
        self
    }
}

/// Трейт для удобной передачи запроса строкой, кортежем или готовой структурой `SearchRequest`.
pub trait IntoSearchRequest {
    fn into_search_request(self, settings: &Settings) -> SearchRequest;
}

impl IntoSearchRequest for &SearchRequest {
    fn into_search_request(self, _settings: &Settings) -> SearchRequest {
        self.clone()
    }
}

impl IntoSearchRequest for SearchRequest {
    fn into_search_request(self, _settings: &Settings) -> SearchRequest {
        self
    }
}

impl IntoSearchRequest for &str {
    fn into_search_request(self, settings: &Settings) -> SearchRequest {
        SearchRequest {
            text: self.to_string(),
            filters: SearchFilters::default(),
            sort: settings.default_sort,
            limit: settings.max_results,
            mode: settings.multi_word_mode,
            forms: settings.word_forms_mode,
        }
    }
}

impl IntoSearchRequest for String {
    fn into_search_request(self, settings: &Settings) -> SearchRequest {
        self.as_str().into_search_request(settings)
    }
}

impl IntoSearchRequest for (&str, usize) {
    fn into_search_request(self, settings: &Settings) -> SearchRequest {
        let mut req = self.0.into_search_request(settings);
        req.limit = self.1;
        req
    }
}

/// Элемент выдачи поиска (сгруппированная карточка документа).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchItem {
    /// Ключ группы: `text_sha` при наличии извлечённого текста, либо `@file:<id>`.
    pub item_key: String,
    /// Алиас для `item_key` для совместимости.
    pub key: String,
    /// SHA-256 текста документа (если текст извлечён).
    pub text_sha: Option<String>,
    /// Алиас для `text_sha`.
    pub text_sha256: Option<String>,
    /// Имя файла (представителя группы).
    pub name: String,
    /// Расширение файла.
    pub extension: String,
    /// Категория файла.
    pub category: String,
    /// Максимальный размер файла среди копий в группе.
    pub size: u64,
    /// Время изменения файла (наибольшее в группе, в наносекундах).
    pub modified_ns: i64,
    /// Число расположений (путей) файла.
    pub path_count: usize,
    /// Все физические пути к файлам группы, отсортированные по алфавиту без учёта регистра.
    pub paths: Vec<String>,
    /// Статус извлечения текста (сырой код из базы).
    pub content_status: Option<String>,
    /// Человекочитаемый статус извлечения: «текст не извлечён: <причина>».
    pub status: Option<String>,
    /// Флаг дубликата (несколько путей или совпадение SHA-256 с группой дубликатов).
    pub is_duplicate: bool,
    /// Попадание точной фразы.
    pub phrase_hit: bool,
    /// Ранг BM25 / релевантности (меньшее значение = релевантнее).
    pub rank: f64,
}

impl SearchItem {
    #[inline]
    pub fn time(&self) -> i64 {
        self.modified_ns
    }
}

/// Результат поисковой операции.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchResponse {
    /// Точное общее число найденных сгруппированных результатов (до отсечения лимитом).
    pub total: usize,
    /// Карточки результатов (не более `limit`, прижатого к 1..500).
    pub items: Vec<SearchItem>,
    /// Затраченное время на выполнение поиска.
    pub elapsed: Duration,
    /// Предупреждения (например, об отсутствии словаря словоформ).
    pub warnings: Vec<String>,
}

impl SearchResponse {
    #[inline]
    pub fn unwrap(self) -> Self {
        self
    }
}

/// Имена файлов, привязка текстов к файлам и множество SHA-256 групп дубликатов.
type NamesState = (NamesIndex, HashMap<String, Vec<usize>>, HashSet<String>);

/// Поисковый движок ПоисКовичеКа.
#[derive(Clone)]
pub struct SearchEngine {
    paths: AppPaths,
    settings: Arc<RwLock<Settings>>,
    morph: Option<Morph>,
    morph_path: PathBuf,
    index_reader: IndexReader,
    sha_field: Field,
    body_field: Field,
    names: Arc<RwLock<NamesIndex>>,
    sha_to_files: Arc<RwLock<HashMap<String, Vec<usize>>>>,
    duplicate_shas: Arc<RwLock<HashSet<String>>>,
}

impl SearchEngine {
    /// Открывает поисковый движок:
    /// - инициализирует или открывает tantivy index reader;
    /// - открывает базу SQLite строго в режиме `query_only`;
    /// - строит имена в памяти из базы;
    /// - если морфологический словарь не найден, фиксирует предупреждение.
    pub fn open(
        paths: &AppPaths,
        settings: &Settings,
        morph: Option<Morph>,
    ) -> Result<Self, SearchError> {
        let morph_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(crate::paths::morph_path))
            .unwrap_or_else(|| paths.data_dir.join("morph.fst"));

        paths.ensure_dirs()?;

        // 1. Полнотекстовый индекс tantivy
        let mmap_dir = PovtornyiKatalog::open(&paths.index_dir)?;
        let index = if Index::exists(&mmap_dir)? {
            let idx = Index::open(mmap_dir)?;
            register_poisk_tokenizer(&idx, morph.clone());
            idx
        } else {
            let (schema, _, _) = create_schema();
            let idx = Index::create(mmap_dir, schema, tantivy::IndexSettings::default())?;
            register_poisk_tokenizer(&idx, morph.clone());
            idx
        };

        let schema = index.schema();
        let sha_field = schema
            .get_field("sha")
            .map_err(|_| IndexError::MissingField("sha"))?;
        let body_field = schema
            .get_field("body")
            .map_err(|_| IndexError::MissingField("body"))?;

        let index_reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;

        // 2. Проверяем наличие базы данных
        if !paths.catalog_db.is_file() {
            return Err(SearchError::DatabaseNotFound(
                paths.catalog_db.display().to_string(),
            ));
        }

        // 3. Открываем соединение только для чтения для загрузки имен
        let conn = Self::open_read_connection(&paths.catalog_db)?;

        let (names, sha_to_files, duplicate_shas) = Self::load_names_state(&conn, morph.as_ref())?;

        Ok(Self {
            paths: paths.clone(),
            settings: Arc::new(RwLock::new(settings.clone())),
            morph,
            morph_path,
            index_reader,
            sha_field,
            body_field,
            names: Arc::new(RwLock::new(names)),
            sha_to_files: Arc::new(RwLock::new(sha_to_files)),
            duplicate_shas: Arc::new(RwLock::new(duplicate_shas)),
        })
    }

    /// Обновляет текущие настройки поиска без перезапуска движка.
    pub fn update_settings(&self, new_settings: Settings) {
        if let Ok(mut lock) = self.settings.write() {
            *lock = new_settings;
        }
    }

    /// Возвращает текущую копию настроек поиска.
    pub fn settings(&self) -> Settings {
        self.settings.read().map(|s| s.clone()).unwrap_or_default()
    }

    /// Открывает соединение с базой данных `catalog.sqlite3` строго в режиме только для чтения.
    pub fn read_connection(&self) -> Result<Connection, SearchError> {
        Self::open_read_connection(&self.paths.catalog_db)
    }

    /// Алиас для `read_connection` для совместимости.
    pub fn connection(&self) -> Result<Connection, SearchError> {
        self.read_connection()
    }

    fn open_read_connection(path: &Path) -> Result<Connection, SearchError> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(Duration::from_millis(1500))?;
        conn.pragma_update(None, "query_only", true)?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "cache_size", -65536i64)?;
        conn.pragma_update(None, "mmap_size", 268435456i64)?;
        Ok(conn)
    }

    /// Ссылка на рабочие пути приложения.
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Путь к базе данных каталога.
    pub fn db_path(&self) -> &Path {
        &self.paths.catalog_db
    }

    /// Возвращает путь к словарю словоформ.
    pub fn morph_path(&self) -> &Path {
        &self.morph_path
    }

    /// Читает из базы имена файлов, привязку текстов к файлам и список групп дубликатов.
    fn load_names_state(
        conn: &Connection,
        morph: Option<&Morph>,
    ) -> Result<NamesState, SearchError> {
        let names = NamesIndex::from_connection(conn, morph)?;

        let mut sha_to_files: HashMap<String, Vec<usize>> = HashMap::new();
        for (idx, f) in names.files.iter().enumerate() {
            if f.is_deleted() {
                continue;
            }
            if f.sha_id != u32::MAX && (f.sha_id as usize) < names.shas.len() {
                let sha = &names.shas[f.sha_id as usize];
                sha_to_files.entry(sha.clone()).or_default().push(idx);
            }
        }

        let mut duplicate_shas = HashSet::new();
        if let Ok(mut stmt) = conn.prepare("SELECT sha256 FROM duplicate_groups;")
            && let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(0))
        {
            for r in rows.flatten() {
                duplicate_shas.insert(r);
            }
        }

        Ok((names, sha_to_files, duplicate_shas))
    }

    /// Перезагружает читатель индекса и актуализирует имена в памяти.
    ///
    /// Новые данные строятся целиком без блокировок (поиск в это время работает на старых),
    /// затем все три таблицы подменяются разом под записывающими блокировками, взятыми
    /// в том же порядке, что и читающие в `search`: так поиск никогда не видит
    /// имена нового состояния вместе с привязкой текстов старого.
    pub fn reload(&self) -> Result<(), SearchError> {
        self.index_reader.reload()?;
        let conn = self.read_connection()?;
        let (names, sha_to_files, duplicate_shas) =
            Self::load_names_state(&conn, self.morph.as_ref())?;

        let mut names_lock = self.names.write().unwrap_or_else(|e| e.into_inner());
        let mut sha_lock = self.sha_to_files.write().unwrap_or_else(|e| e.into_inner());
        let mut dup_lock = self
            .duplicate_shas
            .write()
            .unwrap_or_else(|e| e.into_inner());
        *names_lock = names;
        *sha_lock = sha_to_files;
        *dup_lock = duplicate_shas;
        Ok(())
    }

    /// Читает сжатое zstd-тело текста из таблицы `texts` по его SHA-256 хэшу.
    pub fn compressed_body(&self, sha: &str) -> Result<Option<Vec<u8>>, SearchError> {
        let conn = self.read_connection()?;
        let mut stmt = conn.prepare_cached("SELECT body FROM texts WHERE sha = ?1;")?;
        let mut rows = stmt.query(rusqlite::params![sha])?;
        if let Some(row) = rows.next()? {
            let body: Vec<u8> = row.get(0)?;
            Ok(Some(body))
        } else {
            Ok(None)
        }
    }

    /// Выполняет поиск по тексту, именам и путям с группировкой и ранжированием.
    pub fn search(&self, request: impl IntoSearchRequest) -> SearchResponse {
        let started = Instant::now();
        let current_settings = self.settings();
        let request = request.into_search_request(&current_settings);

        let mut warnings = Vec::new();
        if self.morph.is_none() {
            warnings.push(format!(
                "Словарь словоформ не найден: {}. Поиск работает без словоформ",
                self.morph_path.display()
            ));
        }

        let effective_limit = request.limit.clamp(1, 500);

        // Перезагружаем ридер tantivy, если были коммиты писателя
        let _ = self.index_reader.reload();

        let parsed = parse_query_forms(
            &request.text,
            self.morph.as_ref(),
            current_settings.enable_morphology,
            request.forms,
        );

        // 1. Пустой запрос
        if parsed.words.is_empty() {
            if request.filters.is_empty() {
                // Пустой текст без фильтров -> пусто, total 0 (R2-В-33, R1-Б-51)
                return SearchResponse {
                    total: 0,
                    items: Vec::new(),
                    elapsed: started.elapsed(),
                    warnings,
                };
            }

            // Пустой текст + фильтры -> список по фильтрам с точным total (R1-А-143, R1-Б-52)
            return self.search_filters_only(&request, effective_limit, started, warnings);
        }

        // 2. Полнотекстовый поиск в Tantivy
        let text_hits = self.search_tantivy(&parsed, request.mode);

        // 3. Поиск по именам и путям через NamesIndex
        let name_filter = NameFilter::from(&request.filters);
        let name_search_mode = match request.mode {
            MultiWordMode::StrictPhrase => SearchMode::StrictPhrase,
            MultiWordMode::AllWords => SearchMode::AllWords,
        };
        let names_guard = self.names.read().unwrap();
        let name_matches = names_guard.search_matches(&parsed, &name_filter, name_search_mode);

        // 4. Группировка и слияние результатов
        let sha_to_files_guard = self.sha_to_files.read().unwrap();
        let dup_shas_guard = self.duplicate_shas.read().unwrap();

        let mut groups: HashMap<String, GroupCandidate> = HashMap::new();

        // 4a. Добавляем совпадения по тексту
        for hit in text_hits {
            let files = match sha_to_files_guard.get(&hit.sha) {
                Some(indices) => indices,
                None => continue,
            };

            let matching_indices: Vec<usize> = files
                .iter()
                .copied()
                .filter(|&idx| {
                    let f = &names_guard.files[idx];
                    file_matches_filters(f, &names_guard, &request.filters)
                })
                .collect();

            if matching_indices.is_empty() {
                continue;
            }

            let entry = groups
                .entry(hit.sha.clone())
                .or_insert_with(|| GroupCandidate {
                    item_key: hit.sha.clone(),
                    text_sha: Some(hit.sha.clone()),
                    rank: hit.rank,
                    phrase_hit: hit.phrase_hit,
                    file_indices: HashSet::new(),
                });

            entry.rank = entry.rank.min(hit.rank);
            entry.phrase_hit |= hit.phrase_hit;
            entry.file_indices.extend(matching_indices);
        }

        // 4b. Добавляем совпадения по имени и пути
        for m in name_matches {
            let file = &names_guard.files[m.file_index];
            let key = if file.sha_id != u32::MAX && (file.sha_id as usize) < names_guard.shas.len()
            {
                names_guard.shas[file.sha_id as usize].clone()
            } else {
                format!("@file:{}", file.id)
            };

            let name_phrase_hit = if parsed.words.len() > 1 {
                match request.mode {
                    MultiWordMode::StrictPhrase => true,
                    MultiWordMode::AllWords => m.in_name || m.in_path,
                }
            } else {
                false
            };

            let entry = groups.entry(key.clone()).or_insert_with(|| {
                let text_sha = if key.starts_with("@file:") {
                    None
                } else {
                    Some(key.clone())
                };
                GroupCandidate {
                    item_key: key,
                    text_sha,
                    rank: 0.0,
                    phrase_hit: name_phrase_hit,
                    file_indices: HashSet::new(),
                }
            });

            entry.phrase_hit |= name_phrase_hit;
            entry.file_indices.insert(m.file_index);
        }

        // 5. Фильтрация дубликатов (если задан фильтр duplicates_only)
        if request.filters.duplicates_only {
            groups.retain(|_, grp| {
                if grp.file_indices.len() > 1 {
                    return true;
                }
                grp.file_indices.iter().any(|&idx| {
                    let f = &names_guard.files[idx];
                    if f.sha_id != u32::MAX && (f.sha_id as usize) < names_guard.shas.len() {
                        let sha = &names_guard.shas[f.sha_id as usize];
                        dup_shas_guard.contains(sha)
                    } else {
                        false
                    }
                })
            });
        }

        let total = groups.len();

        // 6. Сортировка групп
        let mut sorted_groups: Vec<GroupCandidate> = groups.into_values().collect();
        self.sort_groups(&mut sorted_groups, &names_guard, request.sort);

        // 7. Отсекаем по лимиту выдачи
        sorted_groups.truncate(effective_limit);

        // 8. Сборка карточек SearchItem с метаданными из базы
        let items = self.build_items(
            sorted_groups,
            &names_guard,
            &dup_shas_guard,
            parsed.words.len() > 1,
        );

        SearchResponse {
            total,
            items,
            elapsed: started.elapsed(),
            warnings,
        }
    }

    /// Поиск только по фильтрам при пустом текстовом запросе.
    fn search_filters_only(
        &self,
        request: &SearchRequest,
        effective_limit: usize,
        started: Instant,
        warnings: Vec<String>,
    ) -> SearchResponse {
        let name_filter = NameFilter::from(&request.filters);
        let names_guard = self.names.read().unwrap();
        let matches =
            names_guard.search_matches(&ParsedQuery::default(), &name_filter, SearchMode::AllWords);

        let dup_shas_guard = self.duplicate_shas.read().unwrap();
        let mut groups: HashMap<String, GroupCandidate> = HashMap::new();

        for m in matches {
            let file = &names_guard.files[m.file_index];
            let key = if file.sha_id != u32::MAX && (file.sha_id as usize) < names_guard.shas.len()
            {
                names_guard.shas[file.sha_id as usize].clone()
            } else {
                format!("@file:{}", file.id)
            };

            let entry = groups.entry(key.clone()).or_insert_with(|| {
                let text_sha = if key.starts_with("@file:") {
                    None
                } else {
                    Some(key.clone())
                };
                GroupCandidate {
                    item_key: key,
                    text_sha,
                    rank: 0.0,
                    phrase_hit: false,
                    file_indices: HashSet::new(),
                }
            });

            entry.file_indices.insert(m.file_index);
        }

        if request.filters.duplicates_only {
            groups.retain(|_, grp| {
                if grp.file_indices.len() > 1 {
                    return true;
                }
                grp.file_indices.iter().any(|&idx| {
                    let f = &names_guard.files[idx];
                    if f.sha_id != u32::MAX && (f.sha_id as usize) < names_guard.shas.len() {
                        let sha = &names_guard.shas[f.sha_id as usize];
                        dup_shas_guard.contains(sha)
                    } else {
                        false
                    }
                })
            });
        }

        let total = groups.len();
        let mut sorted_groups: Vec<GroupCandidate> = groups.into_values().collect();
        self.sort_groups(&mut sorted_groups, &names_guard, request.sort);
        sorted_groups.truncate(effective_limit);

        let items = self.build_items(sorted_groups, &names_guard, &dup_shas_guard, false);

        SearchResponse {
            total,
            items,
            elapsed: started.elapsed(),
            warnings,
        }
    }

    /// Сортирует список групп документов в соответствии с выбранным режимом `SortMode`.
    fn sort_groups(&self, groups: &mut [GroupCandidate], names: &NamesIndex, sort: SortMode) {
        groups.sort_by(|a, b| {
            match sort {
                SortMode::Relevance => {
                    // Точная фраза -> ранг BM25 (меньше = релевантнее) -> строковый ключ
                    b.phrase_hit
                        .cmp(&a.phrase_hit)
                        .then_with(|| a.rank.partial_cmp(&b.rank).unwrap_or(Ordering::Equal))
                        .then_with(|| a.item_key.cmp(&b.item_key))
                }
                SortMode::SizeDesc => {
                    let size_a = a
                        .file_indices
                        .iter()
                        .map(|&idx| names.files[idx].size)
                        .max()
                        .unwrap_or(0);
                    let size_b = b
                        .file_indices
                        .iter()
                        .map(|&idx| names.files[idx].size)
                        .max()
                        .unwrap_or(0);
                    size_b
                        .cmp(&size_a)
                        .then_with(|| {
                            let name_a = rep_name(a, names);
                            let name_b = rep_name(b, names);
                            name_a
                                .to_lowercase()
                                .cmp(&name_b.to_lowercase())
                                .then_with(|| name_a.cmp(&name_b))
                        })
                        .then_with(|| a.item_key.cmp(&b.item_key))
                }
                SortMode::DateDesc => {
                    let mod_a = a
                        .file_indices
                        .iter()
                        .map(|&idx| names.files[idx].modified_ns)
                        .max()
                        .unwrap_or(0);
                    let mod_b = b
                        .file_indices
                        .iter()
                        .map(|&idx| names.files[idx].modified_ns)
                        .max()
                        .unwrap_or(0);
                    mod_b
                        .cmp(&mod_a)
                        .then_with(|| {
                            let name_a = rep_name(a, names);
                            let name_b = rep_name(b, names);
                            name_a
                                .to_lowercase()
                                .cmp(&name_b.to_lowercase())
                                .then_with(|| name_a.cmp(&name_b))
                        })
                        .then_with(|| a.item_key.cmp(&b.item_key))
                }
                SortMode::NameAsc => {
                    let name_a = rep_name(a, names);
                    let name_b = rep_name(b, names);
                    name_a
                        .to_lowercase()
                        .cmp(&name_b.to_lowercase())
                        .then_with(|| name_a.cmp(&name_b))
                        .then_with(|| a.item_key.cmp(&b.item_key))
                }
            }
        });
    }

    /// Собирает структуры `SearchItem`, загружая `content_status` и `error` из SQLite.
    fn build_items(
        &self,
        groups: Vec<GroupCandidate>,
        names: &NamesIndex,
        duplicate_shas: &HashSet<String>,
        is_multi_word: bool,
    ) -> Vec<SearchItem> {
        if groups.is_empty() {
            return Vec::new();
        }

        // Собираем id файлов для единичного пакетного запроса статусов из базы
        let mut rep_file_ids = Vec::with_capacity(groups.len());
        for grp in &groups {
            if let Some(&rep_idx) = grp.file_indices.iter().next() {
                rep_file_ids.push(names.files[rep_idx].id);
            }
        }

        let mut status_map: HashMap<i64, (Option<String>, Option<String>)> = HashMap::new();
        if let Ok(conn) = self.read_connection() {
            for chunk in rep_file_ids.chunks(400) {
                let placeholders = std::iter::repeat_n("?", chunk.len())
                    .collect::<Vec<_>>()
                    .join(",");
                let sql = format!(
                    "SELECT id, content_status, error FROM files WHERE id IN ({placeholders});"
                );
                if let Ok(mut stmt) = conn.prepare(&sql) {
                    let params = rusqlite::params_from_iter(chunk.iter().copied());
                    if let Ok(rows) = stmt.query_map(params, |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<String>>(2)?,
                        ))
                    }) {
                        for item in rows.flatten() {
                            status_map.insert(item.0, (item.1, item.2));
                        }
                    }
                }
            }
        }

        let mut items = Vec::with_capacity(groups.len());

        for grp in groups {
            let mut paths: Vec<String> = grp
                .file_indices
                .iter()
                .map(|&idx| {
                    let f = &names.files[idx];
                    std::str::from_utf8(
                        &names.path_bytes
                            [f.path_offset as usize..(f.path_offset + f.path_len) as usize],
                    )
                    .unwrap_or("")
                    .to_string()
                })
                .collect();

            // Пути группы строго по алфавиту без учёта регистра (R2-А-23, R2-В-30)
            paths.sort_by(|a, b| {
                a.to_lowercase()
                    .cmp(&b.to_lowercase())
                    .then_with(|| a.cmp(b))
            });

            // Выбираем репрезентативный файл детерминированно: соответствующий первому пути
            let rep_idx = grp
                .file_indices
                .iter()
                .copied()
                .min_by_key(|&idx| {
                    let f = &names.files[idx];
                    let p = std::str::from_utf8(
                        &names.path_bytes
                            [f.path_offset as usize..(f.path_offset + f.path_len) as usize],
                    )
                    .unwrap_or("");
                    (p.to_lowercase(), p.to_string())
                })
                .unwrap();
            let rep_file = &names.files[rep_idx];

            let rep_name = std::str::from_utf8(
                &names.name_bytes[rep_file.name_offset as usize
                    ..(rep_file.name_offset + rep_file.name_len) as usize],
            )
            .unwrap_or("")
            .to_string();

            let category = names.categories[rep_file.category_id as usize].clone();
            let extension = names.extensions[rep_file.ext_id as usize]
                .trim_start_matches('.')
                .to_string();

            let max_size = grp
                .file_indices
                .iter()
                .map(|&idx| names.files[idx].size)
                .max()
                .unwrap_or(0);

            let max_modified = grp
                .file_indices
                .iter()
                .map(|&idx| names.files[idx].modified_ns)
                .max()
                .unwrap_or(0);

            let is_dup = paths.len() > 1
                || grp.file_indices.iter().any(|&idx| {
                    let f = &names.files[idx];
                    if f.sha_id != u32::MAX && (f.sha_id as usize) < names.shas.len() {
                        let sha = &names.shas[f.sha_id as usize];
                        duplicate_shas.contains(sha)
                    } else {
                        false
                    }
                });

            let (content_status, error) = status_map.remove(&rep_file.id).unwrap_or((None, None));

            let status = format_extraction_status(content_status.as_deref(), error.as_deref());

            let item_key = grp.item_key;
            let text_sha = grp.text_sha;
            let phrase_hit = if is_multi_word { grp.phrase_hit } else { false };

            items.push(SearchItem {
                key: item_key.clone(),
                item_key,
                text_sha: text_sha.clone(),
                text_sha256: text_sha,
                name: rep_name,
                extension,
                category,
                size: max_size,
                modified_ns: max_modified,
                path_count: paths.len(),
                paths,
                content_status,
                status,
                is_duplicate: is_dup,
                phrase_hit,
                rank: grp.rank,
            });
        }

        items
    }

    /// Полнотекстовый поиск tantivy:
    /// - одно слово: объединение терминов (Exact — термины, Prefix — из словаря индекса <= 256);
    /// - строгая фраза: <= 512 сочетаний -> PhraseQuery по всем сочетаниям; иначе кандидаты + find_sequence;
    /// - режим «все слова»: документы со всеми словами, фразовые выше.
    fn search_tantivy(&self, query: &ParsedQuery, mode: MultiWordMode) -> Vec<TantivyHit> {
        let searcher = self.index_reader.searcher();
        let k = query.words.len();

        if k == 0 {
            return Vec::new();
        }

        // 1. Собираем термины tantivy для каждого слова запроса
        let mut terms_per_word: Vec<Vec<String>> = Vec::with_capacity(k);
        for q_word in &query.words {
            let terms = match &q_word.matcher {
                QueryWordMatcher::Exact(tokens) => tokens.clone(),
                QueryWordMatcher::Prefix(prefix) => {
                    resolve_prefix_terms(&searcher, self.body_field, prefix, 256)
                }
            };
            if terms.is_empty() {
                // Если хоть одно слово запроса не имеет терминов в корпусе, фраза/пересечение пусты
                return Vec::new();
            }
            terms_per_word.push(terms);
        }

        // Одно слово
        if k == 1 {
            let terms = &terms_per_word[0];
            let queries: Vec<Box<dyn Query>> = terms
                .iter()
                .map(|t| {
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.body_field, t),
                        IndexRecordOption::WithFreqsAndPositions,
                    )) as Box<dyn Query>
                })
                .collect();
            let tantivy_query = BooleanQuery::union(queries);

            return self.collect_tantivy_hits(&searcher, &tantivy_query, false);
        }

        // Многословный запрос
        let mut total_combinations: usize = 1;
        for t in &terms_per_word {
            total_combinations = total_combinations.saturating_mul(t.len());
        }

        if mode == MultiWordMode::StrictPhrase {
            if total_combinations <= 512 {
                // Строгая фраза: декартово произведение <= 512 -> объединение PhraseQuery
                let combinations = cartesian_product(&terms_per_word);
                let phrase_queries: Vec<Box<dyn Query>> = combinations
                    .into_iter()
                    .map(|combo| {
                        let terms: Vec<Term> = combo
                            .into_iter()
                            .map(|t| Term::from_field_text(self.body_field, &t))
                            .collect();
                        Box::new(PhraseQuery::new(terms)) as Box<dyn Query>
                    })
                    .collect();
                let tantivy_query = BooleanQuery::union(phrase_queries);

                return self.collect_tantivy_hits(&searcher, &tantivy_query, true);
            } else {
                // Больше 512 сочетаний: поиск кандидатов (все слова в документе) + find_sequence
                let candidate_query = build_candidate_query(self.body_field, &terms_per_word);
                let candidate_hits = self.collect_tantivy_hits(&searcher, &candidate_query, false);

                // ОБЯЗАТЕЛЬНАЯ программная проверка find_sequence по сохранённому сжатому тексту (параллельно Rayon)
                return self.verify_phrase_candidates(candidate_hits, query);
            }
        }

        // Режим «все слова»: кандидаты со всеми словами, фразовые выше
        let candidate_query = build_candidate_query(self.body_field, &terms_per_word);
        let candidate_hits = self.collect_tantivy_hits(&searcher, &candidate_query, false);

        self.classify_all_words_candidates(candidate_hits, query)
    }

    /// Сбор всех совпадений из tantivy с чтением сохранённого поля `sha`.
    fn collect_tantivy_hits(
        &self,
        searcher: &Searcher,
        query: &dyn Query,
        phrase_hit: bool,
    ) -> Vec<TantivyHit> {
        let Ok(top_docs) = searcher.search(query, &TopDocs::with_limit(100_000).order_by_score())
        else {
            return Vec::new();
        };

        let mut hits = Vec::with_capacity(top_docs.len());
        for (score, doc_address) in top_docs {
            if let Ok(doc) = searcher.doc::<TantivyDocument>(doc_address)
                && let Some(sha) = doc.get_first(self.sha_field).and_then(|v| v.as_str())
            {
                hits.push(TantivyHit {
                    sha: sha.to_string(),
                    rank: -(score as f64),
                    phrase_hit,
                });
            }
        }
        hits
    }

    /// Проверяет кандидатов строгой фразы через `phrase::find_sequence` параллельно в пуле `rayon`.
    fn verify_phrase_candidates(
        &self,
        candidates: Vec<TantivyHit>,
        query: &ParsedQuery,
    ) -> Vec<TantivyHit> {
        if candidates.is_empty() {
            return Vec::new();
        }

        let verified: Vec<Option<TantivyHit>> = candidates
            .into_par_iter()
            .map(|hit| {
                let Ok(Some(compressed)) = self.compressed_body(&hit.sha) else {
                    return None;
                };
                let matches = compressed_contains_sequence(&compressed, query, self.morph.as_ref())
                    .unwrap_or(false);

                if matches {
                    Some(TantivyHit {
                        phrase_hit: true,
                        ..hit
                    })
                } else {
                    None
                }
            })
            .collect();

        verified.into_iter().flatten().collect()
    }

    /// Разделяет кандидатов режима «все слова» на фразовые (`phrase_hit = true`) и нефразовые.
    fn classify_all_words_candidates(
        &self,
        candidates: Vec<TantivyHit>,
        query: &ParsedQuery,
    ) -> Vec<TantivyHit> {
        if candidates.is_empty() {
            return Vec::new();
        }

        candidates
            .into_par_iter()
            .map(|hit| {
                let phrase_hit = if let Ok(Some(compressed)) = self.compressed_body(&hit.sha) {
                    compressed_contains_sequence(&compressed, query, self.morph.as_ref())
                        .unwrap_or(false)
                } else {
                    false
                };
                TantivyHit { phrase_hit, ..hit }
            })
            .collect()
    }

    /// Строит фрагмент текста по запросу окна для конкретной карточки документа.
    ///
    /// Для групп без сохранённого текста возвращает пустой `Snippet::default()`.
    /// Один фрагмент на группу (R1-Б-67).
    pub fn snippet_for(&self, item_key: &str, request: &SearchRequest) -> Snippet {
        if item_key.starts_with("@file:") || item_key.is_empty() {
            return Snippet::default();
        }

        let Ok(Some(compressed)) = self.compressed_body(item_key) else {
            return Snippet::default();
        };

        let current_settings = self.settings();
        let parsed = parse_query_forms(
            &request.text,
            self.morph.as_ref(),
            current_settings.enable_morphology,
            request.forms,
        );

        if parsed.words.is_empty() {
            return Snippet::default();
        }

        let prefer_phrase = request.mode == MultiWordMode::StrictPhrase && parsed.words.len() > 1;

        build_snippet_compressed(
            &compressed,
            &parsed,
            self.morph.as_ref(),
            current_settings.fragment_before,
            current_settings.fragment_after,
            prefer_phrase,
        )
        .unwrap_or_default()
    }
}

/// Совпадение в Tantivy.
#[derive(Clone, Debug)]
struct TantivyHit {
    sha: String,
    rank: f64,
    phrase_hit: bool,
}

/// Промежуточный кандидат сгруппированной карточки.
struct GroupCandidate {
    item_key: String,
    text_sha: Option<String>,
    rank: f64,
    phrase_hit: bool,
    file_indices: HashSet<usize>,
}

fn rep_name(grp: &GroupCandidate, names: &NamesIndex) -> String {
    if let Some(&idx) = grp.file_indices.iter().next() {
        let f = &names.files[idx];
        std::str::from_utf8(
            &names.name_bytes[f.name_offset as usize..(f.name_offset + f.name_len) as usize],
        )
        .unwrap_or("")
        .to_string()
    } else {
        String::new()
    }
}

fn file_matches_filters(file: &CompactFile, names: &NamesIndex, filters: &SearchFilters) -> bool {
    if file.is_deleted() {
        return false;
    }
    if filters.available_only && !file.is_available() {
        return false;
    }
    if let Some(src) = filters.source_id
        && file.source_id != src
    {
        return false;
    }
    if let Some(vol) = filters.volume_id
        && file.volume_id != vol
    {
        return false;
    }
    if let Some(min_size) = filters.min_size
        && file.size < min_size
    {
        return false;
    }
    if let Some(ref cat) = filters.category {
        let file_cat = &names.categories[file.category_id as usize];
        if !file_cat.eq_ignore_ascii_case(cat) {
            return false;
        }
    }
    if let Some(ref ext) = filters.extension {
        let clean_filter = ext.trim_start_matches('.');
        let file_ext = names.extensions[file.ext_id as usize].trim_start_matches('.');
        if !file_ext.eq_ignore_ascii_case(clean_filter) {
            return false;
        }
    }
    true
}

/// Сканирует словарь терминов tantivy и находит термины с заданным префиксом (не более `max_terms`).
fn resolve_prefix_terms(
    searcher: &Searcher,
    field: Field,
    prefix: &str,
    max_terms: usize,
) -> Vec<String> {
    let mut terms = HashSet::new();
    let prefix_bytes = prefix.as_bytes();

    for segment_reader in searcher.segment_readers() {
        let Ok(inv_index) = segment_reader.inverted_index(field) else {
            continue;
        };
        let term_dict = inv_index.terms();
        let Ok(mut streamer) = term_dict.range().ge(prefix_bytes).into_stream() else {
            continue;
        };

        while streamer.advance() {
            let key = streamer.key();
            if !key.starts_with(prefix_bytes) {
                break;
            }
            if let Ok(s) = std::str::from_utf8(key) {
                terms.insert(s.to_string());
                if terms.len() >= max_terms {
                    break;
                }
            }
        }
        if terms.len() >= max_terms {
            break;
        }
    }

    let mut result: Vec<String> = terms.into_iter().collect();
    result.sort();
    result
}

/// Строит декартово произведение наборов терминов.
fn cartesian_product(lists: &[Vec<String>]) -> Vec<Vec<String>> {
    let mut result = vec![Vec::new()];
    for list in lists {
        let mut next = Vec::with_capacity(result.len() * list.len());
        for prev in &result {
            for item in list {
                let mut combo = prev.clone();
                combo.push(item.clone());
                next.push(combo);
            }
        }
        result = next;
    }
    result
}

/// Строит конъюнктивный запрос кандидатов для режима «все слова» или длинных фраз.
fn build_candidate_query(field: Field, terms_per_word: &[Vec<String>]) -> BooleanQuery {
    let mut clauses = Vec::with_capacity(terms_per_word.len());
    for word_terms in terms_per_word {
        let term_queries: Vec<Box<dyn Query>> = word_terms
            .iter()
            .map(|t| {
                Box::new(TermQuery::new(
                    Term::from_field_text(field, t),
                    IndexRecordOption::WithFreqsAndPositions,
                )) as Box<dyn Query>
            })
            .collect();
        clauses.push((
            Occur::Must,
            Box::new(BooleanQuery::union(term_queries)) as Box<dyn Query>,
        ));
    }
    BooleanQuery::new(clauses)
}

/// Форматирует причину неизвлечения текста для карточки в интерфейсе: «текст не извлечён: <причина>».
pub fn format_extraction_status(
    content_status: Option<&str>,
    error: Option<&str>,
) -> Option<String> {
    let status = content_status?;
    if status == "text" || status == "metadata-only" || status == "ok" {
        return None;
    }

    if status == "cloud-not-downloaded" {
        Some("облачный файл: содержимое не скачивается".to_string())
    } else if let Some(ext) = status.strip_prefix("unsupported-text-") {
        Some(format!(
            "текст внутри .{ext} не извлечён: извлекатель не встроен"
        ))
    } else if status == "scan-needs-ocr" {
        Some("PDF без текстового слоя (скан): нужен OCR".to_string())
    } else if status == "sensitive-metadata-only" {
        Some("закрытые данные: содержимое не читается".to_string())
    } else if status == "archive-member-too-large" || status.starts_with("archive-member-too-large")
    {
        let n = error.unwrap_or("20");
        Some(format!("внутренний файл архива больше {n} МБ"))
    } else if status == "failed" {
        if let Some(err) = error.filter(|e| !e.trim().is_empty()) {
            Some(format!("ошибка извлечения: {err}"))
        } else {
            Some("ошибка извлечения".to_string())
        }
    } else {
        status
            .strip_prefix("failed: ")
            .map(|err| format!("ошибка извлечения: {err}"))
    }
}

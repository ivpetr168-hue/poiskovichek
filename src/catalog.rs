use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;

/// Запись о файле в каталоге.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: Option<i64>,
    pub path: String,
    pub source_id: Option<i64>,
    pub volume_id: Option<i64>,
    pub rel_path: String,
    pub name: String,
    pub extension: String,
    pub size: u64,
    pub modified_ns: i64,
    pub created_ns: i64,
    pub category: String,
    pub inside_archive: bool,
    pub container_id: Option<i64>,
    pub content_status: Option<String>,
    pub extraction_version: u32,
    pub text_sha: Option<String>,
    pub quick_hash: Option<String>,
    pub sha256: Option<String>,
    pub error: Option<String>,
    pub scan_run: Option<i64>,
    pub available: bool,
}

/// Компактная запись снимка состояния файла в памяти для быстрой сверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSnapshotEntry {
    pub id: i64,
    pub size: u64,
    pub modified_ns: i64,
    pub extraction_version: u32,
    pub has_error: bool,
    pub container_id: Option<i64>,
    /// SHA-256 прежнего текста файла: при сбое разбора он сохраняется (№222).
    pub text_sha: Option<String>,
}

/// Запись об источнике индексирования.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRecord {
    pub id: Option<i64>,
    pub path: String,
    pub mode: String,
    pub max_depth: u32,
    pub enabled: bool,
    pub volume_id: Option<i64>,
    pub status: String,
    pub last_indexed_at: Option<i64>,
    pub details: Option<String>,
}

/// Ошибки работы с каталогом SQLite.
#[derive(Debug)]
pub enum CatalogError {
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
    IncompatibleSchemaVersion(u32),
    Compression(String),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(err) => write!(f, "Ошибка SQLite: {err}"),
            Self::Io(err) => write!(f, "Ошибка ввода-вывода каталога: {err}"),
            Self::IncompatibleSchemaVersion(v) => {
                write!(
                    f,
                    "Версия схемы базы данных не поддерживается: {v} (ожидается {SCHEMA_VERSION})"
                )
            }
            Self::Compression(msg) => write!(f, "Ошибка сжатия/распаковки текста: {msg}"),
        }
    }
}

impl std::error::Error for CatalogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Sqlite(err) => Some(err),
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for CatalogError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Sqlite(err)
    }
}

impl From<std::io::Error> for CatalogError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Хранилище метаданных и текстов каталога (один писатель).
pub struct Catalog {
    conn: Connection,
    /// Сжиматель zstd, один на каталог: контекст создаётся один раз, а не на каждый текст.
    compressor: zstd::bulk::Compressor<'static>,
}

/// Уровень zstd для текстов: 1 даёт максимальную скорость сжатия.
const ZSTD_LEVEL: i32 = 1;

/// Размер кэша страниц писателя, КиБ (отрицательное значение `cache_size` в SQLite = КиБ).
const WRITER_CACHE_KIB: i64 = 65_536;

/// Результат записи порции: идентификаторы файлов и `(SHA-256, is_new)` для порции текстов.
pub type BatchWrite = (Vec<i64>, Vec<(String, bool)>);

impl fmt::Debug for Catalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Catalog")
    }
}

impl Catalog {
    /// Открывает существующую базу или создает новую по указанному пути.
    pub fn open_or_create(path: &Path) -> Result<Self, CatalogError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;

        let conn = Connection::open(path)?;
        Self::configure_connection(&conn)?;
        Self::configure_writer(&conn)?;
        Self::init_or_check_schema(&conn)?;

        Self::from_connection(conn)
    }

    /// Собирает каталог вокруг настроенного соединения.
    fn from_connection(conn: Connection) -> Result<Self, CatalogError> {
        let mut compressor = zstd::bulk::Compressor::new(ZSTD_LEVEL)
            .map_err(|e| CatalogError::Compression(e.to_string()))?;
        // Как у `zstd::encode_all`: размер содержимого в заголовок кадра не пишется
        compressor
            .include_contentsize(false)
            .map_err(|e| CatalogError::Compression(e.to_string()))?;
        Ok(Self { conn, compressor })
    }

    /// Открывает существующую базу данных. Если файл не существует, возвращает ошибку.
    pub fn open(path: &Path) -> Result<Self, CatalogError> {
        if !path.exists() {
            return Err(CatalogError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("База данных не найдена: {}", path.display()),
            )));
        }

        let conn = Connection::open(path)?;
        Self::configure_connection(&conn)?;
        Self::configure_writer(&conn)?;
        Self::init_or_check_schema(&conn)?;

        Self::from_connection(conn)
    }

    /// Настраивает параметры подключения: WAL, synchronous=NORMAL, busy_timeout 5 с.
    fn configure_connection(conn: &Connection) -> Result<(), CatalogError> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    /// Дополнительные настройки писателя: временные структуры в памяти и увеличенный кэш страниц.
    /// Устойчивость к сбоям не меняется (WAL + synchronous=NORMAL задаются в `configure_connection`).
    fn configure_writer(conn: &Connection) -> Result<(), CatalogError> {
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        conn.pragma_update(None, "cache_size", -WRITER_CACHE_KIB)?;
        Ok(())
    }

    /// Проверяет версию схемы или создает таблицы при пустой базе.
    fn init_or_check_schema(conn: &Connection) -> Result<(), CatalogError> {
        let version: u32 = conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?;

        if version == 0 {
            let table_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%';",
                [],
                |row| row.get(0),
            )?;

            if table_count > 0 {
                // База существует, но версия схемы 0 (устаревшая или неподдерживаемая)
                return Err(CatalogError::IncompatibleSchemaVersion(0));
            }

            Self::create_schema(conn)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version != SCHEMA_VERSION {
            return Err(CatalogError::IncompatibleSchemaVersion(version));
        } else {
            // Версия совпадает, гарантируем наличие таблиц и триггеров
            Self::create_schema(conn)?;
        }

        Ok(())
    }

    /// Создает таблицы, индексы и триггеры схемы.
    fn create_schema(conn: &Connection) -> Result<(), CatalogError> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS volumes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                serial INTEGER NOT NULL,
                label TEXT NOT NULL,
                last_letter TEXT NOT NULL,
                last_seen INTEGER NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_volumes_serial ON volumes(serial);

            CREATE TABLE IF NOT EXISTS sources (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                mode TEXT NOT NULL,
                max_depth INTEGER NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1,
                volume_id INTEGER,
                status TEXT NOT NULL,
                last_indexed_at INTEGER,
                details TEXT,
                FOREIGN KEY(volume_id) REFERENCES volumes(id) ON DELETE SET NULL
            );

            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                source_id INTEGER,
                volume_id INTEGER,
                rel_path TEXT NOT NULL,
                name TEXT NOT NULL,
                extension TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified_ns INTEGER NOT NULL,
                created_ns INTEGER NOT NULL,
                category TEXT NOT NULL,
                inside_archive INTEGER NOT NULL DEFAULT 0,
                container_id INTEGER,
                content_status TEXT,
                extraction_version INTEGER NOT NULL DEFAULT 0,
                text_sha TEXT,
                quick_hash TEXT,
                sha256 TEXT,
                error TEXT,
                scan_run INTEGER,
                available INTEGER NOT NULL DEFAULT 1,
                FOREIGN KEY(source_id) REFERENCES sources(id) ON DELETE SET NULL,
                FOREIGN KEY(volume_id) REFERENCES volumes(id) ON DELETE SET NULL,
                FOREIGN KEY(container_id) REFERENCES files(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_files_container_id ON files(container_id);
            CREATE INDEX IF NOT EXISTS idx_files_size ON files(size);
            CREATE INDEX IF NOT EXISTS idx_files_sha256 ON files(sha256);
            CREATE INDEX IF NOT EXISTS idx_files_text_sha ON files(text_sha);
            CREATE INDEX IF NOT EXISTS idx_files_source_id ON files(source_id);

            CREATE TABLE IF NOT EXISTS texts (
                sha TEXT PRIMARY KEY,
                chars INTEGER NOT NULL,
                body BLOB NOT NULL
            );

            CREATE TABLE IF NOT EXISTS skipped (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL,
                reason TEXT NOT NULL,
                scan_run INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                started INTEGER NOT NULL,
                finished INTEGER,
                phase TEXT NOT NULL,
                status TEXT NOT NULL,
                details TEXT
            );

            CREATE TABLE IF NOT EXISTS duplicate_groups (
                sha256 TEXT PRIMARY KEY,
                file_count INTEGER NOT NULL,
                total_bytes INTEGER NOT NULL,
                reclaimable_bytes INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS index_outbox (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                sha TEXT NOT NULL,
                op TEXT NOT NULL
            );

            -- Триггер: при удалении файла, если на его text_sha больше никто не ссылается, удаляем текст
            CREATE TRIGGER IF NOT EXISTS files_after_delete AFTER DELETE ON files
            FOR EACH ROW WHEN OLD.text_sha IS NOT NULL
            BEGIN
                DELETE FROM texts
                WHERE sha = OLD.text_sha
                  AND NOT EXISTS (SELECT 1 FROM files WHERE text_sha = OLD.text_sha);
            END;

            -- Триггер: при смене text_sha файла удаляем старый текст, если на него нет ссылок
            CREATE TRIGGER IF NOT EXISTS files_after_update AFTER UPDATE OF text_sha ON files
            FOR EACH ROW WHEN OLD.text_sha IS NOT NULL AND (NEW.text_sha IS NULL OR OLD.text_sha != NEW.text_sha)
            BEGIN
                DELETE FROM texts
                WHERE sha = OLD.text_sha
                  AND NOT EXISTS (SELECT 1 FROM files WHERE text_sha = OLD.text_sha);
            END;

            -- Триггер: при удалении записи из texts помещаем sha в очередь на удаление из индекса
            CREATE TRIGGER IF NOT EXISTS texts_after_delete AFTER DELETE ON texts
            FOR EACH ROW
            BEGIN
                INSERT INTO index_outbox(sha, op) VALUES (OLD.sha, 'delete');
            END;
            "#,
        )?;

        Ok(())
    }

    /// Добавляет или обновляет порцию файлов в базе данных.
    /// Возвращает список ID добавленных/обновленных записей.
    pub fn upsert_files(&mut self, batch: &[FileRecord]) -> Result<Vec<i64>, CatalogError> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }

        let tx = self.conn.transaction()?;
        let ids = Self::upsert_files_in(&tx, batch)?;
        tx.commit()?;
        Ok(ids)
    }

    /// Записывает порцию файлов и их текстов ОДНОЙ транзакцией SQLite.
    ///
    /// `texts[i]` — текст файла `batch[i]` (или `None`, если у файла нет нового текста).
    /// Возвращает ID файлов и `(SHA-256, is_new)` для каждого переданного текста
    /// в порядке следования (как у `store_text_batch`). Запись файла и его текста
    /// фиксируются вместе: после сбоя не бывает файла, отмеченного как обработанный, без текста.
    pub fn write_batch(
        &mut self,
        batch: &[FileRecord],
        texts: &[Option<&str>],
    ) -> Result<BatchWrite, CatalogError> {
        debug_assert_eq!(batch.len(), texts.len());
        if batch.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let tx = self.conn.transaction()?;
        let ids = Self::upsert_files_in(&tx, batch)?;
        let items: Vec<(i64, &str)> = ids
            .iter()
            .zip(texts.iter())
            .filter_map(|(&id, text)| text.map(|t| (id, t)))
            .collect();
        let shas = Self::store_texts_in(&tx, &mut self.compressor, &items)?;
        tx.commit()?;
        Ok((ids, shas))
    }

    /// Возвращает ID файла по полному пути (точечный запрос по уникальному индексу).
    pub fn file_id_by_path(&self, path: &str) -> Result<Option<i64>, CatalogError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id FROM files WHERE path = ?1;")?;
        match stmt.query_row(params![path], |row| row.get(0)) {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn upsert_files_in(
        tx: &rusqlite::Transaction<'_>,
        batch: &[FileRecord],
    ) -> Result<Vec<i64>, CatalogError> {
        let mut ids = Vec::with_capacity(batch.len());

        {
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT INTO files (
                    path, source_id, volume_id, rel_path, name, extension, size,
                    modified_ns, created_ns, category, inside_archive, container_id,
                    content_status, extraction_version, text_sha, quick_hash, sha256,
                    error, scan_run, available
                ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20
                )
                ON CONFLICT(path) DO UPDATE SET
                    source_id = excluded.source_id,
                    volume_id = excluded.volume_id,
                    rel_path = excluded.rel_path,
                    name = excluded.name,
                    extension = excluded.extension,
                    size = excluded.size,
                    modified_ns = excluded.modified_ns,
                    created_ns = excluded.created_ns,
                    category = excluded.category,
                    inside_archive = excluded.inside_archive,
                    container_id = excluded.container_id,
                    content_status = excluded.content_status,
                    extraction_version = excluded.extraction_version,
                    text_sha = excluded.text_sha,
                    quick_hash = excluded.quick_hash,
                    sha256 = excluded.sha256,
                    error = excluded.error,
                    scan_run = excluded.scan_run,
                    available = excluded.available
                RETURNING id;
                "#,
            )?;

            for item in batch {
                let id: i64 = stmt.query_row(
                    params![
                        item.path,
                        item.source_id,
                        item.volume_id,
                        item.rel_path,
                        item.name,
                        item.extension,
                        item.size as i64,
                        item.modified_ns,
                        item.created_ns,
                        item.category,
                        if item.inside_archive { 1 } else { 0 },
                        item.container_id,
                        item.content_status,
                        item.extraction_version as i64,
                        item.text_sha,
                        item.quick_hash,
                        item.sha256,
                        item.error,
                        item.scan_run,
                        if item.available { 1 } else { 0 },
                    ],
                    |row| row.get(0),
                )?;
                ids.push(id);
            }
        }

        Ok(ids)
    }

    /// Удаляет файлы по списку ID.
    /// Использует временную таблицу для безопасного удаления любого числа параметров (> 999).
    pub fn delete_files(&mut self, ids: &[i64]) -> Result<usize, CatalogError> {
        if ids.is_empty() {
            return Ok(0);
        }

        let tx = self.conn.transaction()?;

        tx.execute(
            "CREATE TEMP TABLE IF NOT EXISTS temp_ids_to_delete (id INTEGER PRIMARY KEY);",
            [],
        )?;
        tx.execute("DELETE FROM temp_ids_to_delete;", [])?;

        {
            let mut insert_stmt =
                tx.prepare_cached("INSERT OR IGNORE INTO temp_ids_to_delete(id) VALUES (?1);")?;
            for &id in ids {
                insert_stmt.execute([id])?;
            }
        }

        // Удаляем сначала членов архивов, чьи контейнеры удаляются
        let deleted_members = tx.execute(
            "DELETE FROM files WHERE container_id IN (SELECT id FROM temp_ids_to_delete);",
            [],
        )?;

        // Удаляем сами файлы
        let deleted_files = tx.execute(
            "DELETE FROM files WHERE id IN (SELECT id FROM temp_ids_to_delete);",
            [],
        )?;

        let deleted = deleted_members + deleted_files;

        tx.execute("DROP TABLE IF EXISTS temp_ids_to_delete;", [])?;
        tx.commit()?;

        Ok(deleted)
    }

    /// Сжимает и сохраняет текст в таблице `texts`, связывает его с файлом.
    /// Одинаковый текст сохраняется ровно один раз.
    /// Возвращает SHA-256 текста.
    pub fn store_text(&mut self, file_id: i64, text: &str) -> Result<String, CatalogError> {
        let res = self.store_text_batch(&[(file_id, text)])?;
        Ok(res
            .into_iter()
            .next()
            .map(|(sha, _)| sha)
            .unwrap_or_default())
    }

    /// Сохраняет порцию текстов в рамках одной транзакции SQLite.
    /// Возвращает список `(SHA-256, is_new)` для каждого переданного элемента,
    /// где `is_new == true`, если текст добавлен в базу впервые.
    pub fn store_text_batch(
        &mut self,
        batch: &[(i64, &str)],
    ) -> Result<Vec<(String, bool)>, CatalogError> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }

        let tx = self.conn.transaction()?;
        let results = Self::store_texts_in(&tx, &mut self.compressor, batch)?;
        tx.commit()?;
        Ok(results)
    }

    fn store_texts_in(
        tx: &rusqlite::Transaction<'_>,
        compressor: &mut zstd::bulk::Compressor<'static>,
        batch: &[(i64, &str)],
    ) -> Result<Vec<(String, bool)>, CatalogError> {
        let mut results = Vec::with_capacity(batch.len());

        {
            let mut insert_text_stmt = tx.prepare_cached(
                "INSERT INTO texts (sha, chars, body) VALUES (?1, ?2, ?3) ON CONFLICT(sha) DO NOTHING;",
            )?;
            let mut update_file_stmt = tx.prepare_cached(
                "UPDATE files SET text_sha = ?1, content_status = 'text', error = NULL WHERE id = ?2;",
            )?;
            let mut update_empty_stmt = tx.prepare_cached(
                "UPDATE files SET text_sha = NULL, content_status = 'metadata-only', error = NULL WHERE id = ?1;",
            )?;

            for &(file_id, text) in batch {
                if text.is_empty() {
                    update_empty_stmt.execute(params![file_id])?;
                    results.push((String::new(), false));
                    continue;
                }

                let mut hasher = Sha256::new();
                hasher.update(text.as_bytes());
                let sha = hex::encode(hasher.finalize());
                let char_count = text.chars().count() as i64;

                // Уровень 1 zstd обеспечивает максимальную скорость потоковой компрессии
                let compressed = compressor
                    .compress(text.as_bytes())
                    .map_err(|e| CatalogError::Compression(e.to_string()))?;

                let inserted = insert_text_stmt.execute(params![sha, char_count, compressed])?;
                update_file_stmt.execute(params![sha, file_id])?;
                let is_new = inserted > 0;
                results.push((sha, is_new));
            }
        }

        Ok(results)
    }

    /// Читает и распаковывает текст по его SHA-256 хэшу.
    pub fn get_text(&self, sha: &str) -> Result<Option<String>, CatalogError> {
        let body: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT body FROM texts WHERE sha = ?1;",
                params![sha],
                |row| row.get(0),
            )
            .optional()?;

        match body {
            Some(bytes) => {
                let decompressed = zstd::decode_all(&bytes[..])
                    .map_err(|e| CatalogError::Compression(e.to_string()))?;
                let text = String::from_utf8(decompressed)
                    .map_err(|e| CatalogError::Compression(e.to_string()))?;
                Ok(Some(text))
            }
            None => Ok(None),
        }
    }

    /// Возвращает все сохранённые тексты каталога: `(sha, text)` (для восстановления индекса).
    pub fn list_all_texts(&self) -> Result<Vec<(String, String)>, CatalogError> {
        let mut stmt = self.conn.prepare("SELECT sha, body FROM texts;")?;
        let mut rows = stmt.query([])?;
        let mut res = Vec::new();
        while let Some(row) = rows.next()? {
            let sha: String = row.get(0)?;
            let compressed: Vec<u8> = row.get(1)?;
            let decompressed = zstd::decode_all(&compressed[..])
                .map_err(|e| CatalogError::Compression(e.to_string()))?;
            let text = String::from_utf8(decompressed)
                .map_err(|e| CatalogError::Compression(e.to_string()))?;
            res.push((sha, text));
        }
        Ok(res)
    }

    /// Строит компактный снимок всех файлов в памяти для быстрой сверки:
    /// `path → (id, size, modified_ns, extraction_version, has_error, container_id)`.
    pub fn snapshot(&self) -> Result<HashMap<String, FileSnapshotEntry>, CatalogError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT path, id, size, modified_ns, extraction_version, (error IS NOT NULL), container_id, text_sha FROM files;",
        )?;

        let mut map = HashMap::new();
        let mut rows = stmt.query([])?;

        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let id: i64 = row.get(1)?;
            let size: i64 = row.get(2)?;
            let modified_ns: i64 = row.get(3)?;
            let extraction_version: i64 = row.get(4)?;
            let has_error: bool = row.get(5)?;
            let container_id: Option<i64> = row.get(6)?;
            let text_sha: Option<String> = row.get(7)?;

            map.insert(
                path,
                FileSnapshotEntry {
                    id,
                    size: size as u64,
                    modified_ns,
                    extraction_version: extraction_version as u32,
                    has_error,
                    container_id,
                    text_sha,
                },
            );
        }

        Ok(map)
    }

    /// Забирает и очищает очередь удалений из индекса `index_outbox`.
    pub fn drain_index_outbox(&mut self) -> Result<Vec<(String, String)>, CatalogError> {
        let tx = self.conn.transaction()?;
        let mut results = Vec::new();

        {
            let mut stmt = tx.prepare("SELECT sha, op FROM index_outbox ORDER BY id ASC;")?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let sha: String = row.get(0)?;
                let op: String = row.get(1)?;
                results.push((sha, op));
            }
        }

        tx.execute("DELETE FROM index_outbox;", [])?;
        tx.commit()?;

        Ok(results)
    }

    /// Добавляет или обновляет запись о томе по серийному номеру.
    pub fn record_volume(
        &mut self,
        serial: u32,
        label: &str,
        last_letter: &str,
    ) -> Result<i64, CatalogError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let id: i64 = self.conn.query_row(
            r#"
            INSERT INTO volumes (serial, label, last_letter, last_seen)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(serial) DO UPDATE SET
                label = excluded.label,
                last_letter = excluded.last_letter,
                last_seen = excluded.last_seen
            RETURNING id;
            "#,
            params![serial as i64, label, last_letter, now],
            |row| row.get(0),
        )?;

        Ok(id)
    }

    /// Регистрирует или обновляет источник индексирования.
    pub fn record_source(&mut self, source: &SourceRecord) -> Result<i64, CatalogError> {
        let id: i64 = self.conn.query_row(
            r#"
            INSERT INTO sources (path, mode, max_depth, enabled, volume_id, status, last_indexed_at, details)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(path) DO UPDATE SET
                mode = excluded.mode,
                max_depth = excluded.max_depth,
                enabled = excluded.enabled,
                volume_id = excluded.volume_id,
                status = excluded.status,
                last_indexed_at = excluded.last_indexed_at,
                details = excluded.details
            RETURNING id;
            "#,
            params![
                source.path,
                source.mode,
                source.max_depth as i64,
                if source.enabled { 1 } else { 0 },
                source.volume_id,
                source.status,
                source.last_indexed_at,
                source.details,
            ],
            |row| row.get(0),
        )?;

        Ok(id)
    }

    /// Возвращает источник по пути.
    pub fn get_source_by_path(&self, path: &str) -> Result<Option<SourceRecord>, CatalogError> {
        let res = self
            .conn
            .query_row(
                "SELECT id, path, mode, max_depth, enabled, volume_id, status, last_indexed_at, details FROM sources WHERE path = ?1;",
                params![path],
                |row| {
                    Ok(SourceRecord {
                        id: Some(row.get(0)?),
                        path: row.get(1)?,
                        mode: row.get(2)?,
                        max_depth: row.get::<_, i64>(3)? as u32,
                        enabled: row.get::<_, i64>(4)? != 0,
                        volume_id: row.get(5)?,
                        status: row.get(6)?,
                        last_indexed_at: row.get(7)?,
                        details: row.get(8)?,
                    })
                },
            )
            .optional()?;

        Ok(res)
    }

    /// Возвращает список всех источников, отсортированных по пути без учета регистра (R1-А-62).
    pub fn list_sources(&self) -> Result<Vec<SourceRecord>, CatalogError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, mode, max_depth, enabled, volume_id, status, last_indexed_at, details FROM sources ORDER BY path COLLATE NOCASE ASC;",
        )?;

        let mut sources = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            sources.push(SourceRecord {
                id: Some(row.get(0)?),
                path: row.get(1)?,
                mode: row.get(2)?,
                max_depth: row.get::<_, i64>(3)? as u32,
                enabled: row.get::<_, i64>(4)? != 0,
                volume_id: row.get(5)?,
                status: row.get(6)?,
                last_indexed_at: row.get(7)?,
                details: row.get(8)?,
            });
        }

        Ok(sources)
    }

    /// Проставляет источник и том файлам, у которых `source_id` ещё не задан (№221).
    ///
    /// `sources` — `(id источника, id тома, путь источника)`. Файл принадлежит источнику, чей путь
    /// совпадает с его путём или является его началом (`путь\…`, `путь → …` для членов архива).
    /// Вложенные источники: самый длинный путь обрабатывается первым и забирает свои файлы,
    /// короткому остаётся то, что ещё не занято. Всё выполняется одной транзакцией.
    /// Возвращает число обновлённых записей.
    pub fn assign_sources_to_files(
        &mut self,
        sources: &[(i64, Option<i64>, String)],
    ) -> Result<usize, CatalogError> {
        let mut ordered: Vec<(i64, Option<i64>, &str)> = sources
            .iter()
            .map(|(id, volume, path)| (*id, *volume, path.trim_end_matches('\\')))
            .filter(|(_, _, path)| !path.is_empty())
            .collect();
        ordered.sort_by_key(|(_, _, path)| std::cmp::Reverse(path.chars().count()));

        let tx = self.conn.transaction()?;
        let mut total = 0usize;
        {
            // Сравнение через substr: символы % и _ в путях не требуют экранирования, как в LIKE
            let mut stmt = tx.prepare_cached(
                "UPDATE files SET source_id = ?1, volume_id = COALESCE(?2, volume_id) \
                 WHERE source_id IS NULL \
                   AND (path = ?3 OR substr(path, 1, ?4) = ?5 OR substr(path, 1, ?6) = ?7);",
            )?;
            for (source_id, volume_id, path) in ordered {
                let with_slash = format!("{path}\\");
                let with_arrow = format!("{path}{}", crate::rules::ARCHIVE_SEPARATOR);
                total += stmt.execute(params![
                    source_id,
                    volume_id,
                    path,
                    with_slash.chars().count() as i64,
                    with_slash,
                    with_arrow.chars().count() as i64,
                    with_arrow,
                ])?;
            }
        }
        tx.commit()?;
        Ok(total)
    }

    /// Обновляет статус источника.
    pub fn update_source_status(
        &mut self,
        source_id: i64,
        status: &str,
        details: Option<&str>,
    ) -> Result<(), CatalogError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.conn.execute(
            "UPDATE sources SET status = ?1, details = ?2, last_indexed_at = ?3 WHERE id = ?4;",
            params![status, details, now, source_id],
        )?;

        Ok(())
    }

    /// Удаляет источник из таблицы sources.
    pub fn delete_source(&mut self, source_id: i64) -> Result<(), CatalogError> {
        self.conn
            .execute("DELETE FROM sources WHERE id = ?1;", params![source_id])?;
        Ok(())
    }

    /// Регистрирует запуск сканирования или индексации.
    pub fn record_run(
        &mut self,
        phase: &str,
        status: &str,
        details: Option<&str>,
    ) -> Result<i64, CatalogError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let id: i64 = self.conn.query_row(
            "INSERT INTO runs (started, phase, status, details) VALUES (?1, ?2, ?3, ?4) RETURNING id;",
            params![now, phase, status, details],
            |row| row.get(0),
        )?;

        Ok(id)
    }

    /// Завершает запуск, проставляя время окончания и статус.
    pub fn finish_run(
        &mut self,
        run_id: i64,
        status: &str,
        details: Option<&str>,
    ) -> Result<(), CatalogError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        self.conn.execute(
            "UPDATE runs SET finished = ?1, status = ?2, details = ?3 WHERE id = ?4;",
            params![now, status, details, run_id],
        )?;

        Ok(())
    }

    /// Добавляет запись о пропущенном файле или папке.
    pub fn add_skipped(
        &mut self,
        run_id: i64,
        path: &str,
        reason: &str,
    ) -> Result<(), CatalogError> {
        self.conn.execute(
            "INSERT INTO skipped (path, reason, scan_run) VALUES (?1, ?2, ?3);",
            params![path, reason, run_id],
        )?;
        Ok(())
    }

    /// Возвращает общее число записей в таблице files.
    pub fn count_files(&self) -> Result<usize, CatalogError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM files;", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Возвращает ссылку на внутреннее соединение с SQLite (для запросов только на чтение).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str) -> FileRecord {
        FileRecord {
            id: None,
            path: path.to_string(),
            source_id: None,
            volume_id: None,
            rel_path: path.to_string(),
            name: path.to_string(),
            extension: "txt".into(),
            size: 10,
            modified_ns: 1,
            created_ns: 1,
            category: "text".into(),
            inside_archive: false,
            container_id: None,
            content_status: Some("pending".into()),
            extraction_version: 1,
            text_sha: None,
            quick_hash: None,
            sha256: None,
            error: None,
            scan_run: Some(1),
            available: true,
        }
    }

    fn open_temp() -> (tempfile::TempDir, Catalog) {
        let dir = tempfile::tempdir().expect("временная папка");
        let cat = Catalog::open_or_create(&dir.path().join("c.sqlite3")).expect("каталог");
        (dir, cat)
    }

    #[test]
    fn write_batch_pishet_fayly_i_teksty_vmeste() {
        let (_dir, mut cat) = open_temp();
        let recs = vec![rec("a.txt"), rec("b.txt"), rec("c.txt")];
        let texts = [Some("привет мир"), None, Some("привет мир")];
        let (ids, shas) = cat.write_batch(&recs, &texts).expect("запись порции");

        assert_eq!(ids.len(), 3);
        // два текста у первого и третьего файла; одинаковый текст — новый только в первый раз
        assert_eq!(shas.len(), 2);
        assert!(shas[0].1);
        assert!(!shas[1].1);
        assert_eq!(shas[0].0, shas[1].0);
        assert_eq!(
            cat.get_text(&shas[0].0).expect("текст").as_deref(),
            Some("привет мир")
        );
        assert_eq!(cat.count_files().expect("число файлов"), 3);

        let sha_a: Option<String> = cat
            .connection()
            .query_row("SELECT text_sha FROM files WHERE id = ?1", [ids[0]], |r| {
                r.get(0)
            })
            .expect("text_sha");
        assert_eq!(sha_a.as_deref(), Some(shas[0].0.as_str()));
        let sha_b: Option<String> = cat
            .connection()
            .query_row("SELECT text_sha FROM files WHERE id = ?1", [ids[1]], |r| {
                r.get(0)
            })
            .expect("text_sha");
        assert_eq!(sha_b, None);
    }

    #[test]
    fn write_batch_obnovlyaet_sushchestvuyushchiy_fayl() {
        let (_dir, mut cat) = open_temp();
        let (ids1, _) = cat
            .write_batch(&[rec("a.txt")], &[Some("первый")])
            .expect("первая запись");
        let (ids2, shas2) = cat
            .write_batch(&[rec("a.txt")], &[Some("второй")])
            .expect("вторая запись");
        assert_eq!(ids1, ids2);
        assert_eq!(cat.count_files().expect("число файлов"), 1);
        // старый текст убран триггером, новый на месте
        let texts: i64 = cat
            .connection()
            .query_row("SELECT COUNT(*) FROM texts", [], |r| r.get(0))
            .expect("число текстов");
        assert_eq!(texts, 1);
        assert_eq!(
            cat.get_text(&shas2[0].0).expect("текст").as_deref(),
            Some("второй")
        );
        // а удаление старого текста попало в очередь удалений индекса
        let outbox = cat.drain_index_outbox().expect("очередь");
        assert_eq!(outbox.len(), 1);
        assert_eq!(outbox[0].1, "delete");
    }

    #[test]
    fn write_batch_pustoy_poryadok() {
        let (_dir, mut cat) = open_temp();
        let (ids, shas) = cat.write_batch(&[], &[]).expect("пустая порция");
        assert!(ids.is_empty() && shas.is_empty());
    }

    #[test]
    fn file_id_by_path_nahodit_fayl() {
        let (_dir, mut cat) = open_temp();
        let ids = cat.upsert_files(&[rec("x.txt")]).expect("запись");
        assert_eq!(cat.file_id_by_path("x.txt").expect("поиск"), Some(ids[0]));
        assert_eq!(cat.file_id_by_path("net.txt").expect("поиск"), None);
    }

    #[test]
    fn szhatie_chitaetsya_obychnym_zstd() {
        let (_dir, mut cat) = open_temp();
        let text = "слово ".repeat(1000);
        let (_, shas) = cat
            .write_batch(&[rec("a.txt")], &[Some(text.as_str())])
            .expect("запись");
        let body: Vec<u8> = cat
            .connection()
            .query_row("SELECT body FROM texts WHERE sha = ?1", [&shas[0].0], |r| {
                r.get(0)
            })
            .expect("тело");
        let raw = zstd::decode_all(&body[..]).expect("распаковка");
        assert_eq!(raw, text.as_bytes());
    }

    #[test]
    fn nastroyki_pisatelya() {
        let (_dir, cat) = open_temp();
        let conn = cat.connection();
        let mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
            .expect("journal_mode");
        assert_eq!(mode.to_lowercase(), "wal");
        let sync: i64 = conn
            .query_row("PRAGMA synchronous;", [], |r| r.get(0))
            .expect("synchronous");
        assert_eq!(sync, 1, "NORMAL");
        let temp: i64 = conn
            .query_row("PRAGMA temp_store;", [], |r| r.get(0))
            .expect("temp_store");
        assert_eq!(temp, 2, "MEMORY");
        let cache: i64 = conn
            .query_row("PRAGMA cache_size;", [], |r| r.get(0))
            .expect("cache_size");
        assert_eq!(cache, -WRITER_CACHE_KIB);
    }
}

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use crate::catalog::{Catalog, CatalogError, FileRecord, FileSnapshotEntry, SourceRecord};
use crate::rules::{
    ARCHIVE_SEPARATOR, LEGACY_CHANGED_TEXT_EXTENSIONS, TEXT_EXTRACTION_VERSION, category_for,
    refine_book_categories_in_list,
};
use crate::settings::{Settings, Source};
use crate::walk::{WalkEntry, walk_source};

/// Решение сверки одного файла со снимком.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncDecision {
    /// Новый файл, отсутствует в снимке.
    New,
    /// Файл изменился (размер, mtime или повторная попытка после ошибки).
    Changed { id: i64 },
    /// Файл не изменился, но правила извлечения устарели (extraction_version < current).
    StaleExtraction { id: i64 },
    /// Файл полностью идентичен снимку.
    Unchanged { id: i64 },
}

/// Статистика выполнения синхронизации.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub new_files: usize,
    pub changed_files: usize,
    pub stale_files: usize,
    pub unchanged_files: usize,
    pub deleted_files: usize,
    pub archives_changed: usize,
    pub archives_unchanged: usize,
    pub skipped_count: usize,
}

/// Ошибки синхронизации.
#[derive(Debug)]
pub enum SyncError {
    Catalog(CatalogError),
    SourceUnavailable(String),
}

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(err) => write!(f, "Ошибка каталога при синхронизации: {err}"),
            Self::SourceUnavailable(msg) => write!(f, "Источник недоступен: {msg}"),
        }
    }
}

impl std::error::Error for SyncError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Catalog(err) => Some(err),
            _ => None,
        }
    }
}

impl From<CatalogError> for SyncError {
    fn from(err: CatalogError) -> Self {
        Self::Catalog(err)
    }
}

/// Сопоставляет запись обхода со снимком базы данных.
pub fn classify_entry(
    entry: &WalkEntry,
    snapshot: &HashMap<String, FileSnapshotEntry>,
) -> SyncDecision {
    match snapshot.get(&entry.path) {
        None => SyncDecision::New,
        Some(existing) => {
            if existing.has_error {
                // Если при прошлом сканировании была ошибка, пробуем перечитать
                SyncDecision::Changed { id: existing.id }
            } else if entry.size != existing.size || entry.modified_ns != existing.modified_ns {
                SyncDecision::Changed { id: existing.id }
            } else if is_stale_extraction(entry, existing.extraction_version) {
                SyncDecision::StaleExtraction { id: existing.id }
            } else {
                SyncDecision::Unchanged { id: existing.id }
            }
        }
    }
}

/// Проверяет, устарело ли извлечение текста для файла (R1-А-80, R1-Б-29).
pub fn is_stale_extraction(entry: &WalkEntry, existing_version: u32) -> bool {
    if existing_version >= TEXT_EXTRACTION_VERSION {
        return false;
    }

    let ext = Path::new(&entry.path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{}", s.to_lowercase()))
        .unwrap_or_default();

    LEGACY_CHANGED_TEXT_EXTENSIONS.contains(&ext.as_str())
}

/// Регистрирует том и источник в каталоге; общее правило для `sync_source` и основного конвейера.
///
/// Том записывается по серийному номеру (если он известен), источник — по пути в том же виде,
/// что и в `settings.sources` (путь как есть, без правки хвостовой черты).
/// Если сведений о томе нет, прежняя привязка источника к тому сохраняется.
/// Возвращает `(id источника, id тома)`.
pub fn register_source(
    catalog: &mut Catalog,
    source: &Source,
    volume_serial: Option<u32>,
    volume_label: Option<&str>,
    status: &str,
) -> Result<(i64, Option<i64>), CatalogError> {
    let source_path_str = source.path.to_string_lossy().to_string();
    let volume_id = match volume_serial {
        Some(serial) => {
            let letter = source_path_str.chars().take(2).collect::<String>();
            Some(catalog.record_volume(serial, volume_label.unwrap_or(""), &letter)?)
        }
        None => catalog
            .get_source_by_path(&source_path_str)?
            .and_then(|existing| existing.volume_id),
    };
    let source_id = catalog.record_source(&SourceRecord {
        id: None,
        path: source_path_str,
        mode: format!("{:?}", source.mode),
        max_depth: source.max_depth,
        enabled: source.enabled,
        volume_id,
        status: status.to_string(),
        last_indexed_at: None,
        details: None,
    })?;
    Ok((source_id, volume_id))
}

/// Выполняет синхронизацию источника с каталогом (R1-А-73..85, 114, 115).
pub fn sync_source(
    catalog: &mut Catalog,
    source: &Source,
    settings: &Settings,
) -> Result<SyncStats, SyncError> {
    let source_path_str = source.path.to_string_lossy().to_string();

    // 1. Получаем текущий снимок каталога
    let snapshot = catalog.snapshot()?;

    // 2. Подготавливаем замыкание для проверки неизменности архивов (R1-А-67, 79; R1-Б-27)
    // Собираем карту членов архивов: container_id -> Vec<FileSnapshotEntry с путями>
    let mut archive_members_map: HashMap<i64, Vec<WalkEntry>> = HashMap::new();
    for (path, snap_entry) in &snapshot {
        if let Some(cid) = snap_entry.container_id {
            let name = path.rsplit('\\').next().unwrap_or(path).to_string();
            archive_members_map.entry(cid).or_default().push(WalkEntry {
                path: path.clone(),
                rel_path: name,
                size: snap_entry.size,
                modified_ns: snap_entry.modified_ns,
                created_ns: 0,
                is_dir: false,
                inside_archive: true,
                container_path: None,
                document_only: false,
            });
        }
    }

    let mut unchanged_archives_count = 0usize;
    let checker = |archive_path: &str, size: u64, modified_ns: i64| -> Option<Vec<WalkEntry>> {
        if let Some(existing) = snapshot.get(archive_path)
            && !existing.has_error
            && existing.size == size
            && existing.modified_ns == modified_ns
        {
            // Неизменённый ZIP: берем записи членов из снимка, сам ZIP не открываем
            return Some(
                archive_members_map
                    .get(&existing.id)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        None
    };

    // 3. Выполняем обход источника
    let walk_result = walk_source(source, settings, Some(&checker));

    // 4. Проверяем доступность источника (Е-3)
    if !walk_result.is_available {
        // Источник недоступен (отключённый диск или удалённый корень)
        // Записи тома НЕ удаляются, источник помечается «недоступен»
        if let Some(existing_source) = catalog.get_source_by_path(&source_path_str)? {
            catalog.update_source_status(
                existing_source.id.unwrap_or(0),
                "недоступен",
                Some("Корень источника недоступен"),
            )?;
        }
        return Err(SyncError::SourceUnavailable(source_path_str));
    }

    // 5–6. Регистрируем том (при наличии сведений) и источник в таблице sources
    let (source_db_id, volume_id) = register_source(
        catalog,
        source,
        walk_result.volume_serial,
        walk_result.volume_label.as_deref(),
        "indexing",
    )?;

    // 7. Регистрируем запуск сканирования
    let run_id = catalog.record_run("sync", "running", None)?;

    // 8. Записываем пропущенные элементы
    for skip in walk_result.skips() {
        catalog.add_skipped(run_id, &skip.path, &skip.reason)?;
    }

    let mut stats = SyncStats {
        skipped_count: walk_result.skips().len(),
        ..Default::default()
    };

    let mut seen_paths = HashSet::new();
    let mut files_to_upsert = Vec::new();
    let mut changed_archive_container_ids = HashSet::new();

    // 9. Классифицируем каждую запись обхода
    for entry in walk_result.entries() {
        seen_paths.insert(entry.path.clone());
        let decision = classify_entry(entry, &snapshot);

        match decision {
            SyncDecision::New => {
                stats.new_files += 1;
                files_to_upsert.push(entry);
            }
            SyncDecision::Changed { id } => {
                stats.changed_files += 1;
                // Если изменился архив-контейнер, все его старые члены должны быть заменены (R1-А-78, R1-Б-37)
                if entry.path.ends_with(".zip") && !entry.inside_archive {
                    stats.archives_changed += 1;
                    changed_archive_container_ids.insert(id);
                }
                files_to_upsert.push(entry);
            }
            SyncDecision::StaleExtraction { .. } => {
                stats.stale_files += 1;
                files_to_upsert.push(entry);
            }
            SyncDecision::Unchanged { id: _ } => {
                stats.unchanged_files += 1;
                if entry.path.ends_with(".zip") && !entry.inside_archive {
                    unchanged_archives_count += 1;
                }
            }
        }
    }
    stats.archives_unchanged = unchanged_archives_count;

    // 10. Формируем список ID на удаление
    let mut ids_to_delete = Vec::new();

    // 10.1. Члены изменённых архивов подлежат удалению и повторной вставке (R1-А-78, R1-Б-37)
    for cid in &changed_archive_container_ids {
        if let Some(members) = archive_members_map.get(cid) {
            for m in members {
                if let Some(snap_entry) = snapshot.get(&m.path) {
                    ids_to_delete.push(snap_entry.id);
                }
            }
        }
    }

    // 10.2. Удаление отсутствующих файлов источника (R1-А-82)
    // Исключаются:
    // - записи внутри защищённых ветвей (walk_result.protected_prefixes, R1-А-75, 76)
    // - записи других источников
    let is_dir_source = source.path.is_dir();
    let source_prefix_slash = format!("{}\\", source_path_str.trim_end_matches('\\'));

    for (snap_path, snap_entry) in &snapshot {
        // Проверяем принадлежность пути данному источнику
        let belongs_to_source = if is_dir_source {
            snap_path == &source_path_str || snap_path.starts_with(&source_prefix_slash)
        } else {
            snap_path == &source_path_str
        };

        if !belongs_to_source {
            continue;
        }

        // Если файл не был встречен при текущем обходе
        if !seen_paths.contains(snap_path) {
            // Проверяем, не находится ли файл в защищенной ветви (ошибка чтения папки)
            let is_protected = walk_result
                .protected_prefixes
                .iter()
                .any(|prefix| snap_path.starts_with(prefix));

            if !is_protected {
                ids_to_delete.push(snap_entry.id);
            }
        }
    }

    // 11. Выполняем удаление пакетом через временную таблицу (> 999 параметров, R1-А-90, R1-Б-36)
    let deleted_count = catalog.delete_files(&ids_to_delete)?;
    stats.deleted_files = deleted_count;

    // 12. Подготавливаем порцию для добавления/обновления
    // Сначала обычные файлы и архивы, затем внутренние члены архивов
    let mut records_to_upsert: Vec<FileRecord> = files_to_upsert
        .iter()
        .map(|entry| {
            let cat = category_for(&entry.path, &settings.book_folder_words);
            let ext = Path::new(&entry.path)
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| format!(".{}", s.to_lowercase()))
                .unwrap_or_default();

            let name = if entry.inside_archive {
                entry
                    .path
                    .rsplit(ARCHIVE_SEPARATOR)
                    .next()
                    .unwrap_or(&entry.path)
                    .rsplit('\\')
                    .next()
                    .unwrap_or(&entry.path)
                    .to_string()
            } else {
                Path::new(&entry.path)
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            };

            FileRecord {
                id: snapshot.get(&entry.path).map(|e| e.id),
                path: entry.path.clone(),
                source_id: Some(source_db_id),
                volume_id,
                rel_path: entry.rel_path.clone(),
                name,
                extension: ext,
                size: entry.size,
                modified_ns: entry.modified_ns,
                created_ns: entry.created_ns,
                category: cat.to_string(),
                inside_archive: entry.inside_archive,
                container_id: None, // установим после вставки контейнеров
                content_status: Some("pending".to_string()),
                extraction_version: 0,
                text_sha: None,
                quick_hash: None,
                sha256: None,
                error: None,
                scan_run: Some(run_id),
                available: true,
            }
        })
        .collect();

    // 13. Уточняем категории электронных книг по соседству (R1-А-39, R1-Б-5)
    refine_book_categories_in_list(
        &mut records_to_upsert,
        |r| &r.path,
        |r| &r.extension,
        |r, new_cat| r.category = new_cat.to_string(),
    );

    // 14. Сначала добавляем внешние файлы и архивы, чтобы получить их id для связи container_id
    let mut ordinary_records = Vec::new();
    let mut archive_member_records = Vec::new();
    for rec in records_to_upsert {
        if rec.inside_archive {
            archive_member_records.push(rec);
        } else {
            ordinary_records.push(rec);
        }
    }

    catalog.upsert_files(&ordinary_records)?;

    // Если есть члены архивов, связываем их с id родительских архивов (R1-А-18, 54, 78)
    if !archive_member_records.is_empty() {
        // Обновляем снимок, чтобы знать актуальные id контейнеров
        let updated_snapshot = catalog.snapshot()?;
        for member in &mut archive_member_records {
            if let Some(archive_path) = member.path.split(ARCHIVE_SEPARATOR).next()
                && let Some(container_entry) = updated_snapshot.get(archive_path)
            {
                member.container_id = Some(container_entry.id);
            }
        }
        catalog.upsert_files(&archive_member_records)?;
    }

    // 14а. Неизменённые файлы без привязки получают источник и том (№221)
    catalog.assign_sources_to_files(&[(source_db_id, volume_id, source_path_str.clone())])?;

    // 15. Обновляем статус источника на "ready" (R1-А-88)
    let total_files = catalog.count_files()?;
    catalog.update_source_status(
        source_db_id,
        "ready",
        Some(&format!("Файлов: {total_files}")),
    )?;

    // 16. Завершаем запуск со статусом completed
    catalog.finish_run(
        run_id,
        "completed",
        Some(&format!(
            "new: {}, changed: {}, deleted: {}",
            stats.new_files, stats.changed_files, stats.deleted_files
        )),
    )?;

    Ok(stats)
}

/// Удаляет источник, каталог или файл из базы данных по пути или префиксу (R1-А-89..91).
/// Сами файлы на диске не затрагиваются.
pub fn remove_source(catalog: &mut Catalog, path: &str) -> Result<usize, SyncError> {
    let normalized = path.replace('/', "\\");
    let target = normalized.trim_end_matches('\\');

    // Находим все файлы в каталоге, совпадающие с путем или начинающиеся с target\ или target →
    let snapshot = catalog.snapshot()?;
    let prefix_slash = format!("{target}\\");
    let prefix_arrow = format!("{target}{ARCHIVE_SEPARATOR}");

    let mut ids_to_delete = Vec::new();
    for (file_path, entry) in &snapshot {
        if file_path == target
            || file_path.starts_with(&prefix_slash)
            || file_path.starts_with(&prefix_arrow)
        {
            ids_to_delete.push(entry.id);
        }
    }

    // Удаляем файлы через delete_files (временная таблица, безопасная при > 999 записях)
    let deleted = catalog.delete_files(&ids_to_delete)?;

    // Если был зарегистрирован источник с таким путем, удаляем его
    if let Some(source_rec) = catalog.get_source_by_path(path)? {
        if let Some(sid) = source_rec.id {
            catalog.delete_source(sid)?;
        }
    } else if let Some(source_rec) = catalog.get_source_by_path(target)?
        && let Some(sid) = source_rec.id
    {
        catalog.delete_source(sid)?;
    }

    Ok(deleted)
}

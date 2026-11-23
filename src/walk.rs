use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::rules::{
    ARCHIVE_SEPARATOR, DEFAULT_MAX_ARCHIVE_MEMBER_SIZE, effective_exclude_paths,
    is_archive_content_extension, is_document_only_extension, is_file_excluded,
    is_folder_name_excluded, is_path_excluded, normalize_path_str,
};
use crate::settings::{Settings, Source, SourceMode};

/// Битовая маска атрибута reparse point (символические ссылки, точки монтирования) в Windows.
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// Атрибуты файлов Windows для облачных файлов и файлов с отложенной загрузкой (OneDrive, Google Drive).
pub const FILE_ATTRIBUTE_OFFLINE: u32 = 0x0000_1000;
pub const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x0004_0000;
pub const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

/// Проверяет, является ли файл облачным (содержимое не скачано локально).
pub fn is_cloud_file_attributes(attributes: u32) -> bool {
    (attributes
        & (FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_OFFLINE))
        != 0
}

/// Проверяет, является ли значение атрибута точкой повторного анализа (R1-А-38, R1-Б-1).
pub fn is_reparse_attribute(attributes: u32) -> bool {
    (attributes & FILE_ATTRIBUTE_REPARSE_POINT) != 0
}

/// Проверяет, является ли файл или папка точкой повторного анализа Windows.
#[cfg(windows)]
pub fn is_reparse_point(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    is_reparse_attribute(meta.file_attributes())
}

#[cfg(not(windows))]
pub fn is_reparse_point(meta: &fs::Metadata) -> bool {
    meta.file_type().is_symlink()
}

/// Извлекает размер, время модификации и создания в наносекундах.
pub fn get_metadata_times_and_size(meta: &fs::Metadata) -> (u64, i64, i64) {
    let size = meta.len();
    let modified_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let created_ns = meta
        .created()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    (size, modified_ns, created_ns)
}

/// Получает серийный номер и метку тома для пути через Windows API (GetVolumeInformationW).
#[cfg(windows)]
pub fn get_volume_info(path: &Path) -> (Option<u32>, Option<String>) {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Storage::FileSystem::GetVolumeInformationW;
    use windows::core::PCWSTR;

    let root = match path.components().next() {
        Some(std::path::Component::Prefix(prefix)) => {
            let p = prefix.as_os_str().to_string_lossy();
            format!("{}\\", p)
        }
        _ => return (None, None),
    };

    let root_wide: Vec<u16> = std::ffi::OsStr::new(&root)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut volume_name = [0u16; 261];
    let mut serial: u32 = 0;

    unsafe {
        let res = GetVolumeInformationW(
            PCWSTR(root_wide.as_ptr()),
            Some(&mut volume_name),
            Some(&mut serial),
            None,
            None,
            None,
        );
        if res.is_ok() {
            let len = volume_name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(volume_name.len());
            let label = String::from_utf16_lossy(&volume_name[..len]);
            (Some(serial), Some(label))
        } else {
            (None, None)
        }
    }
}

#[cfg(not(windows))]
pub fn get_volume_info(_path: &Path) -> (Option<u32>, Option<String>) {
    (None, None)
}

/// Запись о найденном при обходе файле.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkEntry {
    pub path: String,
    pub rel_path: String,
    pub size: u64,
    pub modified_ns: i64,
    pub created_ns: i64,
    pub is_dir: bool,
    pub inside_archive: bool,
    pub container_path: Option<String>,
    pub document_only: bool,
}

/// Запись о пропуске файла или папки с указанием причины.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkSkip {
    pub path: String,
    pub reason: String,
    pub is_protected: bool,
}

/// Элемент потока обхода файловой системы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkItem {
    Entry(WalkEntry),
    Skip(WalkSkip),
}

/// Результат полного обхода одного источника.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkResult {
    pub items: Vec<WalkItem>,
    pub volume_serial: Option<u32>,
    pub volume_label: Option<String>,
    pub protected_prefixes: Vec<String>,
    pub is_available: bool,
}

impl WalkResult {
    /// Возвращает только успешные записи файлов.
    pub fn entries(&self) -> Vec<&WalkEntry> {
        self.items
            .iter()
            .filter_map(|item| match item {
                WalkItem::Entry(e) => Some(e),
                _ => None,
            })
            .collect()
    }

    /// Возвращает только пропуски.
    pub fn skips(&self) -> Vec<&WalkSkip> {
        self.items
            .iter()
            .filter_map(|item| match item {
                WalkItem::Skip(s) => Some(s),
                _ => None,
            })
            .collect()
    }
}

/// Декодирует имя файла из ZIP-архива: UTF-8, либо CP866 при наличии не-ASCII байтов (R1-А-34).
pub fn decode_zip_filename(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (cow, _, _) = encoding_rs::IBM866.decode(raw);
            cow.into_owned()
        }
    }
}

/// Имя члена ZIP-архива в том виде, в котором оно хранится в базе: декодирование
/// (`decode_zip_filename`) и замена '/' на '\\'. Единственное правило имени для обхода и чтения.
pub fn normalize_zip_member_name(raw: &[u8]) -> String {
    decode_zip_filename(raw).replace('/', "\\")
}

/// Ищет в архиве член, чьё нормализованное имя точно равно `member_name`
/// (имя взято из базы, то есть получено через `normalize_zip_member_name`).
pub fn find_zip_member_index<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    member_name: &str,
) -> Option<usize> {
    let is_member = |archive: &mut zip::ZipArchive<R>, i: usize| -> bool {
        archive
            .by_index_raw(i)
            .is_ok_and(|f| !f.is_dir() && normalize_zip_member_name(f.name_raw()) == member_name)
    };
    // Быстрый путь: имя в архиве хранится как есть (с '\\') или с '/'. Найденное
    // всё равно проверяется по общему правилу.
    let with_slashes = member_name.replace('\\', "/");
    for candidate in [member_name, with_slashes.as_str()] {
        if let Some(i) = archive.index_for_name(candidate)
            && is_member(archive, i)
        {
            return Some(i);
        }
    }
    // Медленный путь: имена не в UTF-8 (CP866) крейт читает как CP437, поэтому перебор.
    (0..archive.len()).find(|&i| is_member(archive, i))
}

/// Структура задания для стека обхода папок.
struct DirStackItem {
    path: PathBuf,
    rel_path: String,
    depth: u32,
    document_only: bool,
}

/// Функция проверки неизменности архива: принимает `(archive_path, size, modified_ns)`.
/// Если архив не изменился, возвращает `Some(cached_entries)`.
pub type ArchiveUnchangedChecker<'a> = dyn Fn(&str, u64, i64) -> Option<Vec<WalkEntry>> + Sync + 'a;

/// Выполняет обход источника по правилам ТЗ без обращения к базе данных.
pub fn walk_source(
    source: &Source,
    settings: &Settings,
    is_archive_unchanged: Option<&ArchiveUnchangedChecker>,
) -> WalkResult {
    walk_source_counting(source, settings, is_archive_unchanged, None)
}

/// Выполняет обход источника и увеличивает общий счётчик найденных файлов во время обхода.
/// Счётчик нужен окну программы для полосы хода; без счётчика поведение прежнее.
pub fn walk_source_counting(
    source: &Source,
    settings: &Settings,
    is_archive_unchanged: Option<&ArchiveUnchangedChecker>,
    found: Option<&AtomicUsize>,
) -> WalkResult {
    let source_path = &source.path;
    let (volume_serial, volume_label) = get_volume_info(source_path);

    // Проверка доступности корня источника (Е-3)
    if !source_path.exists() {
        return WalkResult {
            items: vec![WalkItem::Skip(WalkSkip {
                path: source_path.to_string_lossy().to_string(),
                reason: "Источник недоступен (путь не существует)".to_string(),
                is_protected: true,
            })],
            volume_serial,
            volume_label,
            protected_prefixes: vec![source_path.to_string_lossy().to_string()],
            is_available: false,
        };
    }

    // Папка данных программы исключается ВСЕГДА, даже внутри источника
    let data_dir_buf;
    let data_dir_ref = if !settings.data_dir.as_os_str().is_empty() {
        Some(settings.data_dir.as_path())
    } else {
        data_dir_buf = crate::paths::local_appdata_dir().join("ПоисКовичеК");
        Some(data_dir_buf.as_path())
    };

    if let Some(dd) = data_dir_ref {
        let dd_clean = normalize_path_str(&dd.to_string_lossy());
        if !dd_clean.is_empty() {
            let src_clean = normalize_path_str(&source_path.to_string_lossy());
            if src_clean == dd_clean || src_clean.starts_with(&format!("{dd_clean}\\")) {
                return WalkResult {
                    items: vec![WalkItem::Skip(WalkSkip {
                        path: source_path.to_string_lossy().to_string(),
                        reason: "исключено правилом".to_string(),
                        is_protected: false,
                    })],
                    volume_serial,
                    volume_label,
                    protected_prefixes: Vec::new(),
                    is_available: true,
                };
            }
        }
    }

    // Если источник — одиночный явно выбранный файл (R1-А-66, R1-Б-11)
    if source_path.is_file() {
        return walk_single_file(source, settings, is_archive_unchanged, found);
    }

    // Если источник — папка
    let metadata = match fs::symlink_metadata(source_path) {
        Ok(m) => m,
        Err(err) => {
            return WalkResult {
                items: vec![WalkItem::Skip(WalkSkip {
                    path: source_path.to_string_lossy().to_string(),
                    reason: format!("Ошибка чтения корня источника: {err}"),
                    is_protected: true,
                })],
                volume_serial,
                volume_label,
                protected_prefixes: vec![source_path.to_string_lossy().to_string()],
                is_available: false,
            };
        }
    };

    if is_reparse_point(&metadata) {
        return WalkResult {
            items: vec![WalkItem::Skip(WalkSkip {
                path: source_path.to_string_lossy().to_string(),
                reason: "ссылка или точка повторного анализа".to_string(),
                is_protected: false,
            })],
            volume_serial,
            volume_label,
            protected_prefixes: Vec::new(),
            is_available: true,
        };
    }

    let initial_doc_only = source.mode == SourceMode::DocumentsOnly;

    let effective_exclude_paths = effective_exclude_paths(source_path, &settings.exclude_paths);
    let max_depth = source.max_depth;
    let max_member_size = settings.zip_max_file_size_mb * 1024 * 1024;
    let effective_max_member_size = if max_member_size > 0 {
        max_member_size
    } else {
        DEFAULT_MAX_ARCHIVE_MEMBER_SIZE
    };

    let stack = Arc::new(Mutex::new(vec![DirStackItem {
        path: source_path.clone(),
        rel_path: String::new(),
        depth: 0,
        document_only: initial_doc_only,
    }]));

    let active = Arc::new(AtomicUsize::new(0));
    let all_items = Arc::new(Mutex::new(Vec::new()));
    let all_protected = Arc::new(Mutex::new(Vec::new()));

    let num_threads = if settings.hdd_gentle_mode {
        1
    } else if settings.worker_threads > 0 {
        settings.worker_threads
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(8)
    };

    let run_worker = || {
        let mut local_items = Vec::new();
        let mut local_protected = Vec::new();

        loop {
            let current_dir = {
                let mut guard = stack.lock().unwrap();
                guard.pop()
            };

            let current_dir = match current_dir {
                Some(d) => {
                    active.fetch_add(1, Ordering::SeqCst);
                    d
                }
                None => {
                    if active.load(Ordering::SeqCst) == 0 {
                        break;
                    }
                    std::thread::yield_now();
                    continue;
                }
            };

            let entries = match fs::read_dir(&current_dir.path) {
                Ok(iter) => iter,
                Err(err) => {
                    let dir_str = current_dir.path.to_string_lossy().to_string();
                    local_items.push(WalkItem::Skip(WalkSkip {
                        path: dir_str.clone(),
                        reason: format!("Ошибка чтения папки: {err}"),
                        is_protected: true,
                    }));
                    local_protected.push(dir_str);
                    active.fetch_sub(1, Ordering::SeqCst);
                    continue;
                }
            };

            let mut new_dirs = Vec::new();

            for entry_res in entries {
                let entry = match entry_res {
                    Ok(e) => e,
                    Err(err) => {
                        let dir_str = current_dir.path.to_string_lossy().to_string();
                        local_items.push(WalkItem::Skip(WalkSkip {
                            path: dir_str.clone(),
                            reason: format!("Ошибка чтения записи: {err}"),
                            is_protected: true,
                        }));
                        continue;
                    }
                };

                let file_name_os = entry.file_name();
                let file_name = file_name_os.to_string_lossy();
                let file_path = entry.path();
                let file_path_str = file_path.to_string_lossy().to_string();

                let rel_path = if current_dir.rel_path.is_empty() {
                    file_name.to_string()
                } else {
                    format!("{}\\{}", current_dir.rel_path, file_name)
                };

                // Используем entry.metadata(), которая на Windows использует уже полученные
                // данные WIN32_FIND_DATAW без лишних системных вызовов
                let meta = match entry.metadata() {
                    Ok(m) => m,
                    Err(err) => {
                        local_items.push(WalkItem::Skip(WalkSkip {
                            path: file_path_str,
                            reason: format!("Ошибка stat: {err}"),
                            is_protected: false,
                        }));
                        continue;
                    }
                };

                // 1. Проверка на reparse point / symlink (R1-А-38, 47; R1-Б-1)
                if is_reparse_point(&meta) {
                    local_items.push(WalkItem::Skip(WalkSkip {
                        path: file_path_str,
                        reason: "ссылка или точка повторного анализа".to_string(),
                        is_protected: false,
                    }));
                    continue;
                }

                let is_dir = meta.is_dir();

                if is_dir {
                    // Проверка исключения папки по имени
                    if is_folder_name_excluded(&file_name, &settings.exclude_folder_names) {
                        local_items.push(WalkItem::Skip(WalkSkip {
                            path: file_path_str,
                            reason: "исключено правилом".to_string(),
                            is_protected: false,
                        }));
                        continue;
                    }

                    // Проверка исключения папки по пути
                    if is_path_excluded(&file_path, &effective_exclude_paths, data_dir_ref) {
                        local_items.push(WalkItem::Skip(WalkSkip {
                            path: file_path_str,
                            reason: "исключено правилом".to_string(),
                            is_protected: false,
                        }));
                        continue;
                    }

                    // Проверка соблюдения max_depth
                    let next_depth = current_dir.depth + 1;
                    if max_depth > 0 && next_depth >= max_depth {
                        continue;
                    }

                    // Наследование режима «только документы» (R1-А-48..50, R1-Б-7, 10)
                    let dir_doc_only = current_dir.document_only
                        || crate::rules::is_document_only_folder_name(
                            &file_name,
                            &settings.documents_only_folder_names,
                        );

                    new_dirs.push(DirStackItem {
                        path: file_path,
                        rel_path,
                        depth: next_depth,
                        document_only: dir_doc_only,
                    });
                } else {
                    // Файл
                    // Проверка системных файлов и шаблонов имён (ntuser.dat*, ~$*, etc.)
                    if is_file_excluded(&file_name, &settings.exclude_files) {
                        continue;
                    }

                    // Проверка исключения файла по пути
                    if is_path_excluded(&file_path, &effective_exclude_paths, data_dir_ref) {
                        local_items.push(WalkItem::Skip(WalkSkip {
                            path: file_path_str,
                            reason: "исключено правилом".to_string(),
                            is_protected: false,
                        }));
                        continue;
                    }

                    let ext = file_path
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| format!(".{}", s.to_lowercase()))
                        .unwrap_or_default();

                    // Фильтрация в режиме «только документы» (R1-А-50)
                    if current_dir.document_only && !is_document_only_extension(&ext) {
                        continue;
                    }

                    let (size, modified_ns, created_ns) = get_metadata_times_and_size(&meta);

                    local_items.push(WalkItem::Entry(WalkEntry {
                        path: file_path_str.clone(),
                        rel_path: rel_path.clone(),
                        size,
                        modified_ns,
                        created_ns,
                        is_dir: false,
                        inside_archive: false,
                        container_path: None,
                        document_only: current_dir.document_only,
                    }));
                    count_found(found, 1);

                    // Если это ZIP-архив и включено сканирование ZIP (R1-А-51..55)
                    if ext == ".zip" && settings.zip_scan {
                        if let Some(checker) = is_archive_unchanged
                            && let Some(cached_members) = checker(&file_path_str, size, modified_ns)
                        {
                            count_found(found, cached_members.len());
                            for member in cached_members {
                                local_items.push(WalkItem::Entry(member));
                            }
                            continue;
                        }

                        let before = local_items.len();
                        walk_zip_file(
                            &file_path,
                            &file_path_str,
                            &rel_path,
                            current_dir.document_only,
                            effective_max_member_size,
                            settings.zip_max_file_size_mb,
                            modified_ns,
                            created_ns,
                            &mut local_items,
                        );
                        let added = local_items[before..]
                            .iter()
                            .filter(|item| matches!(item, WalkItem::Entry(_)))
                            .count();
                        count_found(found, added);
                    }
                }
            }

            if !new_dirs.is_empty() {
                let mut guard = stack.lock().unwrap();
                guard.extend(new_dirs);
            }

            active.fetch_sub(1, Ordering::SeqCst);
        }

        let mut g_items = all_items.lock().unwrap();
        g_items.extend(local_items);
        let mut g_prot = all_protected.lock().unwrap();
        g_prot.extend(local_protected);
    };

    if num_threads <= 1 {
        run_worker();
    } else {
        std::thread::scope(|s| {
            for _ in 0..num_threads {
                s.spawn(|| {
                    run_worker();
                });
            }
        });
    }

    let items = Arc::try_unwrap(all_items).unwrap().into_inner().unwrap();
    let protected_prefixes = Arc::try_unwrap(all_protected)
        .unwrap()
        .into_inner()
        .unwrap();

    WalkResult {
        items,
        volume_serial,
        volume_label,
        protected_prefixes,
        is_available: true,
    }
}

/// Увеличивает общий счётчик найденных файлов.
fn count_found(found: Option<&AtomicUsize>, count: usize) {
    if let Some(counter) = found {
        counter.fetch_add(count, Ordering::Relaxed);
    }
}

/// Обход одиночного явно выбранного файла (R1-А-66, R1-Б-11).
fn walk_single_file(
    source: &Source,
    settings: &Settings,
    is_archive_unchanged: Option<&ArchiveUnchangedChecker>,
    found: Option<&AtomicUsize>,
) -> WalkResult {
    let source_path = &source.path;
    let (volume_serial, volume_label) = get_volume_info(source_path);

    let meta = match fs::symlink_metadata(source_path) {
        Ok(m) => m,
        Err(err) => {
            return WalkResult {
                items: vec![WalkItem::Skip(WalkSkip {
                    path: source_path.to_string_lossy().to_string(),
                    reason: format!("Ошибка stat файла: {err}"),
                    is_protected: true,
                })],
                volume_serial,
                volume_label,
                protected_prefixes: vec![source_path.to_string_lossy().to_string()],
                is_available: false,
            };
        }
    };

    let path_str = source_path.to_string_lossy().to_string();
    if is_reparse_point(&meta) {
        return WalkResult {
            items: vec![WalkItem::Skip(WalkSkip {
                path: path_str,
                reason: "ссылка или точка повторного анализа".to_string(),
                is_protected: false,
            })],
            volume_serial,
            volume_label,
            protected_prefixes: Vec::new(),
            is_available: true,
        };
    }

    let file_name = source_path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path_str.clone());

    let (size, modified_ns, created_ns) = get_metadata_times_and_size(&meta);
    let mut items = vec![WalkItem::Entry(WalkEntry {
        path: path_str.clone(),
        rel_path: file_name.clone(),
        size,
        modified_ns,
        created_ns,
        is_dir: false,
        inside_archive: false,
        container_path: None,
        document_only: false,
    })];
    count_found(found, 1);

    let ext = source_path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{}", s.to_lowercase()))
        .unwrap_or_default();

    if ext == ".zip" && settings.zip_scan {
        if let Some(cached_members) =
            is_archive_unchanged.and_then(|checker| checker(&path_str, size, modified_ns))
        {
            count_found(found, cached_members.len());
            for member in cached_members {
                items.push(WalkItem::Entry(member));
            }
            return WalkResult {
                items,
                volume_serial,
                volume_label,
                protected_prefixes: Vec::new(),
                is_available: true,
            };
        }

        let max_member_size = settings.zip_max_file_size_mb * 1024 * 1024;
        let effective_max_member_size = if max_member_size > 0 {
            max_member_size
        } else {
            DEFAULT_MAX_ARCHIVE_MEMBER_SIZE
        };

        let before = items.len();
        walk_zip_file(
            source_path,
            &path_str,
            &file_name,
            false,
            effective_max_member_size,
            settings.zip_max_file_size_mb,
            modified_ns,
            created_ns,
            &mut items,
        );
        let added = items[before..]
            .iter()
            .filter(|item| matches!(item, WalkItem::Entry(_)))
            .count();
        count_found(found, added);
    }

    WalkResult {
        items,
        volume_serial,
        volume_label,
        protected_prefixes: Vec::new(),
        is_available: true,
    }
}

/// Читает содержимое ZIP-архива и формирует записи о внутренних файлах (R1-А-51..55).
#[allow(clippy::too_many_arguments)]
fn walk_zip_file(
    archive_path: &Path,
    archive_path_str: &str,
    archive_rel_path: &str,
    document_only: bool,
    max_member_size: u64,
    max_member_mb: u64,
    archive_modified_ns: i64,
    archive_created_ns: i64,
    items: &mut Vec<WalkItem>,
) {
    let file = match fs::File::open(archive_path) {
        Ok(f) => f,
        Err(err) => {
            items.push(WalkItem::Skip(WalkSkip {
                path: archive_path_str.to_string(),
                reason: format!("ZIP не прочитан: {err}"),
                is_protected: false,
            }));
            return;
        }
    };

    let mut archive = match zip::ZipArchive::new(file) {
        Ok(a) => a,
        Err(err) => {
            items.push(WalkItem::Skip(WalkSkip {
                path: archive_path_str.to_string(),
                reason: format!("ZIP не прочитан: {err}"),
                is_protected: false,
            }));
            return;
        }
    };

    for i in 0..archive.len() {
        let zip_file = match archive.by_index(i) {
            Ok(f) => f,
            Err(err) => {
                items.push(WalkItem::Skip(WalkSkip {
                    path: archive_path_str.to_string(),
                    reason: format!("ZIP не прочитан: {err}"),
                    is_protected: false,
                }));
                continue;
            }
        };

        if zip_file.is_dir() {
            continue;
        }

        let raw_name = zip_file.name_raw();
        // Имя члена: то же правило, что при поиске члена при чтении (normalize_zip_member_name)
        let inner_path = normalize_zip_member_name(raw_name);
        let inner_name = inner_path
            .rsplit('\\')
            .next()
            .unwrap_or(&inner_path)
            .to_string();

        let ext = Path::new(&inner_name)
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| format!(".{}", s.to_lowercase()))
            .unwrap_or_default();

        // Фильтрация расширений (R1-А-52, R1-Б-8, 9)
        if !is_archive_content_extension(&ext, document_only) {
            continue;
        }

        let member_path = format!("{archive_path_str}{ARCHIVE_SEPARATOR}{inner_path}");
        let member_rel_path = format!("{archive_rel_path}{ARCHIVE_SEPARATOR}{inner_path}");
        let member_size = zip_file.size();

        // Превышение лимита размера внутреннего файла (R1-А-53)
        if member_size > max_member_size {
            items.push(WalkItem::Skip(WalkSkip {
                path: member_path,
                reason: format!("внутренний файл больше {max_member_mb} МБ"),
                is_protected: false,
            }));
            continue;
        }

        // Внутренний файл принимается
        items.push(WalkItem::Entry(WalkEntry {
            path: member_path,
            rel_path: member_rel_path,
            size: member_size,
            modified_ns: archive_modified_ns,
            created_ns: archive_created_ns,
            is_dir: false,
            inside_archive: true,
            container_path: Some(archive_path_str.to_string()),
            document_only,
        }));
    }
}

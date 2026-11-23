use std::fs;
use std::path::Path;

use rusqlite::Connection;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
use windows::core::PCWSTR;

/// Режим смены папки данных
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocateMode {
    /// Перенести существующие данные: SQLite базу через backup API, и индекс tantivy
    Move,
    /// Начать с чистой пустой папки данных
    StartEmpty,
}

#[derive(Debug, thiserror::Error)]
pub enum RelocateError {
    #[error(
        "Недостаточно свободного места на диске: требуется {required} байт, доступно {available} байт"
    )]
    NotEnoughSpace { required: u64, available: u64 },
    #[error("Ошибка базы данных SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Ошибка ввода-вывода: {0}")]
    Io(#[from] std::io::Error),
    #[error("Проверка целостности базы данных не пройдена: {0}")]
    IntegrityCheckFailed(String),
    #[error("Ошибка открытия поискового индекса tantivy: {0}")]
    Index(String),
    #[error("Исходная папка данных не найдена: {0}")]
    OldDirNotFound(String),
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Определяет объем свободного места на диске для указанного пути в байтах.
pub fn get_disk_free_space(path: &Path) -> u64 {
    let mut check_path = path.to_path_buf();
    while !check_path.exists() {
        if let Some(parent) = check_path.parent() {
            check_path = parent.to_path_buf();
        } else {
            break;
        }
    }

    let mut path_str = check_path.to_string_lossy().to_string();
    if !path_str.ends_with('\\') && !path_str.ends_with('/') {
        path_str.push('\\');
    }
    let wide = to_wide_null(&path_str);

    let mut free_bytes_available: u64 = 0;
    let mut total_number_of_bytes: u64 = 0;
    let mut total_number_of_free_bytes: u64 = 0;

    let ret = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide.as_ptr()),
            Some(&mut free_bytes_available),
            Some(&mut total_number_of_bytes),
            Some(&mut total_number_of_free_bytes),
        )
    };

    if ret.is_ok() {
        free_bytes_available
    } else {
        u64::MAX
    }
}

/// Подсчитывает суммарный размер файлов в каталоге.
pub fn calculate_dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += calculate_dir_size(&p);
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Рекурсивно копирует каталог.
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest_child = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &dest_child)?;
        } else {
            fs::copy(entry.path(), &dest_child)?;
        }
    }
    Ok(())
}

/// Выполняет перенос папки данных программы (раздел 2.2 ТЗ).
///
/// При `Move`:
/// 1. Проверяет свободное место на целевом диске;
/// 2. Копирует `catalog.sqlite3` через SQLite backup API (онлайн-бэкап без копирования открытого файла);
/// 3. Копирует `index\`;
/// 4. Проверяет целостность в новой папке (`PRAGMA integrity_check`, открытие tantivy индекса);
/// 5. Только после успешной проверки удаляет старое содержимое;
/// 6. При любой ошибке старая папка остается нетронутой, скопированное в новую удаляется.
///
/// При `StartEmpty`:
/// Просто создает новую пустую папку.
pub fn relocate_data(
    old_dir: &Path,
    new_dir: &Path,
    mode: RelocateMode,
) -> Result<(), RelocateError> {
    match mode {
        RelocateMode::StartEmpty => {
            fs::create_dir_all(new_dir)?;
            Ok(())
        }
        RelocateMode::Move => {
            if !old_dir.exists() {
                return Err(RelocateError::OldDirNotFound(
                    old_dir.to_string_lossy().to_string(),
                ));
            }

            // 1. Проверка свободного места
            let required_bytes = calculate_dir_size(old_dir) + 10 * 1024 * 1024; // + 10 МБ запаса
            let available_bytes = get_disk_free_space(new_dir);

            if available_bytes < required_bytes {
                return Err(RelocateError::NotEnoughSpace {
                    required: required_bytes,
                    available: available_bytes,
                });
            }

            // Создаем новую директорию
            fs::create_dir_all(new_dir)?;

            // Выполняем перенос с гарантией отката (rollback при ошибке)
            let move_result = (|| -> Result<(), RelocateError> {
                let old_catalog = old_dir.join("catalog.sqlite3");
                let new_catalog = new_dir.join("catalog.sqlite3");

                // 2. Копирование catalog.sqlite3 через SQLite backup API
                if old_catalog.exists() {
                    let src_conn = Connection::open(&old_catalog)?;
                    src_conn.busy_timeout(std::time::Duration::from_secs(30))?;
                    src_conn.backup(rusqlite::MAIN_DB, &new_catalog, None)?;
                }

                // 3. Копирование index\
                let old_index = old_dir.join("index");
                let new_index = new_dir.join("index");
                if old_index.exists() {
                    copy_dir_all(&old_index, &new_index)?;
                }

                // 4. Проверка целостности в новой папке
                if new_catalog.exists() {
                    let conn = Connection::open(&new_catalog)?;
                    conn.busy_timeout(std::time::Duration::from_secs(30))?;
                    let check_res: String =
                        conn.query_row("PRAGMA integrity_check;", [], |r| r.get(0))?;
                    if check_res != "ok" {
                        drop(conn);
                        return Err(RelocateError::IntegrityCheckFailed(check_res));
                    }
                    drop(conn);
                }

                if new_index.exists() && fs::read_dir(&new_index)?.next().is_some() {
                    crate::index::open_or_create(&new_index, None)
                        .map_err(|e| RelocateError::Index(e.to_string()))?;
                }

                Ok(())
            })();

            if let Err(err) = move_result {
                // При ошибке: старое остаётся, новое удаляется, ошибка возвращается
                let _ = fs::remove_dir_all(new_dir);
                return Err(err);
            }

            // 5. И только потом удаляем старое содержимое (в старой становится пусто)
            if let Ok(entries) = fs::read_dir(old_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        let _ = fs::remove_dir_all(&p);
                    } else {
                        let _ = fs::remove_file(&p);
                    }
                }
            }

            Ok(())
        }
    }
}

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::settings::Settings;

/// Пути к файлам конфигурации и данным программы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Полный путь к файлу настроек `settings.toml`.
    pub settings_file: PathBuf,
    /// Папка данных (содержит базу данных, индекс и журналы).
    pub data_dir: PathBuf,
    /// Путь к базе метаданных `catalog.sqlite3`.
    pub catalog_db: PathBuf,
    /// Папка полнотекстового индекса tantivy.
    pub index_dir: PathBuf,
    /// Папка файлов журнала.
    pub logs_dir: PathBuf,
}

impl AppPaths {
    /// Создает все необходимые директории на диске, если они еще не существуют.
    pub fn ensure_dirs(&self) -> io::Result<()> {
        if let Some(parent) = self.settings_file.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir_all(&self.data_dir)?;
        fs::create_dir_all(&self.index_dir)?;
        fs::create_dir_all(&self.logs_dir)?;
        Ok(())
    }
}

/// Проверяет, включен ли переносной режим (наличие `settings.toml` рядом с исполняемым файлом).
pub fn is_portable_mode(exe_dir: &Path) -> bool {
    exe_dir.join("settings.toml").is_file()
}

/// Возвращает путь к системной папке настроек roaming (%APPDATA%) через SHGetKnownFolderPath / dirs.
pub fn roaming_appdata_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    })
}

/// Возвращает путь к системной папке локальных данных (%LOCALAPPDATA%) через SHGetKnownFolderPath / dirs.
pub fn local_appdata_dir() -> PathBuf {
    dirs::data_local_dir().unwrap_or_else(|| {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    })
}

/// Определяет путь к файлу настроек `settings.toml` (переносной или системный).
pub fn default_settings_path(exe_dir: &Path) -> PathBuf {
    if is_portable_mode(exe_dir) {
        exe_dir.join("settings.toml")
    } else {
        roaming_appdata_dir()
            .join("ПоисКовичеК")
            .join("settings.toml")
    }
}

/// Разрешает все пути программы на основе расположения exe и настроек (раздел 2.2 ТЗ).
///
/// - Переносной режим: если рядом с exe лежит `settings.toml`, настройки берутся оттуда,
///   а папка данных по умолчанию — `<папка exe>\data`.
/// - Обычный режим: настройки лежат в `%APPDATA%\ПоисКовичеК\settings.toml`,
///   папка данных по умолчанию — `%LOCALAPPDATA%\ПоисКовичеК`.
/// - Если в настройках явно задан `data_dir`, он переопределяет папку данных в обоих режимах.
pub fn resolve(exe_dir: &Path, settings: &Settings) -> AppPaths {
    let portable = is_portable_mode(exe_dir);

    let settings_file = if portable {
        exe_dir.join("settings.toml")
    } else {
        roaming_appdata_dir()
            .join("ПоисКовичеК")
            .join("settings.toml")
    };

    let data_dir = if !settings.data_dir.as_os_str().is_empty() {
        settings.data_dir.clone()
    } else if portable {
        exe_dir.join("data")
    } else {
        local_appdata_dir().join("ПоисКовичеК")
    };

    let catalog_db = data_dir.join("catalog.sqlite3");
    let index_dir = data_dir.join("index");
    let logs_dir = data_dir.join("logs");

    AppPaths {
        settings_file,
        data_dir,
        catalog_db,
        index_dir,
        logs_dir,
    }
}

/// Возвращает путь к морфологическому словарю словоформ `morph.fst`.
///
/// Путь ТОЛЬКО рядом с исполняемым файлом exe, без поисков в других каталогах.
pub fn morph_path(exe_dir: &Path) -> PathBuf {
    exe_dir.join("morph.fst")
}

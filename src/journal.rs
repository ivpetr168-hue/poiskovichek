use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetLocalTime;

fn current_local_time() -> SYSTEMTIME {
    unsafe { GetLocalTime() }
}

/// Уровни важности сообщений журнала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Debug => write!(f, "DEBUG"),
            Self::Info => write!(f, "INFO"),
            Self::Warn => write!(f, "WARN"),
            Self::Error => write!(f, "ERROR"),
        }
    }
}

/// Вычисляет непрерывное число дней для даты (алгоритм Говарда Хиннанта для григорианского календаря).
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let mut y = year;
    let mut m = month as i32;
    if m <= 2 {
        y -= 1;
        m += 9;
    } else {
        m -= 3;
    }
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * m + 2) / 5 + day as i32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era as i64 * 146097 + doe as i64
}

/// Парсит имя файла вида `ГГГГ-ММ-ДД.log` в (год, месяц, день).
fn parse_log_filename(name: &str) -> Option<(i32, u32, u32)> {
    let base = name.strip_suffix(".log")?;
    let parts: Vec<&str> = base.split('-').collect();
    if parts.len() != 3 {
        return None;
    }
    let y: i32 = parts[0].parse().ok()?;
    let m: u32 = parts[1].parse().ok()?;
    let d: u32 = parts[2].parse().ok()?;
    if (1..=12).contains(&m) && (1..=31).contains(&d) {
        Some((y, m, d))
    } else {
        None
    }
}

/// Экземпляр журнала.
#[derive(Debug)]
pub struct Journal {
    logs_dir: PathBuf,
}

impl Journal {
    /// Открывает журнал в указанной папке и выполняет очистку старых записей.
    pub fn open(logs_dir: &Path, keep_days: u32) -> io::Result<Self> {
        fs::create_dir_all(logs_dir)?;
        let journal = Self {
            logs_dir: logs_dir.to_path_buf(),
        };
        journal.clean_old_logs(keep_days)?;
        Ok(journal)
    }

    /// Записывает строку в журнал: `время уровень модуль текст`.
    pub fn write_entry(&self, level: Level, module: &str, text: &str) -> io::Result<()> {
        let st = current_local_time();
        let log_file_name = format!("{:04}-{:02}-{:02}.log", st.wYear, st.wMonth, st.wDay);
        let log_path = self.logs_dir.join(log_file_name);

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)?;

        let timestamp = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond, st.wMilliseconds
        );

        writeln!(file, "{timestamp} {level} {module} {text}")?;
        file.flush()?;
        Ok(())
    }

    /// Удаляет файлы журналов старше `keep_days` дней.
    pub fn clean_old_logs(&self, keep_days: u32) -> io::Result<usize> {
        if !self.logs_dir.exists() {
            return Ok(0);
        }

        let st = current_local_time();
        let today_days = days_from_civil(st.wYear as i32, st.wMonth as u32, st.wDay as u32);
        let mut deleted_count = 0;

        for entry in fs::read_dir(&self.logs_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };

            let should_delete = if let Some((y, m, d)) = parse_log_filename(file_name) {
                let file_days = days_from_civil(y, m, d);
                (today_days - file_days) > keep_days as i64
            } else if file_name.ends_with(".log") {
                if let Ok(meta) = entry.metadata() {
                    if let Ok(modified) = meta.modified() {
                        if let Ok(age) = std::time::SystemTime::now().duration_since(modified) {
                            age.as_secs() > (keep_days as u64 * 86_400)
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            };

            if should_delete && fs::remove_file(&path).is_ok() {
                deleted_count += 1;
            }
        }

        Ok(deleted_count)
    }
}

// Глобальное состояние журнала для удобного вызова из любых модулей
static GLOBAL_JOURNAL: Mutex<Option<Journal>> = Mutex::new(None);

/// Инициализирует глобальный журнал в указанной папке.
pub fn init(logs_dir: &Path, keep_days: u32) -> io::Result<()> {
    let journal = Journal::open(logs_dir, keep_days)?;
    let mut lock = GLOBAL_JOURNAL.lock().unwrap();
    *lock = Some(journal);
    Ok(())
}

/// Записывает сообщение в глобальный журнал.
pub fn log(level: Level, module: &str, text: &str) {
    let lock = GLOBAL_JOURNAL.lock().unwrap();
    if let Some(ref journal) = *lock {
        let _ = journal.write_entry(level, module, text);
    }
}

/// Записывает информационное сообщение в журнал.
pub fn info(module: &str, text: &str) {
    log(Level::Info, module, text);
}

/// Записывает предупреждение в журнал.
pub fn warn(module: &str, text: &str) {
    log(Level::Warn, module, text);
}

/// Записывает ошибку в журнал.
pub fn error(module: &str, text: &str) {
    log(Level::Error, module, text);
}

/// Удаляет журналы старше `keep_days` из папки логов.
pub fn clean_old_logs(logs_dir: &Path, keep_days: u32) -> io::Result<usize> {
    let journal = Journal {
        logs_dir: logs_dir.to_path_buf(),
    };
    journal.clean_old_logs(keep_days)
}

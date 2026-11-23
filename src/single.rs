use sha2::{Digest, Sha256};
use std::path::Path;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, LPARAM, WPARAM,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_BROADCAST, PostMessageW, RegisterWindowMessageW,
};
use windows::core::PCWSTR;

/// Название зарегистрированного сообщения Windows для подъема окна первого экземпляра
pub const SHOW_MESSAGE_NAME: &str = "ПоисКовичеК-показать";

/// Ошибка попытки запуска: программа с данной папкой данных уже запущена
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AlreadyRunning;

impl std::fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Программа ПоисКовичеК уже запущена для этой папки данных"
        )
    }
}

impl std::error::Error for AlreadyRunning {}

/// Защитный объект владения межпроцессным мьютексом единственной копии.
/// При сбросе (Drop) освобождает и закрывает дескриптор мьютекса.
pub struct SingleInstanceGuard {
    handle: HANDLE,
}

// Дескриптор мьютекса Windows может безопасно передаваться между потоками
unsafe impl Send for SingleInstanceGuard {}
unsafe impl Sync for SingleInstanceGuard {}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.handle.is_invalid() && !self.handle.0.is_null() {
                let _ = ReleaseMutex(self.handle);
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// Вычисляет имя межпроцессного именованного мьютекса: `Local\ПоисКовичеК-<хэш папки данных>`.
pub fn mutex_name_for_data_dir(data_dir: &Path) -> String {
    let normalized = std::fs::canonicalize(data_dir)
        .unwrap_or_else(|_| data_dir.to_path_buf())
        .to_string_lossy()
        .to_lowercase();
    let hash = hex::encode(Sha256::digest(normalized.as_bytes()));
    format!("Local\\ПоисКовичеК-{}", &hash[..16])
}

/// Пытается захватить межпроцессный именованный мьютекс для указанной папки данных.
///
/// Возвращает `Ok(SingleInstanceGuard)` при успешном первом запуске.
/// Если мьютекс уже занят другим процессом, возвращает `Err(AlreadyRunning)`.
pub fn acquire(data_dir: &Path) -> Result<SingleInstanceGuard, AlreadyRunning> {
    let mutex_name = mutex_name_for_data_dir(data_dir);
    let wide_name: Vec<u16> = mutex_name
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let handle = unsafe {
        CreateMutexW(
            None,
            true, // захватываем владение мьютексом
            PCWSTR(wide_name.as_ptr()),
        )
    };

    let handle = match handle {
        Ok(h) => h,
        Err(_) => return Err(AlreadyRunning),
    };

    if handle.is_invalid() || handle.0.is_null() {
        return Err(AlreadyRunning);
    }

    let last_err = unsafe { GetLastError() };
    if last_err == ERROR_ALREADY_EXISTS {
        // Мьютекс уже существовал — другой процесс уже запущен
        unsafe {
            let _ = CloseHandle(handle);
        }
        return Err(AlreadyRunning);
    }

    Ok(SingleInstanceGuard { handle })
}

/// Имя мьютекса «программа запущена» для установщика (Inno Setup, `CheckForMutexes`).
pub const INSTALLER_MUTEX_NAME: &str = "ПоисКовичеК-запущен";

/// Создаёт мьютекс «программа запущена», который видит установщик. Дескриптор живёт
/// до конца процесса: система закроет его сама, когда программа завершится.
pub fn hold_installer_mutex() {
    let wide: Vec<u16> = INSTALLER_MUTEX_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // Дескриптор намеренно не закрывается
    let _ = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) };
}

/// Возвращает системный идентификатор зарегистрированного оконного сообщения Windows.
pub fn show_message_id() -> u32 {
    let wide: Vec<u16> = SHOW_MESSAGE_NAME
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe { RegisterWindowMessageW(PCWSTR(wide.as_ptr())) }
}

/// Отправляет широковещательное оконное сообщение первому экземпляру программы для подъема его окна.
pub fn notify_existing() -> bool {
    let msg_id = show_message_id();
    if msg_id == 0 {
        return false;
    }

    unsafe { PostMessageW(Some(HWND_BROADCAST), msg_id, WPARAM(0), LPARAM(0)).is_ok() }
}

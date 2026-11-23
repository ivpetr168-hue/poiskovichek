use std::path::Path;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPEN_CREATE_OPTIONS, REG_SZ, REG_VALUE_TYPE,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW,
};
use windows::core::PCWSTR;

pub const DEFAULT_RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
pub const AUTOSTART_VALUE_NAME: &str = "ПоисКовичеК";

#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    #[error("Ошибка реестра Windows (код {0})")]
    Windows(u32),
}

fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Устанавливает или удаляет автозапуск программы в стандартном разделе Run реестра Windows.
pub fn set_autostart(enabled: bool, exe_path: &Path) -> Result<(), AutostartError> {
    set_autostart_custom(
        HKEY_CURRENT_USER,
        DEFAULT_RUN_KEY,
        AUTOSTART_VALUE_NAME,
        enabled,
        exe_path,
    )
}

/// Проверяет, включен ли автозапуск программы в стандартном разделе Run реестра Windows.
pub fn is_autostart_enabled() -> Result<bool, AutostartError> {
    is_autostart_enabled_custom(HKEY_CURRENT_USER, DEFAULT_RUN_KEY, AUTOSTART_VALUE_NAME)
}

/// Настраивает автозапуск в указанном разделе реестра (для возможности тестирования без изменения реального Run).
pub fn set_autostart_custom(
    root: HKEY,
    sub_key: &str,
    value_name: &str,
    enabled: bool,
    exe_path: &Path,
) -> Result<(), AutostartError> {
    let sub_key_wide = to_wide_null(sub_key);
    let val_name_wide = to_wide_null(value_name);

    if enabled {
        let mut hkey = HKEY::default();
        let ret = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(sub_key_wide.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPEN_CREATE_OPTIONS(0),
                KEY_WRITE,
                None,
                &mut hkey,
                None,
            )
        };
        if ret != ERROR_SUCCESS {
            return Err(AutostartError::Windows(ret.0));
        }

        let cmd_str = format!("\"{}\" --background", exe_path.display());
        let wide_data = to_wide_null(&cmd_str);
        let byte_slice = unsafe {
            std::slice::from_raw_parts(
                wide_data.as_ptr() as *const u8,
                wide_data.len() * std::mem::size_of::<u16>(),
            )
        };

        let set_ret = unsafe {
            RegSetValueExW(
                hkey,
                PCWSTR(val_name_wide.as_ptr()),
                None,
                REG_SZ,
                Some(byte_slice),
            )
        };
        unsafe {
            let _ = RegCloseKey(hkey);
        }

        if set_ret != ERROR_SUCCESS {
            return Err(AutostartError::Windows(set_ret.0));
        }
    } else {
        let mut hkey = HKEY::default();
        let ret = unsafe {
            RegOpenKeyExW(
                root,
                PCWSTR(sub_key_wide.as_ptr()),
                None,
                KEY_WRITE,
                &mut hkey,
            )
        };
        if ret == ERROR_FILE_NOT_FOUND {
            // Раздел не существует, значит и автозапуск уже выключен
            return Ok(());
        }
        if ret != ERROR_SUCCESS {
            return Err(AutostartError::Windows(ret.0));
        }

        let del_ret = unsafe { RegDeleteValueW(hkey, PCWSTR(val_name_wide.as_ptr())) };
        unsafe {
            let _ = RegCloseKey(hkey);
        }

        if del_ret != ERROR_SUCCESS && del_ret != ERROR_FILE_NOT_FOUND {
            return Err(AutostartError::Windows(del_ret.0));
        }
    }

    Ok(())
}

/// Проверяет наличие значения автозапуска в указанном разделе реестра.
pub fn is_autostart_enabled_custom(
    root: HKEY,
    sub_key: &str,
    value_name: &str,
) -> Result<bool, AutostartError> {
    let sub_key_wide = to_wide_null(sub_key);
    let val_name_wide = to_wide_null(value_name);

    let mut hkey = HKEY::default();
    let ret = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(sub_key_wide.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        )
    };
    if ret == ERROR_FILE_NOT_FOUND {
        return Ok(false);
    }
    if ret != ERROR_SUCCESS {
        return Err(AutostartError::Windows(ret.0));
    }

    let query_ret =
        unsafe { RegQueryValueExW(hkey, PCWSTR(val_name_wide.as_ptr()), None, None, None, None) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }

    if query_ret == ERROR_SUCCESS {
        Ok(true)
    } else if query_ret == ERROR_FILE_NOT_FOUND {
        Ok(false)
    } else {
        Err(AutostartError::Windows(query_ret.0))
    }
}

/// Считывает строковое значение команды автозапуска из реестра (для проверок в тестах).
pub fn get_autostart_command_custom(
    root: HKEY,
    sub_key: &str,
    value_name: &str,
) -> Result<Option<String>, AutostartError> {
    let sub_key_wide = to_wide_null(sub_key);
    let val_name_wide = to_wide_null(value_name);

    let mut hkey = HKEY::default();
    let ret = unsafe {
        RegOpenKeyExW(
            root,
            PCWSTR(sub_key_wide.as_ptr()),
            None,
            KEY_READ,
            &mut hkey,
        )
    };
    if ret == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if ret != ERROR_SUCCESS {
        return Err(AutostartError::Windows(ret.0));
    }

    let mut val_type = REG_VALUE_TYPE::default();
    let mut cb_data: u32 = 0;

    let size_ret = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(val_name_wide.as_ptr()),
            None,
            Some(&mut val_type),
            None,
            Some(&mut cb_data),
        )
    };
    if size_ret == ERROR_FILE_NOT_FOUND {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        return Ok(None);
    }
    if size_ret != ERROR_SUCCESS {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        return Err(AutostartError::Windows(size_ret.0));
    }

    let mut buffer = vec![0u8; cb_data as usize];
    let data_ret = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(val_name_wide.as_ptr()),
            None,
            Some(&mut val_type),
            Some(buffer.as_mut_ptr()),
            Some(&mut cb_data),
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }

    if data_ret != ERROR_SUCCESS {
        return Err(AutostartError::Windows(data_ret.0));
    }

    // Декодируем UTF-16
    let u16_slice: &[u16] =
        unsafe { std::slice::from_raw_parts(buffer.as_ptr() as *const u16, buffer.len() / 2) };
    let s = String::from_utf16_lossy(u16_slice);
    let clean = s.trim_end_matches('\0').to_string();
    Ok(Some(clean))
}

/// Удаляет подраздел реестра (используется для очистки в тестах).
pub fn delete_key(root: HKEY, sub_key: &str) -> Result<(), AutostartError> {
    let sub_key_wide = to_wide_null(sub_key);
    let ret: WIN32_ERROR = unsafe { RegDeleteKeyW(root, PCWSTR(sub_key_wide.as_ptr())) };
    if ret == ERROR_SUCCESS || ret == ERROR_FILE_NOT_FOUND {
        Ok(())
    } else {
        Err(AutostartError::Windows(ret.0))
    }
}

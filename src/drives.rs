//! Список дисков компьютера для мастера первого запуска.
//!
//! Модуль опрашивает логические диски Windows: букву, метку тома, файловую систему,
//! вид диска, объём и свободное место. Неготовый диск (пустой привод, отключённая сеть)
//! возвращается с `ready: false` и без системных окон «вставьте диск».

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::Diagnostics::Debug::{SEM_FAILCRITICALERRORS, SetErrorMode};
use windows::Win32::System::WindowsProgramming::{
    DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
};
use windows::core::PCWSTR;

/// Сколько ждём ответа от сетевого диска, прежде чем считать его неготовым.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Длина буфера для метки тома и имени файловой системы: MAX_PATH + 1.
const NAME_BUFFER_LEN: usize = 261;

/// Вид диска по классификации Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    CdRom,
    RamDisk,
    Cloud,
    Unknown,
}

/// Сведения об одном диске для мастера первого запуска.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveInfo {
    pub letter: char,
    pub root: PathBuf,
    pub label: String,
    pub kind: DriveKind,
    pub filesystem: String,
    pub serial: Option<u32>,
    pub total_bytes: Option<u64>,
    pub free_bytes: Option<u64>,
    pub ready: bool,
}

/// Данные тома, полученные от Windows.
#[derive(Debug, Clone, Default)]
struct VolumeData {
    label: String,
    filesystem: String,
    serial: Option<u32>,
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
    ready: bool,
}

/// Собирает список логических дисков компьютера.
///
/// На время опроса отключает системные окна об ошибках критического устройства,
/// затем возвращает прежний режим обработки ошибок.
pub fn list_drives() -> Vec<DriveInfo> {
    let previous_mode = unsafe { SetErrorMode(SEM_FAILCRITICALERRORS) };
    let drives = collect_drives();
    unsafe { SetErrorMode(previous_mode) };
    drives
}

/// Строка для мастера первого запуска: буква, метка, вид, файловая система, объёмы.
pub fn describe(drive: &DriveInfo) -> String {
    let mut parts = vec![format!("{}:", drive.letter)];
    let label = drive.label.trim();
    if !label.is_empty() {
        parts.push(format!("«{label}»"));
    }
    parts.push(kind_title(drive.kind).to_string());
    let filesystem = drive.filesystem.trim();
    if !filesystem.is_empty() {
        parts.push(filesystem.to_string());
    }
    if drive.ready {
        if let (Some(total), Some(free)) = (drive.total_bytes, drive.free_bytes) {
            parts.push(format!(
                "{} ГБ, свободно {} ГБ",
                format_gib(total),
                format_gib(free)
            ));
        }
    } else {
        parts.push("(нет носителя)".to_string());
    }
    parts.join("  ")
}

/// Что отметить в мастере по умолчанию: готовые локальные диски.
pub fn default_selection(drives: &[DriveInfo]) -> Vec<char> {
    drives
        .iter()
        .filter(|drive| drive.ready && drive.kind == DriveKind::Fixed)
        .map(|drive| drive.letter)
        .collect()
}

fn collect_drives() -> Vec<DriveInfo> {
    let mask = unsafe { GetLogicalDrives() };
    let mut drives = Vec::new();
    for (index, letter_byte) in (b'A'..=b'Z').enumerate() {
        if mask & (1u32 << index) == 0 {
            continue;
        }
        let letter = letter_byte as char;
        let root = PathBuf::from(format!("{letter}:\\"));
        let mut kind = drive_kind(&root);
        let volume = if kind == DriveKind::Network {
            probe_network(&root)
        } else {
            Some(probe_volume(&root))
        }
        .unwrap_or_default();
        let root_attrs = get_root_attributes(&root);
        if is_cloud_drive(&volume.label, &volume.filesystem, root_attrs) {
            kind = DriveKind::Cloud;
        }
        drives.push(DriveInfo {
            letter,
            root,
            label: volume.label,
            kind,
            filesystem: volume.filesystem,
            serial: volume.serial,
            total_bytes: volume.total_bytes,
            free_bytes: volume.free_bytes,
            ready: volume.ready,
        });
    }
    drives
}

/// Определяет вид диска по корневому пути.
fn drive_kind(root: &Path) -> DriveKind {
    let root_wide = to_wide(root);
    let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };
    kind_from_type(drive_type)
}

/// Переводит значение `GetDriveTypeW` в вид диска.
fn kind_from_type(drive_type: u32) -> DriveKind {
    match drive_type {
        DRIVE_FIXED => DriveKind::Fixed,
        DRIVE_REMOVABLE => DriveKind::Removable,
        DRIVE_REMOTE => DriveKind::Network,
        DRIVE_CDROM => DriveKind::CdRom,
        DRIVE_RAMDISK => DriveKind::RamDisk,
        _ => DriveKind::Unknown,
    }
}

/// Опрашивает сетевой диск в отдельном потоке с ожиданием не больше `PROBE_TIMEOUT`.
fn probe_network(root: &Path) -> Option<VolumeData> {
    let root = root.to_path_buf();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(probe_volume(&root));
    });
    receiver.recv_timeout(PROBE_TIMEOUT).ok()
}

/// Читает метку, файловую систему, серийный номер и объёмы тома.
fn probe_volume(root: &Path) -> VolumeData {
    let root_wide = to_wide(root);
    let root_ptr = PCWSTR(root_wide.as_ptr());
    let mut label_buffer = [0u16; NAME_BUFFER_LEN];
    let mut filesystem_buffer = [0u16; NAME_BUFFER_LEN];
    let mut serial: u32 = 0;

    let info = unsafe {
        GetVolumeInformationW(
            root_ptr,
            Some(&mut label_buffer),
            Some(&mut serial),
            None,
            None,
            Some(&mut filesystem_buffer),
        )
    };
    if info.is_err() {
        return VolumeData::default();
    }

    let mut available: u64 = 0;
    let mut total: u64 = 0;
    let sizes_ok = unsafe {
        GetDiskFreeSpaceExW(root_ptr, Some(&mut available), Some(&mut total), None).is_ok()
    };

    VolumeData {
        label: decode_wide(&label_buffer),
        filesystem: decode_wide(&filesystem_buffer),
        serial: Some(serial),
        total_bytes: sizes_ok.then_some(total),
        free_bytes: sizes_ok.then_some(available),
        ready: true,
    }
}

/// Переводит строку в UTF-16 с завершающим нулём.
fn to_wide(path: &Path) -> Vec<u16> {
    path.to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect()
}

/// Читает строку из буфера UTF-16 до завершающего нуля.
fn decode_wide(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// Русское название вида диска для мастера.
fn kind_title(kind: DriveKind) -> &'static str {
    match kind {
        DriveKind::Fixed => "локальный диск",
        DriveKind::Removable => "съёмный диск",
        DriveKind::Network => "сетевой диск",
        DriveKind::CdRom => "оптический привод",
        DriveKind::RamDisk => "RAM-диск",
        DriveKind::Cloud => "облачный диск",
        DriveKind::Unknown => "неизвестный",
    }
}

/// Проверяет, является ли диск облачным по метке, файловой системе или атрибутам корня.
pub fn is_cloud_drive(label: &str, filesystem: &str, root_attributes: u32) -> bool {
    let recall_mask = 0x0040_0000 | 0x0004_0000;
    if (root_attributes & recall_mask) != 0 {
        return true;
    }
    let label_lower = label.to_lowercase();
    let fs_lower = filesystem.to_lowercase();
    let patterns = [
        "google drive",
        "googledrive",
        "google",
        "onedrive",
        "dropbox",
        "яндекс",
        "yandex",
    ];
    for pat in patterns {
        if label_lower.contains(pat) || fs_lower.contains(pat) {
            return true;
        }
    }
    false
}

/// Читает файловые атрибуты корневой папки диска.
pub fn get_root_attributes(root: &Path) -> u32 {
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
        let root_wide = to_wide(root);
        unsafe {
            let attrs = GetFileAttributesW(PCWSTR(root_wide.as_ptr()));
            if attrs == INVALID_FILE_ATTRIBUTES {
                0
            } else {
                attrs
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        0
    }
}

/// Проверяет, расположен ли путь на облачном диске.
pub fn is_cloud_path(path: &Path) -> bool {
    let root = match path.components().next() {
        Some(std::path::Component::Prefix(prefix)) => {
            let p = prefix.as_os_str().to_string_lossy();
            PathBuf::from(format!("{p}\\"))
        }
        _ => return false,
    };
    let volume = probe_volume(&root);
    let root_attrs = get_root_attributes(&root);
    is_cloud_drive(&volume.label, &volume.filesystem, root_attrs)
}

/// Объём в гигабайтах с одним знаком после запятой.
fn format_gib(bytes: u64) -> String {
    let gib = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    format!("{gib:.1}").replace('.', ",")
}

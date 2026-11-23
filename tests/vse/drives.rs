use std::path::PathBuf;

use poiskovichek::drives::{
    DriveInfo, DriveKind, default_selection, describe, is_cloud_drive, list_drives,
};

const GIB: u64 = 1024 * 1024 * 1024;

fn fake(
    letter: char,
    label: &str,
    kind: DriveKind,
    filesystem: &str,
    total_bytes: Option<u64>,
    free_bytes: Option<u64>,
    ready: bool,
) -> DriveInfo {
    DriveInfo {
        letter,
        root: PathBuf::from(format!("{letter}:\\")),
        label: label.to_string(),
        kind,
        filesystem: filesystem.to_string(),
        serial: Some(0x1234_ABCD),
        total_bytes,
        free_bytes,
        ready,
    }
}

#[test]
fn system_drive_is_ready_fixed() {
    let drives = list_drives();
    let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let letter = system.chars().next().expect("SystemDrive пуст");
    let found = drives
        .iter()
        .find(|drive| drive.letter.eq_ignore_ascii_case(&letter))
        .expect("системный диск отсутствует в списке");
    assert_eq!(found.kind, DriveKind::Fixed);
    assert!(found.ready);
    let total = found.total_bytes.expect("нет общего объёма");
    let free = found.free_bytes.expect("нет свободного объёма");
    assert!(total > free);
}

#[test]
fn describe_ready_local_drive() {
    let drive = fake(
        'C',
        "Windows",
        DriveKind::Fixed,
        "NTFS",
        Some(256 * GIB),
        Some(55 * GIB),
        true,
    );
    assert_eq!(
        describe(&drive),
        "C:  «Windows»  локальный диск  NTFS  256,0 ГБ, свободно 55,0 ГБ"
    );
}

#[test]
fn describe_ready_drive_without_label() {
    let drive = fake(
        'D',
        "",
        DriveKind::Fixed,
        "NTFS",
        Some(1_000_000_000_000),
        Some(100_000_000_000),
        true,
    );
    assert_eq!(
        describe(&drive),
        "D:  локальный диск  NTFS  931,3 ГБ, свободно 93,1 ГБ"
    );
}

#[test]
fn describe_not_ready_removable() {
    let drive = fake('E', "", DriveKind::Removable, "", None, None, false);
    assert_eq!(describe(&drive), "E:  съёмный диск  (нет носителя)");
}

#[test]
fn describe_network_drive() {
    let drive = fake('Z', "", DriveKind::Network, "", None, None, true);
    assert_eq!(describe(&drive), "Z:  сетевой диск");
}

#[test]
fn default_selection_takes_ready_fixed_only() {
    let drives = vec![
        fake(
            'C',
            "Windows",
            DriveKind::Fixed,
            "NTFS",
            Some(200 * GIB),
            Some(50 * GIB),
            true,
        ),
        fake(
            'D',
            "Данные",
            DriveKind::Fixed,
            "NTFS",
            Some(500 * GIB),
            Some(300 * GIB),
            true,
        ),
        fake('E', "", DriveKind::Fixed, "", None, None, false),
        fake(
            'F',
            "Флешка",
            DriveKind::Removable,
            "FAT32",
            Some(32 * GIB),
            Some(16 * GIB),
            true,
        ),
        fake('G', "", DriveKind::CdRom, "UDF", None, None, false),
        fake(
            'R',
            "RAM",
            DriveKind::RamDisk,
            "NTFS",
            Some(GIB),
            Some(GIB),
            true,
        ),
        fake('Z', "", DriveKind::Network, "", None, None, true),
        fake(
            'G',
            "user@gmail.com - Google Drive",
            DriveKind::Cloud,
            "FAT32",
            Some(100 * GIB),
            Some(50 * GIB),
            true,
        ),
    ];
    assert_eq!(default_selection(&drives), vec!['C', 'D']);
}

#[test]
fn test_is_cloud_drive_fake_values() {
    // 1. Google Drive в различных написаниях
    assert!(is_cloud_drive("user@gmail.com - Google Drive", "FAT32", 0));
    assert!(is_cloud_drive("GoogleDrive", "FAT32", 0));
    assert!(is_cloud_drive("", "Google Drive", 0));

    // 2. OneDrive
    assert!(is_cloud_drive("OneDrive - Personal", "NTFS", 0));
    assert!(is_cloud_drive("Диск", "OneDrive", 0));

    // 3. Dropbox
    assert!(is_cloud_drive("Dropbox", "NTFS", 0));

    // 4. Яндекс / Yandex
    assert!(is_cloud_drive("Яндекс.Диск", "NTFS", 0));
    assert!(is_cloud_drive("Yandex.Disk", "NTFS", 0));

    // 5. Атрибуты повторного вызова (recall on data access / recall on open)
    assert!(is_cloud_drive("Локальный том", "NTFS", 0x0040_0000));
    assert!(is_cloud_drive("Локальный том", "NTFS", 0x0004_0000));

    // 6. Обычные локальные диски
    assert!(!is_cloud_drive("Windows", "NTFS", 0));
    assert!(!is_cloud_drive("Data", "FAT32", 0x0000_0020));
    assert!(!is_cloud_drive("", "NTFS", 0));
}

#[test]
fn describe_cloud_drive() {
    let drive = fake(
        'G',
        "Google Drive",
        DriveKind::Cloud,
        "FAT32",
        Some(100 * GIB),
        Some(60 * GIB),
        true,
    );
    assert_eq!(
        describe(&drive),
        "G:  «Google Drive»  облачный диск  FAT32  100,0 ГБ, свободно 60,0 ГБ"
    );
}

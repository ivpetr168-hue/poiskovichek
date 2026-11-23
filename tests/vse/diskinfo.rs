use std::path::Path;

use poiskovichek::{DiskClass, describe_disk, disk_info_for_letter, disk_info_for_path};

#[test]
fn test_diskinfo_class_methods() {
    let classes = [
        (DiskClass::Nvme, "NVMe SSD", "Nvme"),
        (DiskClass::SataSsd, "SATA SSD", "SataSsd"),
        (DiskClass::Hdd, "жёсткий диск (HDD)", "Hdd"),
        (DiskClass::UsbSsd, "USB SSD", "UsbSsd"),
        (DiskClass::UsbHdd, "USB HDD", "UsbHdd"),
        (DiskClass::Network, "сетевой диск", "Network"),
        (DiskClass::Unknown, "неизвестно", "Unknown"),
    ];

    for (class, title, name) in classes {
        assert_eq!(class.title(), title);
        assert_eq!(class.name(), name);
        assert_eq!(DiskClass::from_name(name), Some(class));
    }

    assert_eq!(DiskClass::from_name("Invalid"), None);
}

#[test]
fn test_diskinfo_system_drive() {
    let sys_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
    let letter = sys_drive.chars().next().unwrap_or('C');
    let info = disk_info_for_letter(letter);

    assert_ne!(info.class, DiskClass::Unknown);
    assert!(info.physical_disk_id.is_some());
    assert!(info.sector_size.is_some_and(|s| s > 0));
    assert!(info.volume_serial.is_some());

    let text = describe_disk(&info);
    assert!(
        text.contains("физический диск"),
        "describe must contain 'физический диск', got: {text}"
    );

    if letter.eq_ignore_ascii_case(&'C') {
        let cargo_path = Path::new(r"C:\projects\poiskovichek\Cargo.toml");
        if cargo_path.exists() {
            let path_info = disk_info_for_path(cargo_path);
            assert_eq!(path_info.physical_disk_id, info.physical_disk_id);
            assert_eq!(path_info.class, info.class);
        }
    }
}

#[test]
fn test_diskinfo_machine_disks_describe() {
    // Проверка диска C:
    if Path::new(r"C:\").exists() {
        let c_info = disk_info_for_letter('C');
        println!("C: -> {}", describe_disk(&c_info));
        println!("C: raw -> {:?}", c_info);
        if c_info.physical_disk_id.is_some() {
            assert_ne!(c_info.class, DiskClass::Unknown);
            assert_eq!(c_info.class, DiskClass::Nvme);
        }
    }

    // Проверка диска D:
    if Path::new(r"D:\").exists() {
        let d_info = disk_info_for_letter('D');
        println!("D: -> {}", describe_disk(&d_info));
        println!("D: raw -> {:?}", d_info);
        if d_info.physical_disk_id.is_some() {
            assert_ne!(d_info.class, DiskClass::Unknown);
            assert_eq!(d_info.class, DiskClass::Hdd);
        }
    }

    // Проверка диска P: (может отсутствовать)
    if Path::new(r"P:\").exists() {
        let p_info = disk_info_for_letter('P');
        println!("P: -> {}", describe_disk(&p_info));
        println!("P: raw -> {:?}", p_info);
        if p_info.physical_disk_id.is_some() {
            assert_ne!(p_info.class, DiskClass::Unknown);
        }
    }
}

#[test]
fn test_diskinfo_invalid_paths() {
    let relative = Path::new("relative/path/to/file.txt");
    let rel_info = disk_info_for_path(relative);
    assert_eq!(rel_info.class, DiskClass::Unknown);
    assert_eq!(
        rel_info.reason.as_deref(),
        Some("путь не на локальном томе")
    );

    let unc = Path::new(r"\\server\share\file.txt");
    let unc_info = disk_info_for_path(unc);
    assert_eq!(unc_info.class, DiskClass::Unknown);
    assert_eq!(
        unc_info.reason.as_deref(),
        Some("путь не на локальном томе")
    );

    let bad_letter = disk_info_for_letter('1');
    assert_eq!(bad_letter.class, DiskClass::Unknown);
    assert_eq!(
        bad_letter.reason.as_deref(),
        Some("некорректная буква диска")
    );
}

//! Определение физического накопителя, его класса, шины и характеристик.
//!
//! Модуль опрашивает логический том Windows через системные вызовы без прав
//! администратора: открывает том с доступом 0, запрашивает экстенты тома
//! и свойства накопителя (шину и штраф за поиск).

use std::path::{Path, PathBuf};

/// Класс физического накопителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskClass {
    Nvme,
    SataSsd,
    Hdd,
    UsbSsd,
    UsbHdd,
    Network,
    Unknown,
}

impl DiskClass {
    /// Название класса накопителя по-русски.
    pub fn title(self) -> &'static str {
        match self {
            Self::Nvme => "NVMe SSD",
            Self::SataSsd => "SATA SSD",
            Self::Hdd => "жёсткий диск (HDD)",
            Self::UsbSsd => "USB SSD",
            Self::UsbHdd => "USB HDD",
            Self::Network => "сетевой диск",
            Self::Unknown => "неизвестно",
        }
    }

    /// Восстановление класса из строкового идентификатора (для чтения JSON моделью).
    pub fn from_name(name: &str) -> Option<DiskClass> {
        match name {
            "Nvme" => Some(Self::Nvme),
            "SataSsd" => Some(Self::SataSsd),
            "Hdd" => Some(Self::Hdd),
            "UsbSsd" => Some(Self::UsbSsd),
            "UsbHdd" => Some(Self::UsbHdd),
            "Network" => Some(Self::Network),
            "Unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// Строковый идентификатор класса латиницей (ровно как вариант перечисления).
    pub fn name(self) -> &'static str {
        match self {
            Self::Nvme => "Nvme",
            Self::SataSsd => "SataSsd",
            Self::Hdd => "Hdd",
            Self::UsbSsd => "UsbSsd",
            Self::UsbHdd => "UsbHdd",
            Self::Network => "Network",
            Self::Unknown => "Unknown",
        }
    }
}

/// Сведения о физическом накопителе тома.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskInfo {
    /// Путь, по которому спрашивали.
    pub path: PathBuf,
    /// Буква тома, если путь на локальном томе.
    pub letter: Option<char>,
    /// Номер физического диска (`DISK_EXTENT.DiskNumber`).
    pub physical_disk_id: Option<u32>,
    /// Сколько физических дисков у тома.
    pub extent_count: u32,
    /// Класс накопителя.
    pub class: DiskClass,
    /// Шина накопителя ("NVMe", "SATA", "USB", "RAID", "" если неизвестно).
    pub bus: String,
    /// Штраф за поиск (true = HDD).
    pub seek_penalty: Option<bool>,
    /// Размер сектора в байтах.
    pub sector_size: Option<u32>,
    /// Серийный номер тома.
    pub volume_serial: Option<u32>,
    /// Причина по-русски, почему класс Unknown или что не удалось.
    pub reason: Option<String>,
}

/// Для любого пути узнаёт физический диск и характеристики накопителя.
pub fn disk_info_for_path(path: &Path) -> DiskInfo {
    #[cfg(windows)]
    {
        imp::disk_info_for_path(path)
    }
    #[cfg(not(windows))]
    {
        DiskInfo {
            path: path.to_path_buf(),
            letter: None,
            physical_disk_id: None,
            extent_count: 0,
            class: DiskClass::Unknown,
            bus: String::new(),
            seek_penalty: None,
            sector_size: None,
            volume_serial: None,
            reason: Some("поддерживается только в Windows".to_string()),
        }
    }
}

/// Для буквы диска ('C' или 'c') узнаёт физический диск и характеристики накопителя.
pub fn disk_info_for_letter(letter: char) -> DiskInfo {
    #[cfg(windows)]
    {
        imp::disk_info_for_letter(letter)
    }
    #[cfg(not(windows))]
    {
        let _ = letter;
        DiskInfo {
            path: PathBuf::new(),
            letter: None,
            physical_disk_id: None,
            extent_count: 0,
            class: DiskClass::Unknown,
            bus: String::new(),
            seek_penalty: None,
            sector_size: None,
            volume_serial: None,
            reason: Some("поддерживается только в Windows".to_string()),
        }
    }
}

/// Одна строка по-русски для консоли и отчёта.
pub fn describe(info: &DiskInfo) -> String {
    let mut parts = Vec::new();
    if let Some(letter) = info.letter {
        parts.push(format!("{letter}:"));
    } else if !info.path.as_os_str().is_empty() {
        parts.push(info.path.to_string_lossy().to_string());
    }

    parts.push(info.class.title().to_string());

    if let Some(disk_id) = info.physical_disk_id {
        parts.push(format!("физический диск {disk_id}"));
    }

    if !info.bus.is_empty() {
        parts.push(format!("шина {}", info.bus));
    }

    if let Some(sector) = info.sector_size {
        parts.push(format!("сектор {sector}"));
    }

    if let Some(serial) = info.volume_serial {
        parts.push(format!("серийный номер {serial}"));
    }

    if let (Some(reason), DiskClass::Unknown) = (&info.reason, info.class) {
        parts.push(format!("(причина: {reason})"));
    }

    parts.join("  ")
}

#[cfg(windows)]
mod imp {
    use std::path::{Component, Path, PathBuf, Prefix};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        BusTypeAta, BusTypeAtapi, BusTypeNvme, BusTypeRAID, BusTypeSas, BusTypeSata, BusTypeScsi,
        BusTypeSpaces, BusTypeUsb, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ,
        FILE_SHARE_WRITE, GetDiskFreeSpaceW, GetDriveTypeW, GetVolumeInformationW,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, OPEN_EXISTING, STORAGE_BUS_TYPE,
    };
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{
        DEVICE_SEEK_PENALTY_DESCRIPTOR, DISK_EXTENT, IOCTL_STORAGE_QUERY_PROPERTY,
        PropertyStandardQuery, STORAGE_DEVICE_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
        StorageDeviceProperty, StorageDeviceSeekPenaltyProperty, VOLUME_DISK_EXTENTS,
    };
    use windows::Win32::System::WindowsProgramming::DRIVE_REMOTE;
    use windows::core::PCWSTR;

    use super::{DiskClass, DiskInfo};

    struct HandleGuard(HANDLE);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            if !self.0.is_invalid() && !self.0.0.is_null() {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    #[repr(C, align(8))]
    struct AlignedBuf<const N: usize>([u8; N]);

    pub fn disk_info_for_path(path: &Path) -> DiskInfo {
        let letter = match path.components().next() {
            Some(Component::Prefix(prefix)) => match prefix.kind() {
                Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                    let c = (d as char).to_ascii_uppercase();
                    if c.is_ascii_alphabetic() {
                        Some(c)
                    } else {
                        None
                    }
                }
                _ => None,
            },
            _ => None,
        };

        match letter {
            Some(c) => query_disk_info(path.to_path_buf(), Some(c), c),
            None => DiskInfo {
                path: path.to_path_buf(),
                letter: None,
                physical_disk_id: None,
                extent_count: 0,
                class: DiskClass::Unknown,
                bus: String::new(),
                seek_penalty: None,
                sector_size: None,
                volume_serial: None,
                reason: Some("путь не на локальном томе".to_string()),
            },
        }
    }

    pub fn disk_info_for_letter(letter: char) -> DiskInfo {
        if !letter.is_ascii_alphabetic() {
            return DiskInfo {
                path: PathBuf::from(letter.to_string()),
                letter: None,
                physical_disk_id: None,
                extent_count: 0,
                class: DiskClass::Unknown,
                bus: String::new(),
                seek_penalty: None,
                sector_size: None,
                volume_serial: None,
                reason: Some("некорректная буква диска".to_string()),
            };
        }

        let upper = letter.to_ascii_uppercase();
        let path = PathBuf::from(format!("{upper}:\\"));
        query_disk_info(path, Some(upper), upper)
    }

    #[allow(non_upper_case_globals)]
    fn bus_type_to_str(bus_type: STORAGE_BUS_TYPE) -> &'static str {
        match bus_type {
            BusTypeNvme => "NVMe",
            BusTypeSata | BusTypeAta | BusTypeAtapi => "SATA",
            BusTypeUsb => "USB",
            BusTypeRAID | BusTypeSpaces => "RAID",
            BusTypeScsi | BusTypeSas => "SCSI",
            _ => "",
        }
    }

    fn query_disk_info(path: PathBuf, letter: Option<char>, drive_letter: char) -> DiskInfo {
        let root_str = format!("{drive_letter}:\\");
        let root_wide: Vec<u16> = root_str.encode_utf16().chain(std::iter::once(0)).collect();

        // 1. Проверка сетевого диска
        let drive_type = unsafe { GetDriveTypeW(PCWSTR(root_wide.as_ptr())) };
        if drive_type == DRIVE_REMOTE {
            return DiskInfo {
                path,
                letter,
                physical_disk_id: None,
                extent_count: 0,
                class: DiskClass::Network,
                bus: String::new(),
                seek_penalty: None,
                sector_size: None,
                volume_serial: None,
                reason: None,
            };
        }

        // Размер сектора
        let mut bytes_per_sector = 0u32;
        let sector_ok = unsafe {
            GetDiskFreeSpaceW(
                PCWSTR(root_wide.as_ptr()),
                None,
                Some(&mut bytes_per_sector),
                None,
                None,
            )
        };
        let sector_size = if sector_ok.is_ok() && bytes_per_sector > 0 {
            Some(bytes_per_sector)
        } else {
            None
        };

        // Серийный номер тома
        let mut serial = 0u32;
        let vol_ok = unsafe {
            GetVolumeInformationW(
                PCWSTR(root_wide.as_ptr()),
                None,
                Some(&mut serial),
                None,
                None,
                None,
            )
        };
        let volume_serial = if vol_ok.is_ok() { Some(serial) } else { None };

        // Открытие тома с доступом 0
        let volume_str = format!("\\\\.\\{drive_letter}:");
        let volume_wide: Vec<u16> = volume_str
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let handle_res = unsafe {
            CreateFileW(
                PCWSTR(volume_wide.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        };

        let handle = match handle_res {
            Ok(h) if !h.is_invalid() && !h.0.is_null() => h,
            Ok(h) => {
                let _ = unsafe { CloseHandle(h) };
                return DiskInfo {
                    path,
                    letter,
                    physical_disk_id: None,
                    extent_count: 0,
                    class: DiskClass::Unknown,
                    bus: String::new(),
                    seek_penalty: None,
                    sector_size,
                    volume_serial,
                    reason: Some("не удалось открыть том (неверный дескриптор)".to_string()),
                };
            }
            Err(err) => {
                return DiskInfo {
                    path,
                    letter,
                    physical_disk_id: None,
                    extent_count: 0,
                    class: DiskClass::Unknown,
                    bus: String::new(),
                    seek_penalty: None,
                    sector_size,
                    volume_serial,
                    reason: Some(format!("не удалось открыть том: {err}")),
                };
            }
        };

        let _guard = HandleGuard(handle);

        // Получение номера физического диска и числа экстентов
        let total_size =
            std::mem::size_of::<VOLUME_DISK_EXTENTS>() + 8 * std::mem::size_of::<DISK_EXTENT>();
        let num_u64 = total_size.div_ceil(8);
        let mut extents_buf = vec![0u64; num_u64];
        let mut bytes_returned = 0u32;
        let extents_ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                None,
                0,
                Some(extents_buf.as_mut_ptr() as *mut _),
                (num_u64 * 8) as u32,
                Some(&mut bytes_returned),
                None,
            )
        };

        let mut physical_disk_id = None;
        let mut extent_count = 0u32;
        if extents_ok.is_ok() && bytes_returned >= std::mem::size_of::<VOLUME_DISK_EXTENTS>() as u32
        {
            let vde = unsafe { &*(extents_buf.as_ptr() as *const VOLUME_DISK_EXTENTS) };
            extent_count = vde.NumberOfDiskExtents;
            if extent_count > 0 {
                physical_disk_id = Some(vde.Extents[0].DiskNumber);
            }
        }

        // Запрос свойств накопителя (дескриптор устройства -> шина)
        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut device_buf = AlignedBuf::<1024>([0u8; 1024]);
        let mut dev_bytes_returned = 0u32;
        let dev_ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&query as *const _ as *const _),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(device_buf.0.as_mut_ptr() as *mut _),
                device_buf.0.len() as u32,
                Some(&mut dev_bytes_returned),
                None,
            )
        };

        let mut bus = String::new();
        if dev_ok.is_ok() && dev_bytes_returned >= 32 {
            let desc = unsafe { &*(device_buf.0.as_ptr() as *const STORAGE_DEVICE_DESCRIPTOR) };
            bus = bus_type_to_str(desc.BusType).to_string();
        }

        // Запрос штрафа за поиск (seek penalty)
        let penalty_query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageDeviceSeekPenaltyProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let mut penalty_desc = std::mem::MaybeUninit::<DEVICE_SEEK_PENALTY_DESCRIPTOR>::zeroed();
        let mut penalty_bytes_returned = 0u32;
        let penalty_ok = unsafe {
            DeviceIoControl(
                handle,
                IOCTL_STORAGE_QUERY_PROPERTY,
                Some(&penalty_query as *const _ as *const _),
                std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                Some(penalty_desc.as_mut_ptr() as *mut _),
                std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
                Some(&mut penalty_bytes_returned),
                None,
            )
        };

        let seek_penalty = if penalty_ok.is_ok()
            && penalty_bytes_returned
                >= std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32
        {
            let desc = unsafe { penalty_desc.assume_init() };
            Some(desc.IncursSeekPenalty)
        } else {
            None
        };

        // Классификация накопителя
        let mut reason = None;
        let class = match bus.as_str() {
            "USB" => match seek_penalty {
                Some(true) => DiskClass::UsbHdd,
                Some(false) => DiskClass::UsbSsd,
                None => {
                    reason = Some("штраф за поиск не сообщён, считаю SSD".to_string());
                    DiskClass::UsbSsd
                }
            },
            "NVMe" => DiskClass::Nvme,
            "SATA" | "RAID" => match seek_penalty {
                Some(true) => DiskClass::Hdd,
                _ => DiskClass::SataSsd,
            },
            _ => match seek_penalty {
                Some(true) => DiskClass::Hdd,
                Some(false) => DiskClass::SataSsd,
                None => {
                    reason = Some("не удалось определить тип накопителя".to_string());
                    DiskClass::Unknown
                }
            },
        };

        DiskInfo {
            path,
            letter,
            physical_disk_id,
            extent_count,
            class,
            bus,
            seek_penalty,
            sector_size,
            volume_serial,
            reason,
        }
    }
}

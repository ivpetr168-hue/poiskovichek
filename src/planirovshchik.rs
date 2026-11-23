//! Планировщик чтения по физическим дискам.
//!
//! Конвейер индексирования раньше читал файлы всех дисков одними и теми же потоками:
//! медленный жёсткий диск задерживал их, а быстрые диски простаивали. Здесь собрано то,
//! что нужно для раздельного чтения: сколько потоков чтения полезно каждому классу диска,
//! как по пути файла узнать физический диск (две буквы на одном диске — одна очередь)
//! и как прочитать начало файла в память.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Component, Path, Prefix};
use std::sync::Mutex;

use crate::diskinfo::{DiskClass, DiskInfo, disk_info_for_letter};

/// Наибольший файл, который читатель целиком берёт в память (16 МБ).
pub const PREDEL_CHTENIYA: u64 = 16 * 1024 * 1024;

/// Число потоков чтения для класса диска.
///
/// Основание — замер дисков от 26.10.2026 (ноутбук: C: NVMe, D: HDD, P: USB SSD, 4 ядра),
/// суммарная скорость чтения, МБ/с, при k = 1/2/4/8 потоках:
/// - NVMe: файлы 200 КБ — 375/803/1427/1830; 20 МБ — 1737/2017/2093/2217; 4 КБ — 12/26/46/62.
///   Растёт почти до k = 4 на любых файлах, поэтому 4 потока.
/// - SATA SSD: замера нет, как у NVMe — 4 потока (параллельные запросы ему тоже полезны).
/// - HDD: 200 КБ — 82/86/89/97; 20 МБ — 72/72/68/49 (больше потоков вредит); 4 КБ — меньше
///   1 МБ/с. Выше двух потоков выигрыша нет, на крупных файлах вред, поэтому 2.
/// - USB SSD: 200 КБ — 179/336/381/390; 20 МБ — 324/394/394/394; 4 КБ — 14/30/55/91.
///   Выход на предел уже на 2 потоках на средних и крупных файлах, поэтому 2.
/// - USB HDD: замера нет, самый осторожный выбор — 1 поток.
/// - Облачный и сетевой диск: 1 поток (задержка и загрузка по сети, лишние запросы вредят).
/// - Неизвестный: 2 потока.
///
/// Щадящий режим для HDD (`settings.hdd_gentle_mode`, «не более одного потока на физический том»):
/// у `Hdd` и `UsbHdd` ровно 1 поток чтения.
///
/// Разные физические диски читаются одновременно и почти складываются
/// (коэффициент 0,92–1,07 при одном потоке на диск), поэтому у каждого диска свои потоки.
pub fn potokov_chteniya(klass: DiskClass, oblachnyy: bool, shchadyashchiy: bool) -> usize {
    if oblachnyy {
        return 1;
    }
    match klass {
        DiskClass::Nvme => 4,
        DiskClass::SataSsd => 4,
        DiskClass::UsbSsd => 2,
        DiskClass::Hdd if shchadyashchiy => 1,
        DiskClass::Hdd => 2,
        DiskClass::UsbHdd => 1,
        DiskClass::Network => 1,
        DiskClass::Unknown => 2,
    }
}

/// Ключ физического диска: у файлов с одним ключом одна очередь чтения.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KlyuchDiska {
    /// Номер физического диска Windows (общий для всех букв на нём).
    Fizicheskiy(u32),
    /// Диск, номер которого не узнали, или облачный/сетевой диск: отдельная очередь на букву.
    Bukva(char),
    /// Сетевой путь без буквы (`\\сервер\ресурс`).
    Set(String),
    /// Всё прочее (относительные и нестандартные пути): одна общая очередь.
    Neizvestno,
}

/// Что известно о диске файла для планировщика.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvedeniyaODiske {
    pub klyuch: KlyuchDiska,
    pub klass: DiskClass,
    pub oblachnyy: bool,
    /// Сколько потоков чтения у этого диска.
    pub potokov: usize,
}

/// Собирает сведения планировщика из сведений о диске.
/// Чистая функция: опрос системы выполняет вызывающий.
pub fn svedeniya_iz_info(
    info: &DiskInfo,
    oblachnyy: bool,
    shchadyashchiy: bool,
) -> SvedeniyaODiske {
    let klyuch = match (info.letter, info.physical_disk_id) {
        (Some(_), Some(id)) if !oblachnyy && info.class != DiskClass::Network => {
            KlyuchDiska::Fizicheskiy(id)
        }
        (Some(letter), _) => KlyuchDiska::Bukva(letter.to_ascii_uppercase()),
        (None, _) => KlyuchDiska::Neizvestno,
    };
    SvedeniyaODiske {
        klyuch,
        klass: info.class,
        oblachnyy,
        potokov: potokov_chteniya(info.class, oblachnyy, shchadyashchiy),
    }
}

/// Буква тома, с которой начинается путь (`C:\…`, `\\?\C:\…`).
pub fn bukva_puti(put: &Path) -> Option<char> {
    match put.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => {
                let c = (d as char).to_ascii_uppercase();
                c.is_ascii_alphabetic().then_some(c)
            }
            _ => None,
        },
        _ => None,
    }
}

/// Сетевой путь без буквы: `\\сервер\ресурс` в нижнем регистре.
fn set_puti(put: &Path) -> Option<String> {
    match put.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::UNC(..) | Prefix::VerbatimUNC(..) => {
                Some(prefix.as_os_str().to_string_lossy().to_lowercase())
            }
            _ => None,
        },
        _ => None,
    }
}

/// Опрос системы по букве тома (подменяется в тестах).
type OprosBukvy = Box<dyn Fn(char) -> SvedeniyaODiske + Send + Sync>;

/// Определяет диск файла по пути; ответы запоминает по букве тома.
pub struct OpredelitelDiskov {
    kesh: Mutex<HashMap<char, SvedeniyaODiske>>,
    opros: OprosBukvy,
    /// Щадящий режим для HDD: не больше одного потока чтения на диск.
    shchadyashchiy: bool,
}

impl OpredelitelDiskov {
    /// Определитель, опрашивающий Windows (`diskinfo` и облачный признак тома).
    pub fn new(shchadyashchiy: bool) -> Self {
        Self::s_oprosom(
            Box::new(move |letter| opros_sistemy(letter, shchadyashchiy)),
            shchadyashchiy,
        )
    }

    /// Определитель с заданным опросом буквы (для тестов).
    pub fn s_oprosom(opros: OprosBukvy, shchadyashchiy: bool) -> Self {
        Self {
            kesh: Mutex::new(HashMap::new()),
            opros,
            shchadyashchiy,
        }
    }

    /// Сведения о диске, на котором лежит файл.
    pub fn dlya_puti(&self, put: &Path) -> SvedeniyaODiske {
        if let Some(letter) = bukva_puti(put) {
            if let Ok(kesh) = self.kesh.lock()
                && let Some(known) = kesh.get(&letter)
            {
                return known.clone();
            }
            // Опрос диска не держит замок: у каждой буквы он редок (один раз)
            let svedeniya = (self.opros)(letter);
            if let Ok(mut kesh) = self.kesh.lock() {
                kesh.insert(letter, svedeniya.clone());
            }
            return svedeniya;
        }
        if let Some(set) = set_puti(put) {
            return SvedeniyaODiske {
                klyuch: KlyuchDiska::Set(set),
                klass: DiskClass::Network,
                oblachnyy: false,
                potokov: potokov_chteniya(DiskClass::Network, false, self.shchadyashchiy),
            };
        }
        SvedeniyaODiske {
            klyuch: KlyuchDiska::Neizvestno,
            klass: DiskClass::Unknown,
            oblachnyy: false,
            potokov: potokov_chteniya(DiskClass::Unknown, false, self.shchadyashchiy),
        }
    }
}

/// Настоящий опрос Windows по букве тома.
fn opros_sistemy(letter: char, shchadyashchiy: bool) -> SvedeniyaODiske {
    let info = disk_info_for_letter(letter);
    let root = std::path::PathBuf::from(format!("{letter}:\\"));
    let oblachnyy = info.class != DiskClass::Network && crate::drives::is_cloud_path(&root);
    svedeniya_iz_info(&info, oblachnyy, shchadyashchiy)
}

/// Читает первые `skolko` байт файла (или весь файл, если он короче).
pub fn prochitat_nachalo(put: &Path, skolko: u64) -> io::Result<Vec<u8>> {
    let file = File::open(put)?;
    let mut buf = Vec::with_capacity(skolko.min(PREDEL_CHTENIYA) as usize);
    file.take(skolko).read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn info(letter: char, disk: Option<u32>, klass: DiskClass) -> DiskInfo {
        DiskInfo {
            path: PathBuf::from(format!("{letter}:\\")),
            letter: Some(letter),
            physical_disk_id: disk,
            extent_count: u32::from(disk.is_some()),
            class: klass,
            bus: String::new(),
            seek_penalty: None,
            sector_size: None,
            volume_serial: None,
            reason: None,
        }
    }

    #[test]
    fn tablica_potokov_po_klassam() {
        assert_eq!(potokov_chteniya(DiskClass::Nvme, false, false), 4);
        assert_eq!(potokov_chteniya(DiskClass::SataSsd, false, false), 4);
        assert_eq!(potokov_chteniya(DiskClass::UsbSsd, false, false), 2);
        assert_eq!(potokov_chteniya(DiskClass::Hdd, false, false), 2);
        assert_eq!(potokov_chteniya(DiskClass::UsbHdd, false, false), 1);
        assert_eq!(potokov_chteniya(DiskClass::Network, false, false), 1);
        assert_eq!(potokov_chteniya(DiskClass::Unknown, false, false), 2);
    }

    #[test]
    fn shchadyashchiy_rezhim_hdd_i_usb_hdd_odin_potok() {
        // №223: при включённом режиме Hdd и UsbHdd читаются одним потоком
        assert_eq!(potokov_chteniya(DiskClass::Hdd, false, true), 1);
        assert_eq!(potokov_chteniya(DiskClass::UsbHdd, false, true), 1);
        // остальные классы режим не затрагивает
        assert_eq!(potokov_chteniya(DiskClass::Nvme, false, true), 4);
        assert_eq!(potokov_chteniya(DiskClass::SataSsd, false, true), 4);
        assert_eq!(potokov_chteniya(DiskClass::UsbSsd, false, true), 2);
        assert_eq!(potokov_chteniya(DiskClass::Unknown, false, true), 2);
        // и в сведениях о диске, и в определителе по пути
        let d = svedeniya_iz_info(&info('D', Some(1), DiskClass::Hdd), false, true);
        assert_eq!(d.potokov, 1);
        let opredelitel = OpredelitelDiskov::s_oprosom(
            Box::new(|letter| {
                svedeniya_iz_info(&info(letter, Some(1), DiskClass::Hdd), false, true)
            }),
            true,
        );
        assert_eq!(opredelitel.dlya_puti(Path::new(r"D:\a.txt")).potokov, 1);
    }

    #[test]
    fn oblachnyy_disk_odin_potok_pri_lyubom_klasse() {
        for klass in [
            DiskClass::Nvme,
            DiskClass::SataSsd,
            DiskClass::Hdd,
            DiskClass::UsbSsd,
            DiskClass::Unknown,
        ] {
            assert_eq!(potokov_chteniya(klass, true, false), 1, "{klass:?}");
        }
    }

    #[test]
    fn dve_bukvy_na_odnom_diske_odin_klyuch() {
        let c = svedeniya_iz_info(&info('C', Some(0), DiskClass::Nvme), false, false);
        let e = svedeniya_iz_info(&info('E', Some(0), DiskClass::Nvme), false, false);
        let d = svedeniya_iz_info(&info('D', Some(1), DiskClass::Hdd), false, false);
        assert_eq!(c.klyuch, KlyuchDiska::Fizicheskiy(0));
        assert_eq!(c.klyuch, e.klyuch);
        assert_ne!(c.klyuch, d.klyuch);
        assert_eq!(c.potokov, 4);
        assert_eq!(d.potokov, 2);
    }

    #[test]
    fn nomer_diska_neizvesten_klyuch_po_bukve() {
        let s = svedeniya_iz_info(&info('x', None, DiskClass::Unknown), false, false);
        assert_eq!(s.klyuch, KlyuchDiska::Bukva('X'));
        assert_eq!(s.potokov, 2);
    }

    #[test]
    fn setevoy_i_oblachnyy_disk_ne_slivayutsya_s_fizicheskim() {
        // облачный диск на той же «физике» не делит очередь с обычным
        let oblako = svedeniya_iz_info(&info('G', Some(0), DiskClass::Nvme), true, false);
        assert_eq!(oblako.klyuch, KlyuchDiska::Bukva('G'));
        assert_eq!(oblako.potokov, 1);
        let set = svedeniya_iz_info(&info('Z', None, DiskClass::Network), false, false);
        assert_eq!(set.klyuch, KlyuchDiska::Bukva('Z'));
        assert_eq!(set.potokov, 1);
    }

    #[test]
    fn bukva_puti_raznye_zapisi() {
        assert_eq!(bukva_puti(Path::new(r"c:\a\b.txt")), Some('C'));
        assert_eq!(bukva_puti(Path::new(r"\\?\D:\a\b.txt")), Some('D'));
        assert_eq!(bukva_puti(Path::new(r"\\server\share\a.txt")), None);
        assert_eq!(bukva_puti(Path::new(r"a\b.txt")), None);
    }

    #[test]
    fn opredelitel_kesh_po_bukve_i_set_bez_bukvy() {
        let vyzovy = Arc::new(AtomicUsize::new(0));
        let schetchik = Arc::clone(&vyzovy);
        let opredelitel = OpredelitelDiskov::s_oprosom(
            Box::new(move |letter| {
                schetchik.fetch_add(1, Ordering::SeqCst);
                // C и E — один физический диск, D — другой
                let disk = if letter == 'D' { 1 } else { 0 };
                svedeniya_iz_info(&info(letter, Some(disk), DiskClass::Hdd), false, false)
            }),
            false,
        );

        let a = opredelitel.dlya_puti(Path::new(r"C:\x\1.txt"));
        let b = opredelitel.dlya_puti(Path::new(r"c:\y\2.txt"));
        let e = opredelitel.dlya_puti(Path::new(r"E:\z.txt"));
        let d = opredelitel.dlya_puti(Path::new(r"D:\q.txt"));
        assert_eq!(a, b);
        assert_eq!(a.klyuch, e.klyuch);
        assert_ne!(a.klyuch, d.klyuch);
        // опрошены три буквы: C (дважды в кэше), E, D
        assert_eq!(vyzovy.load(Ordering::SeqCst), 3);

        let set1 = opredelitel.dlya_puti(Path::new(r"\\Server\Share\a.txt"));
        let set2 = opredelitel.dlya_puti(Path::new(r"\\server\share\b\c.txt"));
        assert_eq!(set1.klyuch, set2.klyuch);
        assert_eq!(set1.potokov, 1);
        assert_eq!(vyzovy.load(Ordering::SeqCst), 3);

        let prochee = opredelitel.dlya_puti(Path::new("otnositelnyy.txt"));
        assert_eq!(prochee.klyuch, KlyuchDiska::Neizvestno);
        assert_eq!(prochee.potokov, 2);
    }

    #[test]
    fn prochitat_nachalo_chitaet_ne_bolshe_zaprosa() {
        let dir = tempfile::tempdir().expect("tempdir");
        let put = dir.path().join("f.bin");
        std::fs::write(&put, vec![7u8; 1000]).expect("write");
        assert_eq!(prochitat_nachalo(&put, 100).expect("read").len(), 100);
        assert_eq!(prochitat_nachalo(&put, 5000).expect("read").len(), 1000);
        assert!(prochitat_nachalo(&dir.path().join("net.bin"), 10).is_err());
    }

    #[test]
    fn nastoyashchiy_opros_c_vozvrashchaet_otvet() {
        let opredelitel = OpredelitelDiskov::new(false);
        let svedeniya = opredelitel.dlya_puti(Path::new(r"C:\Windows\notepad.exe"));
        assert!(svedeniya.potokov >= 1);
    }
}

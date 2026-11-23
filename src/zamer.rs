//! Замер чтения дисков: чтение набора файлов несколькими потоками в обход кэша Windows,
//! номер записи файла в MFT, потоковая запись с принудительным сбросом на диск.

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_NO_BUFFERING,
    FILE_FLAG_SEQUENTIAL_SCAN, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FlushFileBuffers, GetFileInformationByHandle, OPEN_EXISTING, ReadFile,
};
use windows::core::PCWSTR;

/// Выравнивание буферов и кратность размера чтения для чтения мимо кэша (байт).
pub const VYRAVNIVANIE: usize = 4096;

/// Размер буфера чтения одного потока: 1 МБ (кратно `VYRAVNIVANIE`).
const BUFER_CHTENIYA: usize = 1 << 20;

/// Размер куска потоковой записи: 64 МБ.
const KUSOK_ZAPISI: usize = 64 << 20;

/// GENERIC_READ (в нужном наборе признаков windows отдельной константы нет).
const GENERIC_READ: u32 = 0x8000_0000;

/// Файл набора для замера.
#[derive(Clone, Debug)]
pub struct Fayl {
    /// Путь к файлу.
    pub put: PathBuf,
    /// Размер файла в байтах.
    pub razmer: u64,
}

/// Итог одной ячейки замера.
#[derive(Clone, Debug, Default)]
pub struct ItogYacheyki {
    /// Сколько файлов прочитано (с учётом повторных чтений).
    pub faylov: u64,
    /// Сколько байт прочитано.
    pub bayt: u64,
    /// Фактическое время ячейки в секундах.
    pub sekund: f64,
    /// true, если хоть один файл читался через кэш Windows (чтение мимо кэша не вышло).
    pub kesh: bool,
    /// true, если свежие файлы кончились и набор читался по кругу.
    pub povtor: bool,
}

/// Буфер, выровненный по `VYRAVNIVANIE`.
struct VyrovnennyBufer {
    ukazatel: *mut u8,
    maket: Layout,
}

impl VyrovnennyBufer {
    fn new(razmer: usize) -> Self {
        let maket = Layout::from_size_align(razmer, VYRAVNIVANIE).expect("верный макет буфера");
        // SAFETY: размер ненулевой, макет корректен.
        let ukazatel = unsafe { alloc_zeroed(maket) };
        if ukazatel.is_null() {
            std::alloc::handle_alloc_error(maket);
        }
        Self { ukazatel, maket }
    }

    fn srez(&mut self) -> &mut [u8] {
        // SAFETY: указатель выделен на `maket.size()` байт и инициализирован нулями;
        // доступ уникален через &mut self.
        unsafe { std::slice::from_raw_parts_mut(self.ukazatel, self.maket.size()) }
    }
}

impl Drop for VyrovnennyBufer {
    fn drop(&mut self) {
        // SAFETY: указатель получен из alloc_zeroed с тем же макетом.
        unsafe { dealloc(self.ukazatel, self.maket) };
    }
}

/// Дескриптор Windows, закрываемый при выходе из области видимости.
struct Deskriptor(HANDLE);

impl Drop for Deskriptor {
    fn drop(&mut self) {
        // SAFETY: дескриптор получен от CreateFileW и закрывается один раз.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn shirokiy_put(put: &Path) -> Vec<u16> {
    put.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn otkryt(
    put: &Path,
    dostup: u32,
    flagi: windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES,
) -> io::Result<Deskriptor> {
    let shirokiy = shirokiy_put(put);
    // SAFETY: строка завершена нулём и живёт до конца вызова.
    let res = unsafe {
        CreateFileW(
            PCWSTR(shirokiy.as_ptr()),
            dostup,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            flagi,
            None,
        )
    };
    match res {
        Ok(h) if !h.is_invalid() => Ok(Deskriptor(h)),
        Ok(_) => Err(io::Error::last_os_error()),
        Err(e) => Err(io::Error::from_raw_os_error(e.code().0 & 0xFFFF)),
    }
}

fn otkryt_bez_kesha(put: &Path) -> io::Result<Deskriptor> {
    otkryt(
        put,
        GENERIC_READ,
        FILE_FLAG_NO_BUFFERING | FILE_FLAG_SEQUENTIAL_SCAN,
    )
}

/// Читает файл до конца мимо кэша, возвращает число прочитанных байт.
fn prochitat_bez_kesha(d: &Deskriptor, bufer: &mut [u8]) -> u64 {
    let mut itogo = 0u64;
    loop {
        let mut prochitano = 0u32;
        // SAFETY: буфер выровнен по 4096, его длина кратна 4096.
        let res = unsafe { ReadFile(d.0, Some(bufer), Some(&mut prochitano), None) };
        if res.is_err() {
            break;
        }
        itogo += u64::from(prochitano);
        if (prochitano as usize) < bufer.len() {
            break;
        }
    }
    itogo
}

/// Обычное чтение через `std::fs::File` (через кэш Windows).
fn prochitat_obychno(put: &Path, bufer: &mut [u8]) -> Option<u64> {
    let mut f = File::open(put).ok()?;
    let mut itogo = 0u64;
    loop {
        match f.read(bufer) {
            Ok(0) => break,
            Ok(n) => itogo += n as u64,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    Some(itogo)
}

/// Читает файлы набора `k` потоками в течение `dlitelnost`. Файлы раздаются общим атомарным счётчиком,
/// начиная с индекса `*sdvig`; каждый файл в ячейке читается один раз; свежие кончились — по кругу и povtor=true.
/// По окончании `*sdvig` указывает на первый непрочитанный файл (следующая ячейка берёт свежие).
/// Поток, начавший файл, дочитывает его; новые файлы после истечения времени не берутся. sekund — фактическое время.
pub fn yacheyka_chteniya(
    fayly: &[Fayl],
    k: usize,
    dlitelnost: Duration,
    sdvig: &mut usize,
) -> ItogYacheyki {
    yacheyka_s_predelom(fayly, k, dlitelnost, sdvig, usize::MAX)
}

/// То же, что `yacheyka_chteniya`, но не берёт больше `predel` файлов (нужно для детерминированных тестов).
fn yacheyka_s_predelom(
    fayly: &[Fayl],
    k: usize,
    dlitelnost: Duration,
    sdvig: &mut usize,
    predel: usize,
) -> ItogYacheyki {
    let nachalo = Instant::now();
    let n = fayly.len();
    if n == 0 {
        return ItogYacheyki {
            sekund: nachalo.elapsed().as_secs_f64(),
            ..ItogYacheyki::default()
        };
    }
    let start = *sdvig % n;
    let svezhih = n - start;
    let potokov = k.max(1);
    let schetchik = AtomicUsize::new(0);
    let faylov = AtomicU64::new(0);
    let bayt = AtomicU64::new(0);
    let kesh = AtomicBool::new(false);
    let konec = nachalo + dlitelnost;

    std::thread::scope(|s| {
        for _ in 0..potokov {
            s.spawn(|| {
                let mut bufer = VyrovnennyBufer::new(BUFER_CHTENIYA);
                loop {
                    if Instant::now() >= konec {
                        break;
                    }
                    // Занимаем следующий номер, не превышая предел.
                    let nomer =
                        match schetchik.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |c| {
                            (c < predel).then_some(c + 1)
                        }) {
                            Ok(c) => c,
                            Err(_) => break,
                        };
                    let fayl = &fayly[(start + nomer) % n];
                    let prochitano = match otkryt_bez_kesha(&fayl.put) {
                        Ok(d) => Some(prochitat_bez_kesha(&d, bufer.srez())),
                        Err(_) => {
                            let r = prochitat_obychno(&fayl.put, bufer.srez());
                            if r.is_some() {
                                kesh.store(true, Ordering::Relaxed);
                            }
                            r
                        }
                    };
                    if let Some(b) = prochitano {
                        faylov.fetch_add(1, Ordering::Relaxed);
                        bayt.fetch_add(b, Ordering::Relaxed);
                    }
                }
            });
        }
    });

    let vzyato = schetchik.load(Ordering::SeqCst);
    *sdvig = (start + vzyato) % n;
    ItogYacheyki {
        faylov: faylov.load(Ordering::Relaxed),
        bayt: bayt.load(Ordering::Relaxed),
        sekund: nachalo.elapsed().as_secs_f64(),
        kesh: kesh.load(Ordering::Relaxed),
        povtor: vzyato > svezhih,
    }
}

/// Номер записи файла в MFT: GetFileInformationByHandle, (nFileIndexHigh << 32) | nFileIndexLow.
pub fn nomer_mft(put: &Path) -> io::Result<u64> {
    // Доступ 0 достаточен для чтения атрибутов; BACKUP_SEMANTICS позволяет открывать и папки.
    let d = otkryt(put, 0, FILE_FLAG_BACKUP_SEMANTICS)?;
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: дескриптор действителен, info — корректный выходной параметр.
    unsafe { GetFileInformationByHandle(d.0, &mut info) }
        .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
    Ok((u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow))
}

/// Удаляет файл при выходе из области видимости.
struct UdalitFayl(PathBuf);

impl Drop for UdalitFayl {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Потоковая запись кусками по 64 МБ с FlushFileBuffers после каждого куска в новый файл внутри `papka`,
/// пока не поднят `stop`. Возвращает записанные байты; свой файл удаляет в конце.
pub fn zapis_potokom(papka: &Path, stop: &AtomicBool) -> io::Result<u64> {
    let imya = format!(
        "zamer_zapis_{}_{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let put = papka.join(imya);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&put)?;
    let _udalit = UdalitFayl(put);

    // Несжимаемые данные (xorshift), чтобы сжатие не искажало замер.
    let mut kusok = vec![0u8; KUSOK_ZAPISI];
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for chast in kusok.as_chunks_mut::<8>().0 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *chast = x.to_le_bytes();
    }

    let mut zapisano = 0u64;
    while !stop.load(Ordering::SeqCst) {
        f.write_all(&kusok)?;
        zapisano += kusok.len() as u64;
        // File::sync_all — это и есть FlushFileBuffers; вызываем напрямую для явности.
        let h = {
            use std::os::windows::io::AsRawHandle;
            HANDLE(f.as_raw_handle())
        };
        // SAFETY: дескриптор принадлежит открытому файлу `f`.
        unsafe { FlushFileBuffers(h) }
            .map_err(|e| io::Error::from_raw_os_error(e.code().0 & 0xFFFF))?;
    }
    drop(f);
    Ok(zapisano)
}

/// true, если файл открывается с FILE_FLAG_NO_BUFFERING (чтение мимо кэша Windows возможно).
pub fn chtenie_mimo_kesha(put: &Path) -> bool {
    otkryt_bez_kesha(put).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const RAZMERY: [u64; 6] = [0, 1, 4095, 4096, 4097, 3 * 1024 * 1024 + 5];

    fn sozdat_fayly(papka: &Path, razmery: &[u64]) -> Vec<Fayl> {
        razmery
            .iter()
            .enumerate()
            .map(|(i, &r)| {
                let put = papka.join(format!("f{i}_{r}.bin"));
                let dannye: Vec<u8> = (0..r).map(|j| (j % 251) as u8).collect();
                std::fs::write(&put, dannye).unwrap();
                Fayl { put, razmer: r }
            })
            .collect()
    }

    #[test]
    fn bayt_ravno_summe_razmerov() {
        let t = tempfile::tempdir().unwrap();
        let fayly = sozdat_fayly(t.path(), &RAZMERY);
        let summa: u64 = RAZMERY.iter().sum();
        for k in [1, 3] {
            let mut sdvig = 0;
            let it = yacheyka_s_predelom(&fayly, k, Duration::from_secs(10), &mut sdvig, 6);
            assert_eq!(it.faylov, 6, "k={k}");
            assert_eq!(it.bayt, summa, "k={k}");
            assert!(!it.povtor);
            assert_eq!(sdvig, 0);
        }
    }

    #[test]
    fn kazhdyy_razmer_otdelno() {
        let t = tempfile::tempdir().unwrap();
        let fayly = sozdat_fayly(t.path(), &RAZMERY);
        for (i, f) in fayly.iter().enumerate() {
            let mut sdvig = i;
            let it = yacheyka_s_predelom(&fayly, 1, Duration::from_secs(10), &mut sdvig, 1);
            assert_eq!(it.bayt, f.razmer, "файл {i}");
            assert_eq!(it.faylov, 1);
        }
    }

    #[test]
    fn ogranichenie_po_vremeni() {
        let t = tempfile::tempdir().unwrap();
        let fayly = sozdat_fayly(t.path(), &RAZMERY);
        let mut sdvig = 0;
        let nachalo = Instant::now();
        let it = yacheyka_chteniya(&fayly, 2, Duration::from_millis(300), &mut sdvig);
        let proshlo = nachalo.elapsed();
        assert!(proshlo < Duration::from_millis(1500), "заняло {proshlo:?}");
        assert!(it.sekund >= 0.3 && it.sekund < 1.5, "sekund={}", it.sekund);
        assert!(it.faylov >= 6);
    }

    #[test]
    fn povtor_pri_malom_nabore() {
        let t = tempfile::tempdir().unwrap();
        let fayly = sozdat_fayly(t.path(), &[10, 20, 30]);
        // Детерминированно, без зависимости от времени: время с запасом, число файлов ограничено.
        let mut sdvig = 0;
        let it = yacheyka_s_predelom(&fayly, 2, Duration::from_secs(60), &mut sdvig, 7);
        assert!(it.povtor);
        assert_eq!(it.faylov, 7);
        assert_eq!(it.bayt, 2 * (10 + 20 + 30) + 10);
        assert_eq!(sdvig, 7 % 3);

        // Детерминированно: из 5 файлов со сдвигом 3 «свежих» 2, берём 4.
        let fayly = sozdat_fayly(t.path(), &[1, 2, 3, 4, 5]);
        let mut sdvig = 3;
        let it = yacheyka_s_predelom(&fayly, 1, Duration::from_secs(10), &mut sdvig, 4);
        assert!(it.povtor);
        assert_eq!(it.faylov, 4);
        assert_eq!(it.bayt, 4 + 5 + 1 + 2);
        assert_eq!(sdvig, 2);
    }

    #[test]
    fn sdvig_prodvigaetsya() {
        let t = tempfile::tempdir().unwrap();
        let fayly = sozdat_fayly(t.path(), &[1, 2, 3, 4, 5]);
        let mut sdvig = 1;
        let it = yacheyka_s_predelom(&fayly, 2, Duration::from_secs(10), &mut sdvig, 2);
        assert!(!it.povtor);
        assert_eq!(sdvig, 3);
        assert_eq!(it.bayt, 2 + 3);
        let it = yacheyka_s_predelom(&fayly, 2, Duration::from_secs(10), &mut sdvig, 2);
        assert!(!it.povtor);
        assert_eq!(sdvig, 0);
        assert_eq!(it.bayt, 4 + 5);
    }

    #[test]
    fn pustoy_nabor() {
        let mut sdvig = 0;
        let it = yacheyka_chteniya(&[], 2, Duration::from_millis(50), &mut sdvig);
        assert_eq!(it.faylov, 0);
        assert_eq!(it.bayt, 0);
    }

    #[test]
    fn nomer_mft_raznyy() {
        let t = tempfile::tempdir().unwrap();
        let fayly = sozdat_fayly(t.path(), &[1, 2]);
        let a = nomer_mft(&fayly[0].put).unwrap();
        let b = nomer_mft(&fayly[1].put).unwrap();
        assert_ne!(a, b);
        assert_eq!(a, nomer_mft(&fayly[0].put).unwrap());
    }

    #[test]
    fn mimo_kesha_otkryvaetsya() {
        let t = tempfile::tempdir().unwrap();
        let fayly = sozdat_fayly(t.path(), &[100]);
        let mozhno = chtenie_mimo_kesha(&fayly[0].put);
        println!("чтение мимо кэша во временной папке: {mozhno}");
        let mut sdvig = 0;
        let it = yacheyka_s_predelom(&fayly, 1, Duration::from_secs(10), &mut sdvig, 1);
        assert_eq!(it.kesh, !mozhno);
        assert!(!chtenie_mimo_kesha(&t.path().join("net_takogo.bin")));
    }

    #[test]
    fn zapis_potokom_ostanavlivaetsya_i_chistit() {
        let t = tempfile::tempdir().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let taimer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            stop2.store(true, Ordering::SeqCst);
        });
        let zapisano = zapis_potokom(t.path(), &stop).unwrap();
        taimer.join().unwrap();
        assert!(zapisano > 0);
        assert_eq!(std::fs::read_dir(t.path()).unwrap().count(), 0);
    }
}

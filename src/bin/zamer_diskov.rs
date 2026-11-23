//! Замер скорости дисков для модели планировщика ПоисКовичеК.
//!
//! Подкоманды:
//! - `podgotovka --disk C: --disk D:` — создаёт наборы тестовых файлов;
//! - `opyty --disk C: --vyhod zamery.csv [--predel-min 12] [--tolko A,C] [--tolko-plan]` — опыты A–E
//!   (`--tolko` — выполнить только названные опыты);
//! - `ochistka --disk C:` — удаляет папки замера (только с файлом-меткой).
//!
//! Ключ `--bystro` — проверочный режим (до 2 минут, только C:, временная папка).

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use poiskovichek::zamer::{self, Fayl, ItogYacheyki};
use poiskovichek::{DiskClass, ExtractConfig, disk_info_for_letter, extract_bytes};

/// Имя папки замера на диске.
const PAPKA: &str = "ПоисКовичеК-замер";
/// Файл-метка: без неё папка не удаляется.
const METKA: &str = ".poiskovichek-zamer";
/// Файлов в одной подпапке набора.
const FAYLOV_V_PAPKE: usize = 100;
/// Заголовок CSV (ровно как читает tools/model_diskov.py).
const ZAGOLOVOK: &str =
    "opyt,diski,k,nabor,poryadok,povtor_n,faylov,bayt,sekund,kesh,povtor,potokov";
/// Значения k опыта A.
const ZNACHENIYA_K: [usize; 4] = [1, 2, 4, 8];
/// Потоки разбора в опыте D.
const POTOKI_RAZBORA: [usize; 3] = [1, 2, 4];
/// Форматы опыта D.
const FORMATY_D: [&str; 5] = ["txt", "docx", "pdf", "fb2", "html"];
/// Накладные расходы на одну ячейку, секунд.
const NAKLADNYE: f64 = 0.15;
/// Дополнительные накладные расходы ячейки с записью (дозавершение куска 64 МБ), секунд.
const NAKLADNYE_ZAPISI: f64 = 1.5;
/// Оценка времени вытеснения кэша диска перед ячейкой опыта B (чтение 500 МБ с HDD), секунд.
const VYTESNENIE_SEK: f64 = 5.0;
/// Буфер чтения при вытеснении кэша диска: 1 МБ (кратно `zamer::VYRAVNIVANIE`).
const BUFER_VYTESNENIYA: usize = 1 << 20;
/// FILE_FLAG_SEQUENTIAL_SCAN | FILE_FLAG_NO_BUFFERING.
const FLAGI_BEZ_KESHA: u32 = 0x0800_0000 | 0x2000_0000;

/// Набор тестовых файлов.
#[derive(Clone, Copy)]
struct Nabor {
    imya: &'static str,
    chislo: usize,
    razmer: usize,
}

fn nabory(bystro: bool) -> [Nabor; 3] {
    if bystro {
        [
            Nabor {
                imya: "melkie",
                chislo: 500,
                razmer: 4 * 1024,
            },
            Nabor {
                imya: "srednie",
                chislo: 50,
                razmer: 200 * 1024,
            },
            Nabor {
                imya: "krupnye",
                chislo: 3,
                razmer: 20 * 1024 * 1024,
            },
        ]
    } else {
        [
            Nabor {
                imya: "melkie",
                chislo: 10_000,
                razmer: 4 * 1024,
            },
            Nabor {
                imya: "srednie",
                chislo: 2_000,
                razmer: 200 * 1024,
            },
            Nabor {
                imya: "krupnye",
                chislo: 25,
                razmer: 20 * 1024 * 1024,
            },
        ]
    }
}

// ---------------------------------------------------------------- разбор аргументов

struct Argumenty {
    komanda: String,
    diski: Vec<char>,
    vyhod: Option<PathBuf>,
    predel_min: f64,
    bystro: bool,
    tolko_plan: bool,
    /// Названные в `--tolko` опыты (заглавные буквы A–E); пусто — все.
    tolko: Vec<char>,
}

fn razobrat_disk(s: &str) -> Result<char, String> {
    let mut it = s.chars();
    let bukva = it.next().ok_or_else(|| "пустое имя диска".to_string())?;
    let ostatok: String = it.collect();
    if !bukva.is_ascii_alphabetic() || !matches!(ostatok.as_str(), "" | ":" | ":\\" | ":/") {
        return Err(format!(
            "не похоже на диск: «{s}» (ожидается, например, C:)"
        ));
    }
    Ok(bukva.to_ascii_uppercase())
}

fn razobrat_argumenty() -> Result<Argumenty, String> {
    let mut args = std::env::args().skip(1);
    let komanda = args
        .next()
        .ok_or_else(|| "не указана подкоманда".to_string())?;
    let mut a = Argumenty {
        komanda,
        diski: Vec::new(),
        vyhod: None,
        predel_min: 12.0,
        bystro: false,
        tolko_plan: false,
        tolko: Vec::new(),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--disk" => {
                let v = args.next().ok_or("после --disk нужно имя диска")?;
                let d = razobrat_disk(&v)?;
                if !a.diski.contains(&d) {
                    a.diski.push(d);
                }
            }
            "--vyhod" => {
                a.vyhod = Some(PathBuf::from(
                    args.next().ok_or("после --vyhod нужен путь")?,
                ));
            }
            "--predel-min" => {
                let v = args.next().ok_or("после --predel-min нужно число")?;
                a.predel_min = v
                    .replace(',', ".")
                    .parse::<f64>()
                    .map_err(|_| format!("предел не число: «{v}»"))?;
                if !a.predel_min.is_finite() || a.predel_min <= 0.0 {
                    return Err("предел должен быть больше нуля".into());
                }
            }
            "--bystro" => a.bystro = true,
            "--tolko-plan" => a.tolko_plan = true,
            "--tolko" => {
                let v = args
                    .next()
                    .ok_or("после --tolko нужен список опытов, например B или A,C")?;
                for chast in v.split([',', ';', ' ']).filter(|c| !c.is_empty()) {
                    let mut it = chast.chars();
                    let bukva = it.next().map(|c| c.to_ascii_uppercase());
                    match (bukva, it.next()) {
                        (Some(b @ 'A'..='E'), None) => {
                            if !a.tolko.contains(&b) {
                                a.tolko.push(b);
                            }
                        }
                        _ => {
                            return Err(format!(
                                "неизвестный опыт в --tolko: «{chast}» (есть A, B, C, D, E)"
                            ));
                        }
                    }
                }
                if a.tolko.is_empty() {
                    return Err("список опытов в --tolko пуст".into());
                }
            }
            inoe => return Err(format!("неизвестный ключ: «{inoe}»")),
        }
    }
    if a.bystro {
        a.diski = vec!['C'];
    }
    Ok(a)
}

fn spravka() {
    eprintln!(
        "Использование:\n  zamer_diskov podgotovka --disk C: [--disk D: …] [--bystro]\n  \
         zamer_diskov opyty --disk C: [--disk D: …] --vyhod файл.csv [--predel-min 12] [--tolko A,C] [--tolko-plan] [--bystro]\n  \
         zamer_diskov ochistka --disk C: [--disk D: …] [--bystro]"
    );
}

// ---------------------------------------------------------------- вспомогательное

fn rus(x: f64, znakov: usize) -> String {
    format!("{x:.znakov$}").replace('.', ",")
}

fn vremya_txt(sek: f64) -> String {
    let s = sek.round() as u64;
    if s >= 60 {
        format!("{} мин {:02} с", s / 60, s % 60)
    } else {
        format!("{s} с")
    }
}

fn imya_diska(d: char) -> String {
    format!("{d}:")
}

fn koren(disk: char, bystro: bool) -> PathBuf {
    if bystro {
        std::env::temp_dir().join(PAPKA)
    } else {
        PathBuf::from(format!("{disk}:\\")).join(PAPKA)
    }
}

/// Быстрый некриптостойкий генератор xorshift64.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

// ---------------------------------------------------------------- генератор текста

const SLOVA: &[&str] = &[
    "север",
    "морской",
    "путь",
    "корабль",
    "берег",
    "ветер",
    "поле",
    "город",
    "река",
    "лес",
    "книга",
    "письмо",
    "дорога",
    "время",
    "человек",
    "работа",
    "вопрос",
    "ответ",
    "дом",
    "окно",
    "земля",
    "небо",
    "солнце",
    "ночь",
    "утро",
    "вечер",
    "зима",
    "весна",
    "лето",
    "осень",
    "история",
    "документ",
    "отчёт",
    "справка",
    "договор",
    "проект",
    "задача",
    "решение",
    "числа",
    "программа",
    "система",
    "память",
    "диск",
    "файл",
    "папка",
    "поиск",
    "слово",
    "текст",
    "страница",
    "глава",
    "рассказ",
    "роман",
    "поэт",
    "музыка",
    "картина",
    "театр",
    "школа",
    "учитель",
    "студент",
    "завод",
    "станция",
    "поезд",
    "мост",
    "площадь",
    "улица",
    "сад",
    "огород",
    "урожай",
    "дерево",
    "яблоня",
    "груша",
    "цветок",
    "трава",
    "камень",
    "вода",
    "огонь",
    "воздух",
    "горы",
    "море",
    "большой",
    "малый",
    "новый",
    "старый",
    "светлый",
    "тёмный",
    "быстрый",
    "тихий",
    "дальний",
    "и",
    "в",
    "на",
    "с",
    "по",
    "из",
    "за",
    "от",
    "до",
    "для",
    "что",
    "как",
    "это",
    "был",
    "есть",
];

/// Пул псевдослучайного русского текста (UTF-8); окна из него идут в файлы.
struct Istochnik {
    pool: Vec<u8>,
    rng: Rng,
}

impl Istochnik {
    fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let mut pool = Vec::with_capacity(4 * 1024 * 1024 + 64);
        while pool.len() < 4 * 1024 * 1024 {
            let r = rng.next();
            let slovo = SLOVA[(r % SLOVA.len() as u64) as usize];
            pool.extend_from_slice(slovo.as_bytes());
            match (r >> 20) % 23 {
                0 => pool.extend_from_slice(".\n".as_bytes()),
                1 => pool.extend_from_slice(", ".as_bytes()),
                _ => pool.push(b' '),
            }
        }
        if let Some(last) = pool.last_mut() {
            *last = b' ';
        }
        Self { pool, rng }
    }

    /// Заполняет `buf` ровно `razmer` байтами корректного UTF-8.
    fn zapolnit(&mut self, razmer: usize, buf: &mut Vec<u8>) {
        buf.clear();
        let n = self.pool.len();
        let mut smeshenie = (self.rng.next() % n as u64) as usize;
        // Начало — сразу после пробела: граница символа.
        while self.pool[smeshenie] != b' ' {
            smeshenie = (smeshenie + 1) % n;
        }
        smeshenie = (smeshenie + 1) % n;
        while buf.len() < razmer {
            let berem = (razmer - buf.len()).min(n - smeshenie);
            buf.extend_from_slice(&self.pool[smeshenie..smeshenie + berem]);
            smeshenie = (smeshenie + berem) % n;
        }
        // Обрыв внутри двухбайтового символа — заменяем ведущий байт пробелом.
        if let Some(last) = buf.last_mut()
            && *last >= 0xC0
        {
            *last = b' ';
        }
    }
}

// ---------------------------------------------------------------- списки файлов

fn obhod(papka: &Path, itog: &mut Vec<Fayl>) -> std::io::Result<()> {
    for zapis in fs::read_dir(papka)? {
        let zapis = zapis?;
        let tip = zapis.file_type()?;
        if tip.is_dir() {
            obhod(&zapis.path(), itog)?;
        } else if tip.is_file() {
            itog.push(Fayl {
                put: zapis.path(),
                razmer: zapis.metadata()?.len(),
            });
        }
    }
    Ok(())
}

/// Файлы набора по порядку путей. Пусто, если папки нет.
fn spisok_nabora(papka: &Path) -> Vec<Fayl> {
    let mut v = Vec::new();
    if papka.is_dir() {
        let _ = obhod(papka, &mut v);
    }
    v.sort_by(|a, b| a.put.cmp(&b.put));
    v
}

fn nabor_polon(papka: &Path, n: &Nabor) -> bool {
    let v = spisok_nabora(papka);
    v.len() == n.chislo && v.iter().all(|f| f.razmer == n.razmer as u64)
}

// ---------------------------------------------------------------- подготовка

fn podgotovit_nabor(papka: &Path, n: &Nabor, seed: u64) -> Result<(), String> {
    if papka.exists() {
        fs::remove_dir_all(papka)
            .map_err(|e| format!("не удалось очистить {}: {e}", papka.display()))?;
    }
    let mut ist = Istochnik::new(seed);
    let mut buf = Vec::with_capacity(n.razmer);
    for i in 0..n.chislo {
        let podpapka = papka.join(format!("p{:04}", i / FAYLOV_V_PAPKE));
        if i % FAYLOV_V_PAPKE == 0 {
            fs::create_dir_all(&podpapka).map_err(|e| format!("{}: {e}", podpapka.display()))?;
        }
        ist.zapolnit(n.razmer, &mut buf);
        let put = podpapka.join(format!("f{i:06}.txt"));
        fs::write(&put, &buf).map_err(|e| format!("{}: {e}", put.display()))?;
    }
    Ok(())
}

fn podgotovit_disk(disk: char, bystro: bool) -> Result<(), String> {
    let kor = koren(disk, bystro);
    fs::create_dir_all(&kor).map_err(|e| format!("не удалось создать {}: {e}", kor.display()))?;
    let metka = kor.join(METKA);
    if !metka.exists() {
        fs::write(
            &metka,
            "создано zamer_diskov (ПоисКовичеК); удаляется командой ochistka\n",
        )
        .map_err(|e| format!("{}: {e}", metka.display()))?;
    }
    let nachalo_diska = Instant::now();
    for (i, n) in nabory(bystro).iter().enumerate() {
        let papka = kor.join(n.imya);
        let t = Instant::now();
        if nabor_polon(&papka, n) {
            println!(
                "{}: набор {} уже есть ({} файлов) — пропуск",
                imya_diska(disk),
                n.imya,
                n.chislo
            );
            continue;
        }
        podgotovit_nabor(&papka, n, (disk as u64) * 1000 + i as u64 + 1)?;
        println!(
            "{}: набор {} готов: {} файлов по {} КБ за {} с",
            imya_diska(disk),
            n.imya,
            n.chislo,
            n.razmer / 1024,
            rus(t.elapsed().as_secs_f64(), 1)
        );
    }
    println!(
        "{}: подготовка завершена за {} с",
        imya_diska(disk),
        rus(nachalo_diska.elapsed().as_secs_f64(), 1)
    );
    Ok(())
}

fn komanda_podgotovka(a: &Argumenty) -> u8 {
    if a.diski.is_empty() {
        eprintln!("Не указаны диски: --disk C:");
        return 2;
    }
    println!(
        "Подготовка наборов{}: диски {}",
        if a.bystro {
            " (быстрый режим)"
        } else {
            ""
        },
        a.diski
            .iter()
            .map(|d| imya_diska(*d))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let nachalo = Instant::now();
    let oshibki: Vec<String> = thread::scope(|s| {
        let potoki: Vec<_> = a
            .diski
            .iter()
            .map(|&d| {
                s.spawn(move || {
                    podgotovit_disk(d, a.bystro).map_err(|e| format!("{}: {e}", imya_diska(d)))
                })
            })
            .collect();
        potoki
            .into_iter()
            .filter_map(|p| match p.join() {
                Ok(Ok(())) => None,
                Ok(Err(e)) => Some(e),
                Err(_) => Some("поток подготовки аварийно завершён".to_string()),
            })
            .collect()
    });
    for e in &oshibki {
        eprintln!("Ошибка: {e}");
    }
    println!("Всего: {} с", rus(nachalo.elapsed().as_secs_f64(), 1));
    u8::from(!oshibki.is_empty())
}

// ---------------------------------------------------------------- очистка

fn komanda_ochistka(a: &Argumenty) -> u8 {
    if a.diski.is_empty() {
        eprintln!("Не указаны диски: --disk C:");
        return 2;
    }
    let mut kod = 0;
    for &d in &a.diski {
        let kor = koren(d, a.bystro);
        if !kor.is_dir() {
            println!("{}: папки замера нет — нечего удалять", imya_diska(d));
            continue;
        }
        if !kor.join(METKA).is_file() {
            println!(
                "{}: в {} нет файла-метки — не трогаю",
                imya_diska(d),
                kor.display()
            );
            continue;
        }
        match fs::remove_dir_all(&kor) {
            Ok(()) => println!("{}: папка {} удалена", imya_diska(d), kor.display()),
            Err(e) => {
                eprintln!(
                    "{}: не удалось удалить {}: {e}",
                    imya_diska(d),
                    kor.display()
                );
                kod = 1;
            }
        }
    }
    kod
}

// ---------------------------------------------------------------- план

struct Plan {
    povtorov: usize,
    dlitelnost: f64,
    a: usize,
    b: usize,
    c: usize,
    d: usize,
    e: usize,
    e_s_zapisyu: usize,
    /// Оценка времени вытеснения кэша диска в опыте B, секунд.
    vytesnenie: f64,
    ocenka: f64,
    opisanie: Vec<String>,
}

impl Plan {
    fn vsego(&self) -> usize {
        self.a + self.b + self.c + self.d + self.e
    }

    /// Оценка вытеснения кэша на одну ячейку опыта B, секунд.
    fn vytesnenie_na_yacheyku(&self) -> f64 {
        if self.b == 0 { 0.0 } else { VYTESNENIE_SEK }
    }
}

/// Все сочетания по 2 и 3 диска (по порядку в списке).
fn sochetaniya(diski: &[char]) -> Vec<Vec<char>> {
    let mut r = Vec::new();
    for i in 0..diski.len() {
        for j in i + 1..diski.len() {
            r.push(vec![diski[i], diski[j]]);
            for l in j + 1..diski.len() {
                r.push(vec![diski[i], diski[j], diski[l]]);
            }
        }
    }
    r
}

/// Диск для записи «на другой диск» в опыте E.
fn drugoy_disk(diski: &[char]) -> Option<char> {
    diski.iter().copied().find(|&d| d != 'C')
}

/// Выбран ли опыт `o`; пустой список `--tolko` — выбраны все.
fn vybran(tolko: &[char], o: char) -> bool {
    tolko.is_empty() || tolko.contains(&o)
}

fn sostavit_plan(
    diski: &[(char, DiskClass)],
    bystro: bool,
    predel_min: f64,
    formatov_d: usize,
    tolko: &[char],
) -> Plan {
    let povtorov = if bystro { 1 } else { 3 };
    let bukvy: Vec<char> = diski.iter().map(|d| d.0).collect();
    let n_hdd = diski.iter().filter(|d| d.1 == DiskClass::Hdd).count();
    let komb = sochetaniya(&bukvy).len();
    let ya = |o: char| usize::from(vybran(tolko, o));
    let a = ya('A') * diski.len() * ZNACHENIYA_K.len() * 3 * povtorov;
    let b = ya('B') * n_hdd * 3 * povtorov;
    let c = ya('C') * komb * 2 * povtorov;
    let d = ya('D') * formatov_d * POTOKI_RAZBORA.len() * povtorov;
    let est_c = bukvy.contains(&'C') && vybran(tolko, 'E');
    let drugoy = est_c && drugoy_disk(&bukvy).is_some();
    let e = if est_c {
        povtorov * (1 + usize::from(drugoy))
    } else {
        0
    };
    let e_s_zapisyu = if est_c {
        povtorov * (1 + usize::from(drugoy))
    } else {
        0
    };
    let vsego = a + b + c + d + e;
    let vytesnenie = b as f64 * VYTESNENIE_SEK;
    let fiks = if bystro { 2.0 } else { 15.0 };
    let dlitelnost = if bystro {
        0.5
    } else {
        let byudzhet = predel_min * 60.0 * 0.9
            - fiks
            - vsego as f64 * NAKLADNYE
            - vytesnenie
            - e_s_zapisyu as f64 * NAKLADNYE_ZAPISI;
        let d = if vsego == 0 {
            0.5
        } else {
            byudzhet / vsego as f64
        };
        (d.clamp(0.5, 15.0) * 10.0).floor() / 10.0
    };
    let ocenka = fiks
        + vsego as f64 * (dlitelnost + NAKLADNYE)
        + vytesnenie
        + e_s_zapisyu as f64 * NAKLADNYE_ZAPISI;

    let mut opisanie = Vec::new();
    let mut opisat = |o: char, imya: &str, tekst: String| {
        if vybran(tolko, o) {
            opisanie.push(format!("Опыт {o} ({imya}): {tekst}"));
        } else {
            opisanie.push(format!(
                "Опыт {o} ({imya}): не выбран ключом --tolko — пропущен"
            ));
        }
    };
    opisat(
        'A',
        "чтение каждого диска отдельно",
        format!(
            "{a} ячеек = {} диск(ов) × {} значения k × 3 набора × {povtorov} повтор(а)",
            diski.len(),
            ZNACHENIYA_K.len()
        ),
    );
    opisat(
        'B',
        "порядок чтения на HDD",
        format!(
            "{b} ячеек = {n_hdd} диск(ов) класса Hdd × 3 порядка × {povtorov} повтор(а); набор делится поровну между ячейками диска (у каждой свои файлы, по кругу не читается); перед каждой ячейкой вытеснение кэша диска (чтение набора krupnye, оценка {} с на ячейку, всего {} с)",
            rus(VYTESNENIE_SEK, 0),
            rus(vytesnenie, 0)
        ),
    );
    opisat(
        'C',
        "диски одновременно",
        format!("{c} ячеек = {komb} сочетаний × 2 значения k × {povtorov} повтор(а)"),
    );
    opisat(
        'D',
        "разбор текста",
        format!(
            "{d} ячеек = {formatov_d} форматов × {} значения потоков × {povtorov} повтор(а)",
            POTOKI_RAZBORA.len()
        ),
    );
    let e_txt = if !bukvy.contains(&'C') {
        "диска C: нет в списке — опыт пропущен".to_string()
    } else if drugoy {
        format!("{e} ячеек = (запись на тот же диск, запись на другой диск) × {povtorov} повтор(а)")
    } else {
        format!(
            "{e} ячеек = (запись на тот же диск) × {povtorov} повтор(а); запись на другой диск пропущена — диск один"
        )
    };
    opisat('E', "помехи записи", e_txt);

    Plan {
        povtorov,
        dlitelnost,
        a,
        b,
        c,
        d,
        e,
        e_s_zapisyu,
        vytesnenie,
        ocenka,
        opisanie,
    }
}

fn napechatat_plan(p: &Plan, diski: &[(char, DiskClass)], predel_min: f64) {
    println!(
        "План опытов (предел {} мин): диски {}",
        rus(predel_min, if predel_min.fract() == 0.0 { 0 } else { 1 }),
        diski
            .iter()
            .map(|(d, k)| format!("{}:={}", d, k.name()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    for s in &p.opisanie {
        println!("  {s}");
    }
    println!("  Всего ячеек: {}", p.vsego());
    println!("  Повторов каждой ячейки: {}", p.povtorov);
    println!("  Длительность ячейки: {} с", rus(p.dlitelnost, 1));
    if p.vytesnenie > 0.0 {
        println!(
            "  Вытеснение кэша диска (опыт B, вне времени ячейки): {} с",
            rus(p.vytesnenie, 0)
        );
    }
    println!(
        "  Общая оценка: {} (записей с помехой: {}; предел {})",
        vremya_txt(p.ocenka),
        p.e_s_zapisyu,
        vremya_txt(predel_min * 60.0)
    );
    if p.ocenka > predel_min * 60.0 {
        println!("  ВНИМАНИЕ: оценка выше предела, часть ячеек будет пропущена.");
    }
}

// ---------------------------------------------------------------- документы для опыта D

struct FormatD {
    imya: String,
    dokumenty: Vec<Vec<u8>>,
}

fn papka_proba() -> Option<PathBuf> {
    let mut kandidaty = vec![PathBuf::from("tests").join("proba")];
    kandidaty.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("proba"),
    );
    if let Ok(exe) = std::env::current_exe() {
        for p in exe.ancestors().skip(1).take(5) {
            kandidaty.push(p.join("tests").join("proba"));
        }
    }
    kandidaty.into_iter().find(|p| p.is_dir())
}

fn zagruzit_formaty() -> Vec<FormatD> {
    let Some(papka) = papka_proba() else {
        return Vec::new();
    };
    let mut itog = Vec::new();
    for fmt in FORMATY_D {
        let mut puti: Vec<PathBuf> = fs::read_dir(&papka)
            .map(|r| {
                r.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| {
                        p.extension()
                            .and_then(|e| e.to_str())
                            .is_some_and(|e| e.eq_ignore_ascii_case(fmt))
                    })
                    .collect()
            })
            .unwrap_or_default();
        puti.sort();
        let dokumenty: Vec<Vec<u8>> = puti.iter().filter_map(|p| fs::read(p).ok()).collect();
        if !dokumenty.is_empty() {
            itog.push(FormatD {
                imya: fmt.to_string(),
                dokumenty,
            });
        }
    }
    itog
}

fn yacheyka_razbora(f: &FormatD, potokov: usize, dlitelnost: Duration) -> ItogYacheyki {
    let konfig = ExtractConfig::default();
    let schetchik = AtomicUsize::new(0);
    let faylov = AtomicU64::new(0);
    let bayt = AtomicU64::new(0);
    let n = f.dokumenty.len();
    let nachalo = Instant::now();
    let konec = nachalo + dlitelnost;
    thread::scope(|s| {
        for _ in 0..potokov {
            let _ = thread::Builder::new()
                .stack_size(16 << 20)
                .spawn_scoped(s, || {
                    while Instant::now() < konec {
                        let i = schetchik.fetch_add(1, Ordering::Relaxed);
                        let dok = &f.dokumenty[i % n];
                        let r = extract_bytes(dok, &f.imya, false, &konfig);
                        std::hint::black_box(&r);
                        faylov.fetch_add(1, Ordering::Relaxed);
                        bayt.fetch_add(dok.len() as u64, Ordering::Relaxed);
                    }
                });
        }
    });
    let f_n = faylov.load(Ordering::Relaxed);
    ItogYacheyki {
        faylov: f_n,
        bayt: bayt.load(Ordering::Relaxed),
        sekund: nachalo.elapsed().as_secs_f64(),
        kesh: false,
        povtor: f_n > n as u64,
    }
}

// ---------------------------------------------------------------- опыты

struct DiskOpyta {
    bukva: char,
    imya: String,
    klass: DiskClass,
    koren: PathBuf,
    nabory: HashMap<&'static str, Vec<Fayl>>,
}

struct Hod {
    vyhod: File,
    nomer: usize,
    vsego: usize,
    dlitelnost: Duration,
    konets: Instant,
    ostanovleno: bool,
    propusheno: BTreeMap<&'static str, usize>,
    povtorov: usize,
    sdvigi: HashMap<String, usize>,
    /// Сколько секунд заняло последнее вытеснение кэша диска (до первого — оценка).
    vytesnenie: f64,
}

fn da_net(b: bool) -> &'static str {
    if b { "da" } else { "net" }
}

impl Hod {
    /// Можно ли начинать очередную ячейку; иначе она засчитывается пропущенной.
    fn nachat(&mut self, opyt: &'static str) -> bool {
        self.nachat_s_zapasom(opyt, Duration::ZERO)
    }

    /// То же, но с запасом времени на подготовку ячейки (вне её времени, но в общем пределе).
    fn nachat_s_zapasom(&mut self, opyt: &'static str, zapas: Duration) -> bool {
        if self.ostanovleno || Instant::now() + zapas + self.dlitelnost > self.konets {
            self.ostanovleno = true;
            *self.propusheno.entry(opyt).or_default() += 1;
            return false;
        }
        self.nomer += 1;
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn zapisat(
        &mut self,
        opyt: &str,
        diski: &str,
        k: usize,
        nabor: &str,
        poryadok: &str,
        povtor_n: usize,
        itog: &ItogYacheyki,
        potokov: usize,
        primechanie: &str,
    ) {
        let _ = writeln!(
            self.vyhod,
            "{opyt},{diski},{k},{nabor},{poryadok},{povtor_n},{},{},{:.6},{},{},{potokov}",
            itog.faylov,
            itog.bayt,
            itog.sekund,
            da_net(itog.kesh),
            da_net(itog.povtor)
        );
        let _ = self.vyhod.flush();
        let diski = if diski.is_empty() { "—" } else { diski };
        let mb_s = if itog.sekund > 0.0 {
            itog.bayt as f64 / itog.sekund / 1048576.0
        } else {
            0.0
        };
        println!(
            "[{:>3}/{}] {opyt} {diski} k={k} {nabor}{} повтор {povtor_n}/{}: {} файлов, {} МБ за {} с — {} МБ/с{}{}{}",
            self.nomer,
            self.vsego,
            if poryadok.is_empty() {
                String::new()
            } else {
                format!(" {poryadok}")
            },
            self.povtorov,
            itog.faylov,
            rus(itog.bayt as f64 / 1048576.0, 1),
            rus(itog.sekund, 2),
            rus(mb_s, 1),
            if itog.kesh { ", ЧЕРЕЗ КЭШ" } else { "" },
            if itog.povtor { ", по кругу" } else { "" },
            primechanie
        );
    }

    fn sdvig(&mut self, klyuch: &str) -> usize {
        *self.sdvigi.get(klyuch).unwrap_or(&0)
    }
}

fn opyt_a(hod: &mut Hod, diski: &[DiskOpyta], bystro: bool) {
    for d in diski {
        for n in nabory(bystro) {
            let fayly = &d.nabory[n.imya];
            let klyuch = format!("{}/{}", d.bukva, n.imya);
            for k in ZNACHENIYA_K {
                for povtor in 1..=hod.povtorov {
                    if !hod.nachat("A") {
                        continue;
                    }
                    let mut sd = hod.sdvig(&klyuch);
                    let itog = zamer::yacheyka_chteniya(fayly, k, hod.dlitelnost, &mut sd);
                    hod.sdvigi.insert(klyuch.clone(), sd);
                    hod.zapisat("A", &d.imya, k, n.imya, "papki", povtor, &itog, k, "");
                }
            }
        }
    }
}

/// Читает один файл мимо кэша Windows (если не вышло — через кэш). Возвращает (байт, через ли кэш).
/// `None` — файл не открылся.
fn prochitat_fayl(put: &Path, bufer: &mut [u8]) -> Option<(u64, bool)> {
    let (mut fayl, cherez_kesh) = match OpenOptions::new()
        .read(true)
        .custom_flags(FLAGI_BEZ_KESHA)
        .open(put)
    {
        Ok(h) => (h, false),
        Err(_) => (File::open(put).ok()?, true),
    };
    let mut bayt = 0u64;
    loop {
        match fayl.read(bufer) {
            Ok(0) => break,
            Ok(n) => {
                bayt += n as u64;
                if n < bufer.len() {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    Some((bayt, cherez_kesh))
}

/// Вытесняет кэш диска: читает мимо кэша Windows все файлы `fayly` (набор krupnye этого же диска).
/// Возвращает (прочитано байт, секунд, true — если хоть один файл пришлось читать через кэш Windows).
fn vytesnit_kesh(fayly: &[Fayl]) -> (u64, f64, bool) {
    let nachalo = Instant::now();
    let mut pamyat = vec![0u8; BUFER_VYTESNENIYA + zamer::VYRAVNIVANIE];
    let smeshenie = pamyat.as_ptr().align_offset(zamer::VYRAVNIVANIE);
    let bufer = &mut pamyat[smeshenie..smeshenie + BUFER_VYTESNENIYA];
    let mut bayt = 0u64;
    let mut cherez_kesh = false;
    for f in fayly {
        if let Some((b, k)) = prochitat_fayl(&f.put, bufer) {
            bayt += b;
            cherez_kesh |= k;
        }
    }
    (bayt, nachalo.elapsed().as_secs_f64(), cherez_kesh)
}

/// Ячейка опыта B: один поток читает `fayly` по порядку мимо кэша, каждый файл один раз.
/// Кончается, когда прочитаны все файлы или вышло `dlitelnost` (что раньше); по кругу не читает.
/// Начатый файл дочитывается. `sekund` — фактическое время.
fn yacheyka_porjadka(fayly: &[Fayl], dlitelnost: Duration) -> ItogYacheyki {
    let mut pamyat = vec![0u8; BUFER_VYTESNENIYA + zamer::VYRAVNIVANIE];
    let smeshenie = pamyat.as_ptr().align_offset(zamer::VYRAVNIVANIE);
    let bufer = &mut pamyat[smeshenie..smeshenie + BUFER_VYTESNENIYA];
    let nachalo = Instant::now();
    let konec = nachalo + dlitelnost;
    let mut itog = ItogYacheyki::default();
    for f in fayly {
        if Instant::now() >= konec {
            break;
        }
        if let Some((b, k)) = prochitat_fayl(&f.put, bufer) {
            itog.faylov += 1;
            itog.bayt += b;
            itog.kesh |= k;
        }
    }
    itog.sekund = nachalo.elapsed().as_secs_f64();
    itog
}

/// Номера MFT всех файлов `osnova` (по индексам); `None`, если хоть один не получен.
fn nomera_mft(imya_diska: &str, osnova: &[Fayl]) -> Option<Vec<u64>> {
    let mut itog = Vec::with_capacity(osnova.len());
    for f in osnova {
        match zamer::nomer_mft(&f.put) {
            Ok(m) => itog.push(m),
            Err(e) => {
                eprintln!("{imya_diska}: nomer_mft({}) не вышел: {e}", f.put.display());
                return None;
            }
        }
    }
    Some(itog)
}

/// Опыт B: порядок чтения мелких файлов на HDD (k=1).
/// Набор делится поровну между всеми ячейками диска: у каждой ячейки свои файлы, нигде не читанные
/// раньше (делится по подпапкам: подпапка b достаётся ячейке b % N, так что у каждой доли
/// есть и локальность папок, и разброс по всему диску). Порядок papki/mft/sluchayno применяется
/// внутри доли. Перед каждой ячейкой кэш диска вытесняется чтением набора krupnye.
/// Ячейки идут по кругу порядков (papki, mft, sluchayno, papki, …), чтобы дрейф диска
/// не ложился на один порядок.
fn opyt_b(hod: &mut Hod, diski: &[DiskOpyta]) {
    const PORYADKI: [&str; 3] = ["papki", "mft", "sluchayno"];
    let mut ryad = Rng::new(12345);
    for d in diski.iter().filter(|d| d.klass == DiskClass::Hdd) {
        let osnova = &d.nabory["melkie"];
        let krupnye = &d.nabory["krupnye"];
        let yacheek = PORYADKI.len() * hod.povtorov;
        // Доли: индексы в `osnova`, внутри доли по порядку путей.
        let mut doli: Vec<Vec<usize>> = vec![Vec::new(); yacheek];
        for i in 0..osnova.len() {
            doli[(i / FAYLOV_V_PAPKE) % yacheek].push(i);
        }
        let mut mft: Option<Option<Vec<u64>>> = None;
        for povtor in 1..=hod.povtorov {
            for (nomer_poryadka, poryadok) in PORYADKI.into_iter().enumerate() {
                let dolya = &doli[(povtor - 1) * PORYADKI.len() + nomer_poryadka];
                let zapas = Duration::from_secs_f64(hod.vytesnenie);
                if !hod.nachat_s_zapasom("B", zapas) {
                    continue;
                }
                if dolya.is_empty() {
                    eprintln!(
                        "{}: для ячейки {poryadok} {povtor}/{} не хватило файлов набора — пропущена",
                        d.imya, hod.povtorov
                    );
                    continue;
                }
                // Порядок чтения доли — до вытеснения кэша и вне времени ячейки.
                let ochered: Vec<usize> = match poryadok {
                    "mft" => {
                        let nomera = mft.get_or_insert_with(|| nomera_mft(&d.imya, osnova));
                        match nomera {
                            Some(m) => {
                                let mut v = dolya.clone();
                                v.sort_by_key(|&i| (m[i], i));
                                v
                            }
                            None => {
                                eprintln!(
                                    "{}: порядок {poryadok} недоступен — ячейка пропущена",
                                    d.imya
                                );
                                continue;
                            }
                        }
                    }
                    "sluchayno" => {
                        let mut v = dolya.clone();
                        for i in (1..v.len()).rev() {
                            let j = (ryad.next() % (i as u64 + 1)) as usize;
                            v.swap(i, j);
                        }
                        v
                    }
                    _ => dolya.clone(),
                };
                let fayly: Vec<Fayl> = ochered.iter().map(|&i| osnova[i].clone()).collect();

                // Вытеснение кэша диска: в CSV не пишется, во время ячейки не входит.
                let (bayt_v, sek_v, cherez_kesh) = vytesnit_kesh(krupnye);
                hod.vytesnenie = sek_v;
                println!(
                    "      вытеснение кэша {}: прочитано {} МБ за {} с{}",
                    d.imya,
                    rus(bayt_v as f64 / 1048576.0, 0),
                    rus(sek_v, 1),
                    if cherez_kesh {
                        " (ЧАСТЬ ЧЕРЕЗ КЭШ WINDOWS)"
                    } else {
                        ""
                    }
                );

                let itog = yacheyka_porjadka(&fayly, hod.dlitelnost);
                let primechanie = if (itog.faylov as usize) < fayly.len() {
                    format!(
                        "; время вышло, прочитано {} из {} файлов доли",
                        itog.faylov,
                        fayly.len()
                    )
                } else {
                    format!("; прочитаны все {} файлов доли", fayly.len())
                };
                hod.zapisat(
                    "B",
                    &d.imya,
                    1,
                    "melkie",
                    poryadok,
                    povtor,
                    &itog,
                    1,
                    &primechanie,
                );
            }
        }
    }
}

fn opyt_c(hod: &mut Hod, diski: &[DiskOpyta]) {
    let bukvy: Vec<char> = diski.iter().map(|d| d.bukva).collect();
    for kombinaciya in sochetaniya(&bukvy) {
        let uchastniki: Vec<&DiskOpyta> = kombinaciya
            .iter()
            .filter_map(|b| diski.iter().find(|d| d.bukva == *b))
            .collect();
        let metka = uchastniki
            .iter()
            .map(|d| d.imya.clone())
            .collect::<Vec<_>>()
            .join("+");
        for k in [1usize, 4] {
            for povtor in 1..=hod.povtorov {
                if !hod.nachat("C") {
                    continue;
                }
                let dlit = hod.dlitelnost;
                let start = std::sync::Barrier::new(uchastniki.len());
                let sdvigi_do: Vec<usize> = uchastniki
                    .iter()
                    .map(|d| hod.sdvig(&format!("{}/srednie", d.bukva)))
                    .collect();
                let itogi: Vec<(usize, ItogYacheyki)> = thread::scope(|s| {
                    let potoki: Vec<_> = uchastniki
                        .iter()
                        .zip(&sdvigi_do)
                        .map(|(d, &sd0)| {
                            let start = &start;
                            s.spawn(move || {
                                let mut sd = sd0;
                                start.wait();
                                let it = zamer::yacheyka_chteniya(
                                    &d.nabory["srednie"],
                                    k,
                                    dlit,
                                    &mut sd,
                                );
                                (sd, it)
                            })
                        })
                        .collect();
                    potoki
                        .into_iter()
                        .map(|p| p.join().unwrap_or_else(|_| (0, ItogYacheyki::default())))
                        .collect()
                });
                let mut summa = ItogYacheyki::default();
                let mut skorosti = Vec::new();
                for (d, (sd, it)) in uchastniki.iter().zip(&itogi) {
                    hod.sdvigi.insert(format!("{}/srednie", d.bukva), *sd);
                    summa.faylov += it.faylov;
                    summa.bayt += it.bayt;
                    summa.sekund = summa.sekund.max(it.sekund);
                    summa.kesh |= it.kesh;
                    summa.povtor |= it.povtor;
                    let mb_s = if it.sekund > 0.0 {
                        it.bayt as f64 / it.sekund / 1048576.0
                    } else {
                        0.0
                    };
                    skorosti.push(format!("{} {} МБ/с", d.imya, rus(mb_s, 1)));
                }
                let primechanie = format!("; по дискам: {}", skorosti.join(", "));
                hod.zapisat(
                    "C",
                    &metka,
                    k,
                    "srednie",
                    "vmeste",
                    povtor,
                    &summa,
                    k,
                    &primechanie,
                );
            }
        }
    }
}

fn opyt_d(hod: &mut Hod, formaty: &[FormatD]) {
    for f in formaty {
        for potokov in POTOKI_RAZBORA {
            for povtor in 1..=hod.povtorov {
                if !hod.nachat("D") {
                    continue;
                }
                let itog = yacheyka_razbora(f, potokov, hod.dlitelnost);
                hod.zapisat("D", "", potokov, &f.imya, "", povtor, &itog, potokov, "");
            }
        }
    }
}

/// Ячейка чтения C: (k=4, srednie) с записью в `papka_zapisi` или без записи (`None`).
fn yacheyka_e(hod: &mut Hod, c: &DiskOpyta, papka: &Path, poryadok: &str, povtor: usize) {
    let klyuch = "C/srednie".to_string();
    let mut sd = hod.sdvig(&klyuch);
    let fayly = &c.nabory["srednie"];
    let dlit = hod.dlitelnost;
    if let Err(e) = fs::create_dir_all(papka) {
        eprintln!("не удалось создать {}: {e}", papka.display());
        return;
    }
    let stop = AtomicBool::new(false);
    let (itog, rezultat) = thread::scope(|s| {
        let pisatel = s.spawn(|| zamer::zapis_potokom(papka, &stop));
        let itog = zamer::yacheyka_chteniya(fayly, 4, dlit, &mut sd);
        stop.store(true, Ordering::SeqCst);
        (itog, pisatel.join())
    });
    let _ = fs::remove_dir_all(papka);
    let zapisano = match rezultat {
        Ok(Ok(b)) => format!("; записано {} МБ", rus(b as f64 / 1048576.0, 0)),
        Ok(Err(e)) => format!("; ЗАПИСЬ НЕ УДАЛАСЬ: {e}"),
        Err(_) => "; поток записи аварийно завершён".to_string(),
    };
    hod.sdvigi.insert(klyuch, sd);
    hod.zapisat(
        "E", &c.imya, 4, "srednie", poryadok, povtor, &itog, 4, &zapisano,
    );
}

fn opyt_e(hod: &mut Hod, diski: &[DiskOpyta]) {
    let Some(c) = diski.iter().find(|d| d.bukva == 'C') else {
        println!("Опыт E пропущен: диска C: нет в списке");
        return;
    };
    let drugoy = diski.iter().find(|d| d.bukva != 'C');
    if drugoy.is_none() {
        println!("Опыт E: диск один — запись на другой диск пропущена");
    }
    let papka_c = c.koren.join("zapis");
    let poryadok_c = format!("zapis:{}", c.imya);
    for povtor in 1..=hod.povtorov {
        if hod.nachat("E") {
            yacheyka_e(hod, c, &papka_c, &poryadok_c, povtor);
        }
    }
    if let Some(dr) = drugoy {
        let papka_dr = dr.koren.join("zapis");
        let poryadok_dr = format!("zapis:{}", dr.imya);
        for povtor in 1..=hod.povtorov {
            if hod.nachat("E") {
                yacheyka_e(hod, c, &papka_dr, &poryadok_dr, povtor);
            }
        }
    }
}

fn zagruzit_disk(bukva: char, bystro: bool) -> Result<DiskOpyta, String> {
    let kor = koren(bukva, bystro);
    if !kor.join(METKA).is_file() {
        return Err(format!(
            "{}: наборов нет ({} без файла-метки). Сначала выполните podgotovka.",
            imya_diska(bukva),
            kor.display()
        ));
    }
    let mut mapa = HashMap::new();
    for n in nabory(bystro) {
        let fayly = spisok_nabora(&kor.join(n.imya));
        if fayly.len() != n.chislo || fayly.iter().any(|f| f.razmer != n.razmer as u64) {
            return Err(format!(
                "{}: набор {} неполный ({} из {} файлов). Выполните podgotovka.",
                imya_diska(bukva),
                n.imya,
                fayly.len(),
                n.chislo
            ));
        }
        mapa.insert(n.imya, fayly);
    }
    let klass = disk_info_for_letter(bukva).class;
    Ok(DiskOpyta {
        bukva,
        imya: imya_diska(bukva),
        klass,
        koren: kor,
        nabory: mapa,
    })
}

fn put_diski_csv(vyhod: &Path) -> PathBuf {
    let stem = vyhod
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    vyhod.with_file_name(format!("{stem}-diski.csv"))
}

fn komanda_opyty(a: &Argumenty) -> u8 {
    if a.diski.is_empty() {
        eprintln!("Не указаны диски: --disk C:");
        return 2;
    }
    if a.vyhod.is_none() && !a.tolko_plan {
        eprintln!("Не указан файл результата: --vyhod файл.csv");
        return 2;
    }
    let klassy: Vec<(char, DiskClass)> = a
        .diski
        .iter()
        .map(|&d| (d, disk_info_for_letter(d).class))
        .collect();
    let formaty = zagruzit_formaty();
    if formaty.is_empty() && vybran(&a.tolko, 'D') {
        eprintln!("Предупреждение: документы tests\\proba не найдены — опыт D будет пустым");
    }
    let plan = sostavit_plan(&klassy, a.bystro, a.predel_min, formaty.len(), &a.tolko);
    napechatat_plan(&plan, &klassy, a.predel_min);
    if a.tolko_plan {
        return 0;
    }
    let Some(vyhod_put) = a.vyhod.clone() else {
        return 2;
    };

    // Наборы всех дисков.
    let mut diski = Vec::new();
    for &b in &a.diski {
        match zagruzit_disk(b, a.bystro) {
            Ok(d) => diski.push(d),
            Err(e) => {
                eprintln!("Ошибка: {e}");
                return 1;
            }
        }
    }

    // Файлы результата.
    let mut vyhod = match File::create(&vyhod_put) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Не удалось создать {}: {e}", vyhod_put.display());
            return 1;
        }
    };
    if writeln!(vyhod, "{ZAGOLOVOK}").is_err() {
        eprintln!("Не удалось записать заголовок CSV");
        return 1;
    }
    let diski_put = put_diski_csv(&vyhod_put);
    let mut diski_txt = String::from("диск,класс\n");
    for d in &diski {
        diski_txt.push_str(&format!("{},{}\n", d.imya, d.klass.name()));
    }
    if let Err(e) = fs::write(&diski_put, diski_txt) {
        eprintln!("Не удалось записать {}: {e}", diski_put.display());
        return 1;
    }

    println!();
    let nachalo = Instant::now();
    let mut hod = Hod {
        vyhod,
        nomer: 0,
        vsego: plan.vsego(),
        dlitelnost: Duration::from_secs_f64(plan.dlitelnost),
        konets: nachalo + Duration::from_secs_f64(a.predel_min * 60.0),
        ostanovleno: false,
        propusheno: BTreeMap::new(),
        povtorov: plan.povtorov,
        sdvigi: HashMap::new(),
        vytesnenie: plan.vytesnenie_na_yacheyku(),
    };
    if vybran(&a.tolko, 'A') {
        opyt_a(&mut hod, &diski, a.bystro);
    }
    if vybran(&a.tolko, 'B') {
        opyt_b(&mut hod, &diski);
    }
    if vybran(&a.tolko, 'C') {
        opyt_c(&mut hod, &diski);
    }
    if vybran(&a.tolko, 'D') {
        opyt_d(&mut hod, &formaty);
    }
    if vybran(&a.tolko, 'E') {
        opyt_e(&mut hod, &diski);
    }

    println!();
    println!(
        "Замер завершён: выполнено ячеек {} из {} за {}",
        hod.nomer,
        plan.vsego(),
        vremya_txt(nachalo.elapsed().as_secs_f64())
    );
    if hod.propusheno.is_empty() {
        println!("Пропущено: ничего");
    } else {
        let s: Vec<String> = hod
            .propusheno
            .iter()
            .map(|(o, n)| format!("опыт {o} — {n}"))
            .collect();
        println!(
            "Предел времени вышел. Пропущено ячеек: {} ({}).",
            hod.propusheno.values().sum::<usize>(),
            s.join(", ")
        );
    }
    println!("Результат: {}", vyhod_put.display());
    println!("Классы дисков: {}", diski_put.display());
    0
}

fn main() -> ExitCode {
    let args = match razobrat_argumenty() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Ошибка: {e}");
            spravka();
            return ExitCode::from(2);
        }
    };
    let kod = match args.komanda.as_str() {
        "podgotovka" => komanda_podgotovka(&args),
        "opyty" => komanda_opyty(&args),
        "ochistka" => komanda_ochistka(&args),
        inaya => {
            eprintln!("Неизвестная подкоманда: «{inaya}»");
            spravka();
            2
        }
    };
    ExitCode::from(kod)
}

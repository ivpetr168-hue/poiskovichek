//! Замер скорости индексирования папок.
//!
//! ```text
//! zamer_indeksa --istochnik <папка> [--istochnik <папка> …] --rezhim po-diskam|obshchaya
//!               [--povtorov 3] [--holodno] [--v-protsesse]
//! zamer_indeksa --proverka-holodnogo <файл>
//! ```
//!
//! Каждый прогон индексирует источники в НОВУЮ временную папку данных (на том же томе,
//! что и временная папка Windows, обычно C:) и удаляет её после прогона. Режим один на запуск:
//! `po-diskam` — читатели по физическим дискам, `obshchaya` — прежний путь, когда файлы читает
//! сам пул извлечения. `--holodno` перед каждым прогоном вытесняет файлы источников из кэша
//! Windows (открытие с `FILE_FLAG_NO_BUFFERING`). `--v-protsesse` — извлечение в самом
//! процессе, без рабочих процессов (по умолчанию как в окне программы: рабочие процессы).

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use poiskovichek::catalog::Catalog;
use poiskovichek::pipeline::{PipelineOptions, index_sources_with_report};
use poiskovichek::schetchiki::OtchetEtapov;
use poiskovichek::settings::{Settings, Source, SourceMode};
use poiskovichek::worker::find_worker_exe;

/// Флаг Windows: чтение мимо кэша.
const FILE_FLAG_NO_BUFFERING: u32 = 0x2000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rezhim {
    PoDiskam,
    Obshchaya,
}

impl Rezhim {
    fn nazvanie(self) -> &'static str {
        match self {
            Self::PoDiskam => "по дискам",
            Self::Obshchaya => "общая очередь",
        }
    }
}

struct Argumenty {
    istochniki: Vec<PathBuf>,
    rezhim: Rezhim,
    povtorov: usize,
    holodno: bool,
    v_protsesse: bool,
}

/// Результат одного прогона.
struct Progon {
    faylov: usize,
    megabayt: f64,
    sekund: f64,
    /// Время этапов конвейера за прогон.
    etapy: OtchetEtapov,
}

impl Progon {
    fn mb_v_sekundu(&self) -> f64 {
        if self.sekund > 0.0 {
            self.megabayt / self.sekund
        } else {
            0.0
        }
    }
}

const SPRAVKA: &str = "Замер скорости индексирования.\n\
Запуск: zamer_indeksa --istochnik <папка> [--istochnik <папка> …] --rezhim po-diskam|obshchaya\n\
        [--povtorov 3] [--holodno] [--v-protsesse]\n\
        zamer_indeksa --proverka-holodnogo <файл>\n\
  --istochnik   индексируемая папка (можно несколько)\n\
  --rezhim      po-diskam: читатели по физическим дискам; obshchaya: прежний путь\n\
  --povtorov    сколько прогонов сделать (по умолчанию 3)\n\
  --holodno     перед каждым прогоном вытеснить файлы источников из кэша Windows\n\
  --v-protsesse извлекать в самом процессе (по умолчанию — рабочие процессы, как в окне)\n\
  После каждого прогона печатается таблица времени этапов (обход, сверка, чтение, разбор,
  писатель: база и индекс, ожидание в очередях) и строка «узкое место: …»
  --proverka-holodnogo  проверить на файле, что вытеснение из кэша работает";

fn razobrat(args: &[String]) -> Result<Argumenty, String> {
    let mut istochniki = Vec::new();
    let mut rezhim = None;
    let mut povtorov = 3usize;
    let mut holodno = false;
    let mut v_protsesse = false;
    let mut i = 0;
    while i < args.len() {
        let nazvanie = args[i].as_str();
        let znachenie = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("у ключа {nazvanie} нет значения"))
        };
        match nazvanie {
            "--istochnik" => istochniki.push(PathBuf::from(znachenie(&mut i)?)),
            "--rezhim" => {
                rezhim = Some(match znachenie(&mut i)?.as_str() {
                    "po-diskam" => Rezhim::PoDiskam,
                    "obshchaya" => Rezhim::Obshchaya,
                    drugoe => {
                        return Err(format!(
                            "неизвестный режим «{drugoe}», нужен po-diskam или obshchaya"
                        ));
                    }
                });
            }
            "--povtorov" => {
                let text = znachenie(&mut i)?;
                povtorov = text
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n >= 1)
                    .ok_or_else(|| {
                        format!("--povtorov: нужно целое число от 1, получено «{text}»")
                    })?;
            }
            "--holodno" => holodno = true,
            "--v-protsesse" => v_protsesse = true,
            drugoe => return Err(format!("неизвестный ключ «{drugoe}»")),
        }
        i += 1;
    }
    if istochniki.is_empty() {
        return Err("не указан ни один --istochnik".to_string());
    }
    let rezhim =
        rezhim.ok_or_else(|| "не указан --rezhim (po-diskam или obshchaya)".to_string())?;
    Ok(Argumenty {
        istochniki,
        rezhim,
        povtorov,
        holodno,
        v_protsesse,
    })
}

/// Все файлы папки (рекурсивно). Ссылки и недоступные папки пропускаются.
fn sobrat_fayly(papka: &Path, vyhod: &mut Vec<PathBuf>) {
    let Ok(zapisi) = fs::read_dir(papka) else {
        return;
    };
    for zapis in zapisi.flatten() {
        let Ok(tip) = zapis.file_type() else {
            continue;
        };
        if tip.is_dir() {
            sobrat_fayly(&zapis.path(), vyhod);
        } else if tip.is_file() {
            vyhod.push(zapis.path());
        }
    }
}

/// Открывает файл мимо кэша и сразу закрывает: Windows сбрасывает кэшированные страницы файла.
/// Возвращает `true`, если открыть удалось.
fn vytesnit_iz_kesha(put: &Path) -> bool {
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_NO_BUFFERING)
        .open(put)
        .is_ok()
}

/// Вытесняет из кэша все файлы источников. Возвращает (удалось, не удалось).
fn vytesnit_istochniki(fayly: &[PathBuf]) -> (usize, usize) {
    let mut udalos = 0;
    for put in fayly {
        if vytesnit_iz_kesha(put) {
            udalos += 1;
        }
    }
    (udalos, fayly.len() - udalos)
}

fn mediana(znacheniya: &[f64]) -> f64 {
    let mut sortirovannye = znacheniya.to_vec();
    sortirovannye.sort_by(|a, b| a.total_cmp(b));
    let n = sortirovannye.len();
    if n == 0 {
        0.0
    } else if n % 2 == 1 {
        sortirovannye[n / 2]
    } else {
        (sortirovannye[n / 2 - 1] + sortirovannye[n / 2]) / 2.0
    }
}

/// Число с запятой и нужным числом знаков.
fn chislo(x: f64, znakov: usize) -> String {
    format!("{x:.znakov$}").replace('.', ",")
}

/// Новая временная папка данных для прогона.
fn novaya_papka_dannykh(nomer: usize) -> PathBuf {
    std::env::temp_dir().join(format!(
        "poiskovichek-zamer-indeksa-{}-{nomer}",
        std::process::id()
    ))
}

fn odin_progon(args: &Argumenty, nomer: usize) -> Result<Progon, String> {
    let papka = novaya_papka_dannykh(nomer);
    if papka.exists() {
        fs::remove_dir_all(&papka).map_err(|e| format!("не удалось очистить {papka:?}: {e}"))?;
    }
    let settings = Settings {
        data_dir: papka.clone(),
        // словарь морфологии не участвует: замер читает и разбирает файлы, не склоняет слова
        enable_morphology: false,
        ..Default::default()
    };
    let istochniki: Vec<Source> = args
        .istochniki
        .iter()
        .map(|put| Source {
            path: put.clone(),
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        })
        .collect();
    let options = PipelineOptions {
        po_diskam: args.rezhim == Rezhim::PoDiskam,
        in_process_workers: args.v_protsesse,
        ..Default::default()
    };

    let nachalo = Instant::now();
    let itog = index_sources_with_report(&settings, &istochniki, options);
    let sekund = nachalo.elapsed().as_secs_f64();

    let rezultat = match itog {
        Ok((_, etapy)) => {
            // Число и размер файлов считаем по базе, уже вне замера времени
            let catalog = Catalog::open(&papka.join("catalog.sqlite3"))
                .map_err(|e| format!("не удалось открыть базу прогона: {e}"))?;
            let snimok = catalog
                .snapshot()
                .map_err(|e| format!("не удалось прочитать базу прогона: {e}"))?;
            let mut faylov = 0usize;
            let mut bayt = 0u64;
            for zapis in snimok.values().filter(|z| z.container_id.is_none()) {
                faylov += 1;
                bayt += zapis.size;
            }
            Ok(Progon {
                faylov,
                megabayt: bayt as f64 / (1024.0 * 1024.0),
                sekund,
                etapy,
            })
        }
        Err(e) => Err(format!("индексирование завершилось ошибкой: {e}")),
    };

    // Папка данных одноразовая: удаляем в любом случае (даём время закрыться файлам индекса)
    let mut ostatok = None;
    for popytka in 0..5 {
        match fs::remove_dir_all(&papka) {
            Ok(()) => {
                ostatok = None;
                break;
            }
            Err(e) => {
                ostatok = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(200 * (popytka + 1)));
            }
        }
    }
    if let Some(e) = ostatok {
        eprintln!("Предупреждение: не удалось удалить {papka:?}: {e}");
    }
    rezultat
}

fn rabota(args: &Argumenty) -> Result<(), String> {
    let temp = std::env::temp_dir();
    println!("Режим: {}", args.rezhim.nazvanie());
    for put in &args.istochniki {
        if !put.is_dir() {
            return Err(format!(
                "источник не найден или не папка: {}",
                put.display()
            ));
        }
        println!("Источник: {}", put.display());
    }
    println!("Временные папки данных: {}", temp.display());
    println!(
        "Извлечение: {}",
        if args.v_protsesse {
            "в процессе замера"
        } else {
            "рабочие процессы"
        }
    );
    if !args.v_protsesse && !find_worker_exe().exists() {
        return Err(
            "не найден Poiskovichek.exe для рабочих процессов: соберите проект (cargo build) \
             или укажите --v-protsesse"
                .to_string(),
        );
    }

    let mut fayly = Vec::new();
    if args.holodno {
        for put in &args.istochniki {
            sobrat_fayly(put, &mut fayly);
        }
        println!(
            "Холодный режим: перед каждым прогоном вытесняю из кэша {} файлов",
            fayly.len()
        );
    }

    let mut progony: Vec<Progon> = Vec::new();
    for nomer in 1..=args.povtorov {
        if args.holodno {
            let nachalo = Instant::now();
            let (udalos, ne_udalos) = vytesnit_istochniki(&fayly);
            println!(
                "Прогон {nomer}: кэш сброшен ({udalos} файлов, не открылось {ne_udalos}, {} с)",
                chislo(nachalo.elapsed().as_secs_f64(), 1)
            );
        }
        let progon = odin_progon(args, nomer)?;
        println!(
            "Прогон {nomer} из {}: файлов {}, {} МБ, {} с, {} МБ/с",
            args.povtorov,
            progon.faylov,
            chislo(progon.megabayt, 1),
            chislo(progon.sekund, 2),
            chislo(progon.mb_v_sekundu(), 1),
        );
        println!("{}", progon.etapy.tablica());
        if progon.etapy.formaty.formaty.is_empty() {
            println!("Разбор по форматам: файлов не разбирали");
        } else {
            println!("Разбор по форматам (время внутри submit_task):");
            println!("{}", progon.etapy.formaty.tablica_formatov(15));
            println!("{}", progon.etapy.formaty.tablica_dolgih());
        }
        progony.push(progon);
    }

    let skorosti: Vec<f64> = progony.iter().map(Progon::mb_v_sekundu).collect();
    let sekundy: Vec<f64> = progony.iter().map(|p| p.sekund).collect();
    println!(
        "Медиана по {} прогонам: {} МБ/с, {} с",
        progony.len(),
        chislo(mediana(&skorosti), 1),
        chislo(mediana(&sekundy), 2)
    );
    Ok(())
}

/// Читает файл целиком обычным способом и возвращает секунды.
fn prochitat_obychno(put: &Path) -> Result<f64, String> {
    let nachalo = Instant::now();
    let mut file = fs::File::open(put).map_err(|e| format!("не удалось открыть: {e}"))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| format!("не удалось прочитать: {e}"))?;
    Ok(nachalo.elapsed().as_secs_f64())
}

/// Проверка вытеснения из кэша: чтение, повторное чтение (из кэша), сброс, чтение после сброса.
fn proverka_holodnogo(put: &Path) -> Result<(), String> {
    let pervoe = prochitat_obychno(put)?;
    let vtoroe = prochitat_obychno(put)?;
    if !vytesnit_iz_kesha(put) {
        return Err("файл не открылся с FILE_FLAG_NO_BUFFERING".to_string());
    }
    let posle_sbrosa = prochitat_obychno(put)?;
    println!("Файл: {}", put.display());
    println!("1. Первое чтение:            {} с", chislo(pervoe, 3));
    println!("2. Повторное (из кэша):      {} с", chislo(vtoroe, 3));
    println!("3. После сброса кэша:        {} с", chislo(posle_sbrosa, 3));
    if posle_sbrosa > vtoroe * 4.0 && posle_sbrosa > 0.02 {
        println!("Итог: сброс работает, после него чтение идёт с диска.");
    } else {
        println!("Итог: сброс НЕ сработал, чтение после него осталось из кэша.");
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{SPRAVKA}");
        return ExitCode::SUCCESS;
    }
    if args[0] == "--proverka-holodnogo" {
        let Some(put) = args.get(1) else {
            eprintln!("Ошибка: у --proverka-holodnogo нет пути к файлу");
            return ExitCode::from(2);
        };
        return match proverka_holodnogo(Path::new(put)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("Ошибка: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let razobrannye = match razobrat(&args) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Ошибка: {e}\n\n{SPRAVKA}");
            return ExitCode::from(2);
        }
    };
    match rabota(&razobrannye) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Ошибка: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn razbor_polnogo_zapuska() {
        let a = razobrat(&s(&[
            "--istochnik",
            "C:\\a",
            "--istochnik",
            "C:\\b",
            "--rezhim",
            "obshchaya",
            "--povtorov",
            "5",
            "--holodno",
        ]))
        .expect("разбор");
        assert_eq!(a.istochniki.len(), 2);
        assert_eq!(a.rezhim, Rezhim::Obshchaya);
        assert_eq!(a.povtorov, 5);
        assert!(a.holodno && !a.v_protsesse);
    }

    #[test]
    fn razbor_oshibki() {
        assert!(razobrat(&s(&["--rezhim", "po-diskam"])).is_err());
        assert!(razobrat(&s(&["--istochnik", "C:\\a"])).is_err());
        assert!(razobrat(&s(&["--istochnik", "C:\\a", "--rezhim", "xxx"])).is_err());
        assert!(
            razobrat(&s(&[
                "--istochnik",
                "C:\\a",
                "--rezhim",
                "po-diskam",
                "--povtorov",
                "0"
            ]))
            .is_err()
        );
    }

    #[test]
    fn mediana_chetnaya_i_nechetnaya() {
        assert_eq!(mediana(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(mediana(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(mediana(&[]), 0.0);
    }
}

//! Проверка программы замера дисков в быстром режиме (`--bystro`).
//!
//! Отдельная тестовая программа (не входит в tests\vse): использует
//! `env!("CARGO_BIN_EXE_zamer_diskov")` и запускает собранный exe как внешний процесс.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PREDEL: Duration = Duration::from_secs(180);

/// Запускает программу замера; предел времени — общий для всего теста.
fn zapustit(argumenty: &[&str], nachalo: Instant) -> (i32, String) {
    let mut ditya = Command::new(env!("CARGO_BIN_EXE_zamer_diskov"))
        .args(argumenty)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("не удалось запустить zamer_diskov");
    let stdout = ditya.stdout.take().expect("stdout");
    let stderr = ditya.stderr.take().expect("stderr");
    let chitat = |mut r: Box<dyn std::io::Read + Send>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            let _ = r.read_to_end(&mut b);
            String::from_utf8_lossy(&b).into_owned()
        })
    };
    let o = chitat(Box::new(stdout));
    let e = chitat(Box::new(stderr));
    let kod = loop {
        if let Some(status) = ditya.try_wait().expect("try_wait") {
            break status.code().unwrap_or(-1);
        }
        if nachalo.elapsed() > PREDEL {
            let _ = ditya.kill();
            let _ = ditya.wait();
            panic!("превышен предел теста 3 мин на команде {argumenty:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let vyvod = format!(
        "{}{}",
        o.join().unwrap_or_default(),
        e.join().unwrap_or_default()
    );
    (kod, vyvod)
}

fn proverit_csv(put: &Path) {
    let tekst = std::fs::read_to_string(put).expect("CSV не создан");
    let mut stroki = tekst.lines();
    assert_eq!(
        stroki.next(),
        Some("opyt,diski,k,nabor,poryadok,povtor_n,faylov,bayt,sekund,kesh,povtor,potokov"),
        "неверный заголовок CSV"
    );
    let dannye: Vec<&str> = stroki.collect();
    assert!(!dannye.is_empty(), "в CSV нет строк с данными");
    for s in &dannye {
        assert_eq!(s.split(',').count(), 12, "в строке не 12 столбцов: {s}");
    }
    let diski = put.with_file_name(format!(
        "{}-diski.csv",
        put.file_stem().unwrap().to_string_lossy()
    ));
    let d = std::fs::read_to_string(&diski).expect("файл классов дисков не создан");
    assert!(
        d.starts_with("диск,класс"),
        "неверный заголовок файла дисков: {d}"
    );
    assert!(d.lines().count() >= 2, "в файле дисков нет строк");
}

#[test]
fn bystryj_zamer_vypolnyaetsya_do_konca() {
    let nachalo = Instant::now();
    let papka = tempfile::tempdir().expect("временная папка");
    let csv = papka.path().join("zamery.csv");
    let csv_s = csv.to_string_lossy().into_owned();

    let (kod, vyvod) = zapustit(&["podgotovka", "--bystro"], nachalo);
    assert_eq!(kod, 0, "podgotovka: {vyvod}");

    let (kod_opytov, vyvod_opytov) = zapustit(&["opyty", "--bystro", "--vyhod", &csv_s], nachalo);

    // Очистка выполняется всегда, даже если опыты упали.
    let (kod_ochistki, vyvod_ochistki) = zapustit(&["ochistka", "--bystro"], nachalo);

    assert_eq!(kod_opytov, 0, "opyty: {vyvod_opytov}");
    assert_eq!(kod_ochistki, 0, "ochistka: {vyvod_ochistki}");
    proverit_csv(&csv);
}

#[test]
fn tolko_vybiraet_opyty_v_plane() {
    let nachalo = Instant::now();
    let (kod, vyvod) = zapustit(
        &["opyty", "--disk", "D:", "--tolko", "B", "--tolko-plan"],
        nachalo,
    );
    assert_eq!(kod, 0, "plan: {vyvod}");
    for o in ["A", "C", "D", "E"] {
        let stroka = vyvod
            .lines()
            .find(|s| s.trim_start().starts_with(&format!("Опыт {o} ")))
            .unwrap_or_else(|| panic!("нет строки опыта {o}: {vyvod}"));
        assert!(
            stroka.contains("не выбран"),
            "опыт {o} должен быть пропущен: {stroka}"
        );
    }
    let b = vyvod
        .lines()
        .find(|s| s.trim_start().starts_with("Опыт B "))
        .expect("нет строки опыта B");
    assert!(!b.contains("не выбран"), "опыт B должен быть в плане: {b}");
    assert!(vyvod.contains("Всего ячеек:"), "{vyvod}");

    let (kod, vyvod) = zapustit(
        &["opyty", "--disk", "C:", "--tolko", "a,c", "--tolko-plan"],
        nachalo,
    );
    assert_eq!(kod, 0, "plan A,C: {vyvod}");
    assert!(
        vyvod
            .lines()
            .any(|s| s.trim_start().starts_with("Опыт B ") && s.contains("не выбран")),
        "{vyvod}"
    );
}

#[test]
fn tolko_s_oshibkoy_otklonyaetsya() {
    let nachalo = Instant::now();
    let (kod, vyvod) = zapustit(
        &["opyty", "--disk", "C:", "--tolko", "X", "--tolko-plan"],
        nachalo,
    );
    assert_eq!(kod, 2, "{vyvod}");
    assert!(vyvod.contains("неизвестный опыт"), "{vyvod}");
}

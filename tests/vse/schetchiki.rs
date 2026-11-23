//! Счётчики времени этапов конвейера: после индексирования маленькой папки
//! счётчики всех этапов, которые были, ненулевые.

use std::fs;

use tempfile::tempdir;

use poiskovichek::pipeline::{PipelineOptions, index_sources_with_report};
use poiskovichek::schetchiki::OtchetEtapov;
use poiskovichek::settings::{Settings, Source, SourceMode};

/// Индексирует папку из 30 текстовых файлов и возвращает отчёт об этапах.
fn proindeksirovat(po_diskam: bool, v_protsesse: bool) -> OtchetEtapov {
    let korenj = tempdir().expect("tempdir");
    let istochnik = korenj.path().join("source");
    fs::create_dir_all(istochnik.join("vlozhennaya")).expect("папка");
    for i in 0..30 {
        fs::write(
            istochnik.join(format!("f_{i:03}.txt")),
            format!("документ {i:03} со словами для счётчиков этапов номер {i}"),
        )
        .expect("файл");
    }
    fs::write(
        istochnik.join("vlozhennaya").join("glubokiy.txt"),
        "вложенный документ для счётчиков",
    )
    .expect("вложенный");
    let settings = Settings {
        data_dir: korenj.path().join("data"),
        enable_morphology: false,
        ..Default::default()
    };
    let sources = vec![Source {
        path: istochnik,
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    }];
    let options = PipelineOptions {
        po_diskam,
        in_process_workers: v_protsesse,
        ..Default::default()
    };
    let (itog, otchet) =
        index_sources_with_report(&settings, &sources, options).expect("индексирование");
    assert_eq!(itog.added, 31);
    otchet
}

/// Этапы, общие для всех режимов, ненулевые.
fn proverit_obshchie(o: &OtchetEtapov) {
    assert!(o.wall_ns > 0, "время прогона");
    assert_eq!(o.obhod_potokov, 1, "потоков обхода");
    assert!(o.obhod_ns > 0, "обход");
    assert_eq!(o.obhod_faylov, 31, "файлов в обходе");
    assert!(o.sverka_ns > 0, "сверка");
    assert_eq!(o.sverka_faylov, 31, "файлов в сверке");
    assert!(o.razbor_potokov > 0, "потоков разбора");
    assert!(o.razbor_ns > 0, "разбор");
    assert_eq!(o.razbor_faylov, 31, "файлов в разборе");
    assert_eq!(o.pisatel_zapisey, 31, "записей писателя");
    assert!(o.pisatel_podgotovka_ns > 0, "подготовка записей");
    assert!(o.pisatel_baza_ns > 0, "база");
    assert!(o.pisatel_indeks_dobavlenie_ns > 0, "добавление в индекс");
    assert_eq!(o.pisatel_dokumentov, 31, "документов в индексе");
    assert!(o.pisatel_fiksaciya_ns > 0, "фиксация индекса");
    assert!(o.pisatel_fiksaciy >= 1, "число фиксаций");
    // строка о узком месте и таблица строятся без падений и называют этап
    let t = o.tablica();
    assert!(t.contains("узкое место:"), "{t}");
    assert!(o.zanyatost_etapov().len() >= 4);
}

#[test]
fn schetchiki_po_diskam_v_protsesse() {
    let o = proindeksirovat(true, true);
    proverit_obshchie(&o);
    assert!(o.chtenie_potokov > 0, "потоков чтения");
    assert!(o.chtenie_ns > 0, "чтение");
    assert_eq!(o.chtenie_faylov, 31, "файлов прочитано читателями");
    assert!(o.chtenie_bayt > 0);
    assert_eq!(o.protsess_zadach, 0, "рабочих процессов не было");
}

#[test]
fn schetchiki_obshchaya_ochered_chtenie_ne_byvalo() {
    let o = proindeksirovat(false, true);
    proverit_obshchie(&o);
    assert_eq!(o.chtenie_potokov, 0, "читателей в общей очереди нет");
    assert_eq!(o.chtenie_ns, 0);
    assert_eq!(o.chtenie_faylov, 0);
}

#[test]
fn schetchiki_rabochie_processy_pokazyvayut_peredachu_dannykh() {
    let o = proindeksirovat(true, false);
    proverit_obshchie(&o);
    assert_eq!(o.protsess_zadach, 31, "заданий передано процессам");
    assert!(o.protsess_serializ_ns > 0, "сериализация");
    assert!(o.protsess_zapis_ns > 0, "запись в канал");
    assert!(o.protsess_otvet_ns > 0, "ожидание ответа");
    assert!(o.protsess_razbor_otveta_ns > 0, "разбор ответа");
    assert!(o.protsess_rabochiy_izvlechenie_ns > 0, "работа процесса");
    assert!(
        o.protsess_rabochiy_dekodirovanie_ns > 0,
        "чтение задания в процессе"
    );
    assert!(o.protsess_bayt > 0);
    let t = o.tablica();
    assert!(t.contains("сериализация задания"), "{t}");
}

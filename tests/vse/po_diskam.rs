//! Читатели по физическим дискам (`PipelineOptions::po_diskam`): результат индексирования
//! не должен отличаться от прежнего пути, когда файлы читает сам пул извлечения.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use tempfile::tempdir;

use poiskovichek::catalog::Catalog;
use poiskovichek::index::reader;
use poiskovichek::pipeline::{PipelineOptions, sync_all_with_options};
use poiskovichek::settings::{Settings, Source, SourceMode};
use tantivy::query::QueryParser;

/// Слова, которые должны находиться: (слово, сколько документов).
const ISKOMYE: &[(&str, usize)] = &[
    ("слово_мелкого_текста_ноль", 5),
    ("слово_мелкого_текста_семь", 5),
    ("слово_разметки_html_альфа", 1),
    ("слово_заметки_md_бета", 1),
    ("слово_таблицы_csv_гамма", 1),
    ("слово_крупного_файла_дельта", 1),
    ("слово_очень_крупного_файла_эпсилон", 1),
    ("слово_вложенной_папки_зета", 1),
    // эти слова лежат в файлах, содержимое которых индексироваться не должно
    ("слово_закрытого_файла_эта", 0),
    ("слово_старого_формата_тета", 0),
];

/// Состав проверочной папки: разные размеры и форматы, в том числе файл больше 16 МБ,
/// закрытый файл, неподдерживаемый формат и испорченный PDF.
fn sozdat_istochnik(papka: &Path) {
    fs::create_dir_all(papka.join("vlozhennaya")).expect("папка");
    for i in 0..40 {
        let slova = [
            "ноль",
            "один",
            "два",
            "три",
            "четыре",
            "пять",
            "шесть",
            "семь",
        ];
        fs::write(
            papka.join(format!("melkiy_{i:03}.txt")),
            format!(
                "слово_мелкого_текста_{} номер {i} общий_текст_для_всех",
                slova[i % 8]
            ),
        )
        .expect("txt");
    }
    fs::write(
        papka.join("razmetka.html"),
        "<html><body><p>слово_разметки_html_альфа</p></body></html>",
    )
    .expect("html");
    fs::write(
        papka.join("zametka.md"),
        "# заметка\nслово_заметки_md_бета\n",
    )
    .expect("md");
    fs::write(
        papka.join("tablica.csv"),
        "a;b\nслово_таблицы_csv_гамма;2\n",
    )
    .expect("csv");
    fs::write(
        papka.join("vlozhennaya").join("glubokiy.txt"),
        "слово_вложенной_папки_зета",
    )
    .expect("вложенный");
    // 6 МБ текста: читается по пределу символов (начало файла).
    // Заполнитель латиницей: при обрезке посреди русской буквы прежний разбор кодировки
    // принял бы весь кусок за CP1251 (см. отчёт), а здесь проверяется не он
    let mut krupnyy = String::from("слово_крупного_файла_дельта\n");
    while krupnyy.len() < 6 * 1024 * 1024 {
        krupnyy.push_str("repeating filler line for the volume\n");
    }
    fs::write(papka.join("krupnyy.txt"), krupnyy).expect("крупный");
    // больше 16 МБ: читатель его не берёт, извлечение читает само
    let mut ochen = String::from("слово_очень_крупного_файла_эпсилон\n");
    while ochen.len() < 17 * 1024 * 1024 {
        ochen.push_str("repeating filler line for the volume\n");
    }
    fs::write(papka.join("ochen_krupnyy.txt"), ochen).expect("очень крупный");
    // закрытый файл и неподдерживаемый формат
    fs::write(papka.join(".env"), "слово_закрытого_файла_эта=1").expect("env");
    fs::write(papka.join("staryy.doc"), "слово_старого_формата_тета").expect("doc");
    // испорченный PDF и двоичный файл
    fs::write(papka.join("isporchennyy.pdf"), b"%PDF-1.4 not really a pdf").expect("pdf");
    fs::write(papka.join("dvoichnyy.bin"), [0u8; 64]).expect("bin");
    fs::write(papka.join("pustoy.txt"), b"").expect("пустой");
}

struct Itog {
    dobavleno: usize,
    zapisey: usize,
    /// путь (относительно источника) -> (статус, sha текста)
    fayly: BTreeMap<String, (Option<String>, Option<String>)>,
    nayideno: BTreeMap<&'static str, usize>,
}

fn proindeksirovat(istochnik: &Path, po_diskam: bool, v_protsesse: bool) -> Itog {
    let korenj = tempdir().expect("tempdir");
    let data_dir = korenj.path().join("data");
    let settings = Settings {
        data_dir: data_dir.clone(),
        enable_morphology: false,
        sources: vec![Source {
            path: istochnik.to_path_buf(),
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        ..Default::default()
    };
    let options = PipelineOptions {
        in_process_workers: v_protsesse,
        po_diskam,
        ..Default::default()
    };
    let summary = sync_all_with_options(&settings, options).expect("индексирование");

    let catalog = Catalog::open(&data_dir.join("catalog.sqlite3")).expect("база");
    let zapisey = catalog.count_files().expect("счёт");
    let prefiks = format!("{}\\", istochnik.to_string_lossy().trim_end_matches('\\'));
    let mut fayly = BTreeMap::new();
    {
        let mut stmt = catalog
            .connection()
            .prepare("SELECT path, content_status, text_sha FROM files")
            .expect("запрос");
        let stroki = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })
            .expect("строки");
        for stroka in stroki {
            let (put, status, sha) = stroka.expect("строка");
            let otnositelnyy = put.strip_prefix(&prefiks).unwrap_or(&put).to_string();
            fayly.insert(otnositelnyy, (status, sha));
        }
    }

    let rdr = reader(&data_dir.join("index"), None).expect("читатель индекса");
    let searcher = rdr.searcher();
    let body = searcher.schema().get_field("body").expect("поле body");
    let parser = QueryParser::for_index(searcher.index(), vec![body]);
    let mut nayideno = BTreeMap::new();
    for (slovo, _) in ISKOMYE {
        let q = parser.parse_query(slovo).expect("запрос поиска");
        let n = searcher
            .search(&q, &tantivy::collector::Count)
            .expect("поиск");
        nayideno.insert(*slovo, n);
    }
    Itog {
        dobavleno: summary.added,
        zapisey,
        fayly,
        nayideno,
    }
}

fn proverit_ozhidaemoe(itog: &Itog, nazvanie: &str) {
    for (slovo, ozhidalos) in ISKOMYE {
        assert_eq!(
            itog.nayideno[slovo], *ozhidalos,
            "{nazvanie}: слово «{slovo}» найдено неверное число раз"
        );
    }
    assert_eq!(
        itog.zapisey, itog.dobavleno,
        "{nazvanie}: записей и добавлено"
    );
}

#[test]
fn po_diskam_i_obshchaya_ochered_dayut_odinakovyy_rezultat() {
    let istochnik = tempdir().expect("tempdir");
    sozdat_istochnik(istochnik.path());

    let po_diskam = proindeksirovat(istochnik.path(), true, true);
    let obshchaya = proindeksirovat(istochnik.path(), false, true);

    proverit_ozhidaemoe(&po_diskam, "по дискам");
    proverit_ozhidaemoe(&obshchaya, "общая очередь");
    assert!(po_diskam.zapisey >= 50, "ожидалось не менее 50 файлов");

    assert_eq!(po_diskam.zapisey, obshchaya.zapisey, "число файлов");
    assert_eq!(
        po_diskam.dobavleno, obshchaya.dobavleno,
        "число добавленных"
    );
    assert_eq!(po_diskam.nayideno, obshchaya.nayideno, "найденные слова");
    assert_eq!(
        po_diskam.fayly, obshchaya.fayly,
        "статусы и тексты файлов должны совпадать"
    );

    // закрытый файл не индексируется ни в одном режиме
    let (status, sha) = &po_diskam.fayly[".env"];
    assert_eq!(status.as_deref(), Some("sensitive-metadata-only"));
    assert!(sha.is_none());
}

/// Тот же результат при извлечении рабочими процессами: байты доходят до них целыми
/// (передача сырыми байтами после заголовка) и файл повторно не читается.
#[test]
fn po_diskam_s_rabochimi_processami() {
    let istochnik = tempdir().expect("tempdir");
    sozdat_istochnik(istochnik.path());

    let v_processah = proindeksirovat(istochnik.path(), true, false);
    let v_potokakh = proindeksirovat(istochnik.path(), true, true);
    proverit_ozhidaemoe(&v_processah, "рабочие процессы");
    assert_eq!(v_processah.fayly, v_potokakh.fayly);
    assert_eq!(v_processah.nayideno, v_potokakh.nayideno);
}

/// Отмена посреди работы не вешает конвейер ни в одном из режимов,
/// а повторный запуск доводит индекс до полного состава.
#[test]
fn otmena_ne_zavisaet_v_oboikh_rezhimakh() {
    use poiskovichek::pipeline::{CancelToken, Progress};
    use std::time::Duration;

    for po_diskam in [true, false] {
        let korenj = tempdir().expect("tempdir");
        let istochnik = korenj.path().join("source");
        fs::create_dir_all(&istochnik).expect("папка");
        for i in 0..800 {
            fs::write(
                istochnik.join(format!("f_{i:04}.txt")),
                format!("документ {i:04} для проверки отмены"),
            )
            .expect("файл");
        }
        let settings = Settings {
            data_dir: korenj.path().join("data"),
            enable_morphology: false,
            sources: vec![Source {
                path: istochnik.clone(),
                mode: SourceMode::Full,
                max_depth: 0,
                enabled: true,
                volume_serial: None,
                extra_excludes: Vec::new(),
            }],
            ..Default::default()
        };

        let otmena = CancelToken::new();
        let (tx, rx) = crossbeam_channel::bounded::<Progress>(100);
        let otmena_potok = otmena.clone();
        std::thread::spawn(move || {
            while let Ok(p) = rx.recv() {
                if p.written >= 50 {
                    otmena_potok.cancel();
                    break;
                }
            }
        });
        let nastroyki = settings.clone();
        let (itog_tx, itog_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let options = PipelineOptions {
                queue_capacity: 4,
                in_process_workers: true,
                cancel_token: Some(otmena),
                progress_tx: Some(tx),
                po_diskam,
                ..Default::default()
            };
            let _ = itog_tx.send(sync_all_with_options(&nastroyki, options));
        });
        itog_rx
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("конвейер завис при отмене (po_diskam = {po_diskam})"))
            .expect("отменённый запуск завершается без ошибки");

        let options = PipelineOptions {
            in_process_workers: true,
            po_diskam,
            ..Default::default()
        };
        sync_all_with_options(&settings, options).expect("повторный запуск");
        let catalog = Catalog::open(&settings.data_dir.join("catalog.sqlite3")).expect("база");
        assert_eq!(
            catalog.count_files().expect("счёт"),
            800,
            "после повторного запуска должно быть 800 файлов (po_diskam = {po_diskam})"
        );
    }
}

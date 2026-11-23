use std::path::Path;
use std::time::Instant;

use poiskovichek::catalog::FileRecord;
use poiskovichek::morph::Morph;
use poiskovichek::names::{NameFilter, NamesIndex, SearchMode};
use poiskovichek::query::{WordFormsMode, parse_query, parse_query_forms};

fn load_morph() -> Morph {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let morph_path = manifest_dir.join("assets").join("morph.fst");
    Morph::open(morph_path).expect("не удалось открыть assets/morph.fst")
}

fn sample_file(
    id: i64,
    path: &str,
    name: &str,
    ext: &str,
    category: &str,
    size: u64,
    available: bool,
) -> FileRecord {
    FileRecord {
        id: Some(id),
        path: path.to_string(),
        name: name.to_string(),
        extension: ext.to_string(),
        size,
        modified_ns: 1_700_000_000_000_000_000,
        created_ns: 1_700_000_000_000_000_000,
        category: category.to_string(),
        inside_archive: false,
        container_id: None,
        content_status: Some("text".to_string()),
        extraction_version: 1,
        text_sha: Some(format!("sha_{id}")),
        quick_hash: None,
        sha256: None,
        error: None,
        scan_run: Some(1),
        source_id: None,
        volume_id: None,
        available,
        rel_path: name.to_string(),
    }
}

#[test]
fn path_and_name_cannot_supply_different_words() {
    // Перенос R2-В-26: запрет склейки фразы из разных полей
    let morph = load_morph();
    let mut index = NamesIndex::new();

    // 1. "сельские дороги" в имени, "D:\мужики\файл.fb2" в пути
    let f1 = sample_file(
        1,
        "D:\\мужики\\файл.fb2",
        "сельские дороги",
        "fb2",
        "книга",
        1000,
        true,
    );
    // 2. Фраза целиком в пути
    let f2 = sample_file(
        2,
        "D:\\книги\\сельские мужики.fb2",
        "точная фраза",
        "fb2",
        "книга",
        1000,
        true,
    );

    index.add_file(&f1, Some(&morph));
    index.add_file(&f2, Some(&morph));

    let query = parse_query("сельские мужики", Some(&morph), true);
    let hits = index.search(&query, &NameFilter::default(), SearchMode::StrictPhrase);

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 2);
    assert_eq!(hits[0].path, "D:\\книги\\сельские мужики.fb2");
}

#[test]
fn morph_word_forms_and_order_in_names() {
    // Требование: «сельскими мужиками» находит «Сельские мужики» (настоящий assets\morph.fst),
    // «мужики сельские» — нет (R2-В-23)
    let morph = load_morph();
    let mut index = NamesIndex::new();

    let f1 = sample_file(
        1,
        "C:\\docs\\1.txt",
        "Сельские мужики",
        "txt",
        "документ",
        500,
        true,
    );
    let f2 = sample_file(
        2,
        "C:\\docs\\2.txt",
        "мужики сельские",
        "txt",
        "документ",
        500,
        true,
    );

    index.add_file(&f1, Some(&morph));
    index.add_file(&f2, Some(&morph));

    let query = parse_query("сельскими мужиками", Some(&morph), true);
    let hits = index.search(&query, &NameFilter::default(), SearchMode::StrictPhrase);

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 1);
    assert_eq!(hits[0].name, "Сельские мужики");

    // Обратный запрос «мужики сельские» находит f2, но не f1
    let rev_query = parse_query("мужики сельские", Some(&morph), true);
    let rev_hits = index.search(&rev_query, &NameFilter::default(), SearchMode::StrictPhrase);
    assert_eq!(rev_hits.len(), 1);
    assert_eq!(rev_hits[0].id, 2);
}

#[test]
fn search_modes_strict_phrase_vs_all_words() {
    let morph = load_morph();
    let mut index = NamesIndex::new();

    let f1 = sample_file(
        1,
        "D:\\docs\\report.txt",
        "мужики и сельские дороги",
        "txt",
        "документ",
        200,
        true,
    );
    let f2 = sample_file(
        2,
        "D:\\docs\\split.txt",
        "мужики",
        "txt",
        "документ",
        200,
        true,
    );
    // f2 имеет слово в пути "D:\сельские\split.txt"
    let mut f2_split = f2;
    f2_split.path = "D:\\сельские\\split.txt".to_string();

    index.add_file(&f1, Some(&morph));
    index.add_file(&f2_split, Some(&morph));

    let query = parse_query("сельские мужики", Some(&morph), true);

    // В режиме StrictPhrase f1 не подходит (разрыв 'и'), f2_split не подходит (слова в разных полях)
    let strict_hits = index.search(&query, &NameFilter::default(), SearchMode::StrictPhrase);
    assert!(strict_hits.is_empty());

    // В режиме AllWords f1 подходит (оба слова в имени), а f2_split по-прежнему НЕ подходит
    // (нельзя брать часть слов из пути, часть из имени)
    let all_hits = index.search(&query, &NameFilter::default(), SearchMode::AllWords);
    assert_eq!(all_hits.len(), 1);
    assert_eq!(all_hits[0].id, 1);
}

#[test]
fn filter_extensions_with_and_without_dot() {
    let mut index = NamesIndex::new();

    let f1 = sample_file(1, "C:\\f1.txt", "f1.txt", "txt", "документ", 100, true);
    let f2 = sample_file(2, "C:\\f2.pdf", "f2.pdf", ".pdf", "книга", 200, true);

    index.add_file(&f1, None);
    index.add_file(&f2, None);

    let empty_q = parse_query("", None, false);

    // Фильтр без точки
    let filter_txt = NameFilter {
        extension: Some("txt".to_string()),
        ..Default::default()
    };
    let hits1 = index.search(&empty_q, &filter_txt, SearchMode::StrictPhrase);
    assert_eq!(hits1.len(), 1);
    assert_eq!(hits1[0].id, 1);

    // Фильтр с точкой
    let filter_dot_txt = NameFilter {
        extension: Some(".txt".to_string()),
        ..Default::default()
    };
    let hits2 = index.search(&empty_q, &filter_dot_txt, SearchMode::StrictPhrase);
    assert_eq!(hits2.len(), 1);
    assert_eq!(hits2[0].id, 1);

    // Фильтр без точки для файла с точкой в базе
    let filter_pdf = NameFilter {
        extension: Some("pdf".to_string()),
        ..Default::default()
    };
    let hits3 = index.search(&empty_q, &filter_pdf, SearchMode::StrictPhrase);
    assert_eq!(hits3.len(), 1);
    assert_eq!(hits3[0].id, 2);
}

#[test]
fn filter_by_size_category_and_availability() {
    let mut index = NamesIndex::new();

    let f1 = sample_file(
        1,
        "C:\\a.txt",
        "a",
        "txt",
        "документ",
        500,
        true, // available
    );
    let f2 = sample_file(
        2,
        "C:\\b.txt",
        "b",
        "txt",
        "документ",
        1500,
        false, // not available
    );
    let f3 = sample_file(
        3,
        "C:\\c.txt",
        "c",
        "txt",
        "книга",
        2000,
        true, // available
    );

    index.add_file(&f1, None);
    index.add_file(&f2, None);
    index.add_file(&f3, None);

    let empty_q = parse_query("", None, false);

    // Только доступные
    let filter_avail = NameFilter {
        available_only: true,
        ..Default::default()
    };
    let hits = index.search(&empty_q, &filter_avail, SearchMode::StrictPhrase);
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|h| h.available));

    // Минимальный размер >= 1000
    let filter_size = NameFilter {
        min_size: Some(1000),
        ..Default::default()
    };
    let hits_size = index.search(&empty_q, &filter_size, SearchMode::StrictPhrase);
    assert_eq!(hits_size.len(), 2);
    assert!(hits_size.iter().all(|h| h.size >= 1000));

    // Категория
    let filter_cat = NameFilter {
        category: Some("книга".to_string()),
        ..Default::default()
    };
    let hits_cat = index.search(&empty_q, &filter_cat, SearchMode::StrictPhrase);
    assert_eq!(hits_cat.len(), 1);
    assert_eq!(hits_cat[0].id, 3);
}

#[test]
fn incremental_updates_add_modify_remove() {
    let mut index = NamesIndex::new();
    let f1 = sample_file(
        1,
        "C:\\file.txt",
        "старое имя",
        "txt",
        "документ",
        100,
        true,
    );

    index.add_file(&f1, None);
    assert_eq!(index.len(), 1);

    let q1 = parse_query("старое", None, false);
    assert_eq!(
        index
            .search(&q1, &NameFilter::default(), SearchMode::StrictPhrase)
            .len(),
        1
    );

    // Обновляем запись
    let mut f1_updated = f1.clone();
    f1_updated.name = "новое имя".to_string();
    index.update_file(&f1_updated, None);

    assert_eq!(
        index
            .search(&q1, &NameFilter::default(), SearchMode::StrictPhrase)
            .len(),
        0
    );
    let q2 = parse_query("новое", None, false);
    assert_eq!(
        index
            .search(&q2, &NameFilter::default(), SearchMode::StrictPhrase)
            .len(),
        1
    );

    // Удаляем запись
    index.remove_file(1);
    assert_eq!(
        index
            .search(&q2, &NameFilter::default(), SearchMode::StrictPhrase)
            .len(),
        0
    );
    assert_eq!(index.len(), 0);
}

#[test]
fn overflow_of_category_and_extension_ids_does_not_panic_or_corrupt() {
    // Больше 255 категорий и 65535 расширений: новые получают общий id, индексы не портятся
    let mut index = NamesIndex::new();

    // Сначала обычные записи, которые должны остаться находимыми по своим категориям/расширениям
    let first = sample_file(
        1,
        "D:\\a\\первый.pdf",
        "первый.pdf",
        "pdf",
        "документ",
        10,
        true,
    );
    index.add_file(&first, None);

    for i in 0..300i64 {
        let rec = sample_file(
            100 + i,
            &format!("D:\\cat\\файл{i}.txt"),
            &format!("файл{i}.txt"),
            "txt",
            &format!("категория-{i}"),
            1,
            true,
        );
        index.add_file(&rec, None);
    }
    assert!(index.categories.len() <= 256);

    for i in 0..70_000i64 {
        let rec = sample_file(
            1000 + i,
            &format!("D:\\ext\\f{i}.e{i}"),
            &format!("f{i}.e{i}"),
            &format!("e{i}"),
            "документ",
            1,
            true,
        );
        index.add_file(&rec, None);
    }
    assert!(index.extensions.len() <= 65_536);
    assert_eq!(index.len(), 1 + 300 + 70_000);

    // Любой id внутри таблиц, build_hit не выходит за границы
    let q = parse_query("первый", None, false);
    let hits = index.search(&q, &NameFilter::default(), SearchMode::StrictPhrase);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].category, "документ");
    assert_eq!(hits[0].extension, "pdf");

    // Фильтры по ранее занесённым категории и расширению работают по-прежнему
    let by_cat = index.search(
        &q,
        &NameFilter {
            category: Some("документ".to_string()),
            ..NameFilter::default()
        },
        SearchMode::StrictPhrase,
    );
    assert_eq!(by_cat.len(), 1);
    let by_ext = index.search(
        &q,
        &NameFilter {
            extension: Some("pdf".to_string()),
            ..NameFilter::default()
        },
        SearchMode::StrictPhrase,
    );
    assert_eq!(by_ext.len(), 1);

    // Файл, попавший после переполнения, найден и получил общую категорию «прочее»
    let q_late = parse_query("файл299", None, false);
    let late = index.search(&q_late, &NameFilter::default(), SearchMode::StrictPhrase);
    assert_eq!(late.len(), 1);
    assert_eq!(late[0].category, "прочее");
}

#[test]
fn test_public_search_finds_name_through_path_index() {
    // Перенос R1-Б-60: поиск по имени находит файл
    let mut index = NamesIndex::new();
    let f = sample_file(
        10,
        "D:\\data\\редкийпуть.txt",
        "редкийпуть.txt",
        "txt",
        "документ",
        500,
        true,
    );
    index.add_file(&f, None);

    let q = parse_query("редкийпуть", None, false);
    let hits = index.search(&q, &NameFilter::default(), SearchMode::StrictPhrase);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 10);
    assert!(hits[0].in_name);
}

#[test]
fn repeated_search_has_identical_totals_and_order() {
    // Перенос R2-В-29: детерминированность повторных вызовов
    let mut index = NamesIndex::new();
    for i in 1..=10 {
        let f = sample_file(
            i,
            &format!("D:\\file_{i}.txt"),
            &format!("документ номер {i}"),
            "txt",
            "документ",
            i as u64 * 100,
            true,
        );
        index.add_file(&f, None);
    }

    let q = parse_query("документ", None, false);
    let r1 = index.search(&q, &NameFilter::default(), SearchMode::StrictPhrase);
    let r2 = index.search(&q, &NameFilter::default(), SearchMode::StrictPhrase);

    assert_eq!(r1.len(), r2.len());
    let ids1: Vec<i64> = r1.iter().map(|h| h.id).collect();
    let ids2: Vec<i64> = r2.iter().map(|h| h.id).collect();
    assert_eq!(ids1, ids2);
}

#[repr(C)]
#[allow(non_snake_case)]
struct ProcessMemoryCounters {
    cb: u32,
    PageFaultCount: u32,
    PeakWorkingSetSize: usize,
    WorkingSetSize: usize,
    QuotaPeakPagedPoolUsage: usize,
    QuotaPagedPoolUsage: usize,
    QuotaPeakNonPagedPoolUsage: usize,
    QuotaNonPagedPoolUsage: usize,
    PagefileUsage: usize,
    PeakPagefileUsage: usize,
}

unsafe extern "system" {
    fn K32GetProcessMemoryInfo(
        process: isize,
        ppsmemCounters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
    fn GetCurrentProcess() -> isize;
}

fn get_process_memory_mb() -> f64 {
    unsafe {
        let mut counters: ProcessMemoryCounters = std::mem::zeroed();
        counters.cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) != 0 {
            counters.WorkingSetSize as f64 / (1024.0 * 1024.0)
        } else {
            0.0
        }
    }
}

#[test]
#[ignore]
fn perf_names() {
    // Тест-замер perf_names:
    // 1 млн синтетических путей — загрузка, медиана поиска одного слова и фразы из двух слов,
    // память процесса; числа печатать.
    println!("=== Запуск замера perf_names на 1 000 000 путей ===");
    let morph = load_morph();

    let mem_before = get_process_memory_mb();

    let words_vocab = [
        "отчет",
        "проект",
        "документ",
        "архив",
        "система",
        "каталог",
        "поиск",
        "данные",
        "клиент",
        "сервер",
        "пользователь",
        "задача",
        "модуль",
        "индекс",
        "таблица",
        "база",
        "справка",
        "инструкция",
        "смета",
        "договор",
        "акт",
        "расчет",
        "план",
        "график",
        "ведомость",
        "накладная",
        "журнал",
        "протокол",
        "решение",
        "приказ",
    ];

    let exts = ["txt", "pdf", "docx", "xlsx", "zip", "fb2", "epub", "csv"];
    let categories = ["документ", "книга", "архив", "прочее"];

    let count = 1_000_000usize;
    let mut index = NamesIndex::new();

    let start_build = Instant::now();
    for i in 0..count {
        let w1 = words_vocab[i % words_vocab.len()];
        let w2 = words_vocab[(i / words_vocab.len()) % words_vocab.len()];
        let w3 = words_vocab[(i / 900) % words_vocab.len()];
        let ext = exts[i % exts.len()];
        let cat = categories[i % categories.len()];
        let num = i % 5000;
        let folder_id = i % 1000;

        let name = format!("{w1}_{w2}_{num}.{ext}");
        let path = format!("D:\\проекты\\{w3}_{folder_id}\\{name}");

        let rec = FileRecord {
            id: Some(i as i64 + 1),
            path,
            name,
            extension: ext.to_string(),
            size: (i as u64 % 50_000) * 1024,
            modified_ns: 1_700_000_000_000_000_000 + i as i64,
            created_ns: 1_700_000_000_000_000_000,
            category: cat.to_string(),
            inside_archive: false,
            container_id: None,
            content_status: Some("text".to_string()),
            extraction_version: 1,
            text_sha: Some(format!("sha_{}", i % 20_000)),
            quick_hash: None,
            sha256: None,
            error: None,
            scan_run: Some(1),
            source_id: Some(1),
            volume_id: Some(10),
            available: true,
            rel_path: String::new(),
        };

        index.add_file(&rec, Some(&morph));
    }
    let build_elapsed = start_build.elapsed();

    let mem_after = get_process_memory_mb();
    let mem_used = mem_after - mem_before;

    println!("Число записей: {}", index.len());
    println!("Время загрузки / индексирования 1 млн: {build_elapsed:?}");
    println!(
        "Память процесса до: {mem_before:.2} МБ, после: {mem_after:.2} МБ, занято: {mem_used:.2} МБ"
    );

    // Замер поиска одного слова (7 прогонов, берем медиану)
    let q_single = parse_query("договор", Some(&morph), true);
    let mut single_pass_times = Vec::new();
    let mut single_full_times = Vec::new();
    let mut hits_single_count = 0;
    for _ in 0..7 {
        let t0 = Instant::now();
        let matches =
            index.search_matches(&q_single, &NameFilter::default(), SearchMode::StrictPhrase);
        single_pass_times.push(t0.elapsed());
        hits_single_count = matches.len();

        let t1 = Instant::now();
        let _ = index.search_limited(
            &q_single,
            &NameFilter::default(),
            SearchMode::StrictPhrase,
            1000,
        );
        single_full_times.push(t1.elapsed());
    }
    single_pass_times.sort();
    single_full_times.sort();
    let median_single_pass = single_pass_times[single_pass_times.len() / 2];
    let median_single_top1k = single_full_times[single_full_times.len() / 2];
    println!(
        "Поиск 1 слова: параллельный проход (медиана) = {median_single_pass:?}, top-1000 = {median_single_top1k:?}, совпадений: {hits_single_count}"
    );

    // Замер поиска фразы из 2 слов (7 прогонов, берем медиану)
    let q_phrase = parse_query("проект договор", Some(&morph), true);
    let mut phrase_pass_times = Vec::new();
    let mut phrase_full_times = Vec::new();
    let mut hits_phrase_count = 0;
    for _ in 0..7 {
        let t0 = Instant::now();
        let matches =
            index.search_matches(&q_phrase, &NameFilter::default(), SearchMode::StrictPhrase);
        phrase_pass_times.push(t0.elapsed());
        hits_phrase_count = matches.len();

        let t1 = Instant::now();
        let _ = index.search_limited(
            &q_phrase,
            &NameFilter::default(),
            SearchMode::StrictPhrase,
            1000,
        );
        phrase_full_times.push(t1.elapsed());
    }
    phrase_pass_times.sort();
    phrase_full_times.sort();
    let median_phrase_pass = phrase_pass_times[phrase_pass_times.len() / 2];
    let median_phrase_top1k = phrase_full_times[phrase_full_times.len() / 2];
    println!(
        "Поиск фразы из 2 слов: параллельный проход (медиана) = {median_phrase_pass:?}, top-1000 = {median_phrase_top1k:?}, совпадений: {hits_phrase_count}"
    );

    println!("=== ИТОГИ perf_names ===");
    println!("Медиана поиска 1 слова (параллельный проход rayon): {median_single_pass:?}");
    println!("Медиана поиска 2-словной фразы (параллельный проход rayon): {median_phrase_pass:?}");
    println!("Память процесса: {mem_used:.2} МБ");
}

// Задача №239: «точно как набрано» в именах — целое слово, без других форм и продолжений
#[test]
fn exact_forms_mode_matches_whole_name_word_only() {
    let morph = load_morph();
    let mut index = NamesIndex::new();
    let names = [
        (1, "книга"),
        (2, "книгами"),
        (3, "книги"),
        (4, "книгалово"),
        (5, "Моя КНИГА"),
        (6, "Ёлка"),
    ];
    for (id, name) in names {
        let path = format!("C:\\docs\\{id}.txt");
        let file = sample_file(id, &path, name, "txt", "документ", 10, true);
        index.add_file(&file, Some(&morph));
    }
    let ids = |query: &str, forms: WordFormsMode| -> Vec<i64> {
        let parsed = parse_query_forms(query, Some(&morph), true, forms);
        let mut ids: Vec<i64> = index
            .search(&parsed, &NameFilter::default(), SearchMode::StrictPhrase)
            .iter()
            .map(|h| h.id)
            .collect();
        ids.sort_unstable();
        ids
    };

    // Exact: «книга» не находит «книгами», «книги», «книгалово»; регистр не важен
    assert_eq!(ids("книга", WordFormsMode::Exact), vec![1, 5]);
    assert_eq!(ids("КНИГА", WordFormsMode::Exact), vec![1, 5]);
    // Нужная форма находится сама, а соседние — нет
    assert_eq!(ids("книгами", WordFormsMode::Exact), vec![2]);
    // Нормализация «ё» та же, что при индексации
    assert_eq!(ids("елка", WordFormsMode::Exact), vec![6]);
    assert_eq!(ids("ёлка", WordFormsMode::Exact), vec![6]);
    // AllForms со словарём: все формы слова
    assert_eq!(ids("книга", WordFormsMode::AllForms), vec![1, 2, 3, 5]);
}

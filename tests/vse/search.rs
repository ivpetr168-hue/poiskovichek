use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use poiskovichek::catalog::Catalog;
use poiskovichek::morph::{Morph, build_morph_fst};
use poiskovichek::names::{NameFilter, NamesIndex, SearchMode};
use poiskovichek::paths::AppPaths;
use poiskovichek::pipeline::{PipelineOptions, sync_all_with_options};
use poiskovichek::search::{SearchEngine, SearchFilters, SearchRequest};
use poiskovichek::settings::{
    MultiWordMode, Settings, SortMode, Source, SourceMode, WordFormsMode,
};
use rusqlite::Connection;
use tempfile::tempdir;

struct TestFixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    paths: AppPaths,
    settings: Settings,
    morph: Morph,
}

impl TestFixture {
    fn new() -> Self {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();

        let data_dir = root.join("data");
        let source_dir = root.join("source");
        let sub_source_dir = root.join("sub_source");
        let logs_dir = root.join("logs");
        let index_dir = data_dir.join("index");
        let catalog_db = data_dir.join("catalog.sqlite3");
        let settings_file = data_dir.join("settings.toml");

        fs::create_dir_all(&source_dir).expect("create source_dir");
        fs::create_dir_all(&sub_source_dir).expect("create sub_source_dir");
        fs::create_dir_all(&data_dir).expect("create data_dir");
        fs::create_dir_all(&logs_dir).expect("create logs_dir");

        // 1. Создаём морфологический словарь словоформ в sqlite и компилируем в morph.fst
        let morph_db = root.join("morph.sqlite3");
        let fst_path = root.join("morph.fst");
        let conn = Connection::open(&morph_db).expect("open morph_db");
        conn.execute_batch(
            "CREATE TABLE word_primary (word TEXT NOT NULL, lexeme_id INTEGER NOT NULL);
             INSERT INTO word_primary VALUES
                 ('сельская', 1), ('сельские', 1), ('сельский', 1), ('сельскими', 1),
                 ('мужик', 2), ('мужики', 2), ('мужиками', 2),
                 ('установочный', 3), ('установочные', 3),
                 ('привод', 4), ('приводы', 4),
                 ('сахалинский', 5), ('сахалинская', 5), ('сахалинское', 5), ('сахалинские', 5),
                 ('ааа', 10), ('аа1', 10), ('аа2', 10), ('аа3', 10), ('аа4', 10), ('аа5', 10), ('аа6', 10), ('аа7', 10), ('аа8', 10),
                 ('ббб', 11), ('бб1', 11), ('бб2', 11), ('бб3', 11), ('бб4', 11), ('бб5', 11), ('бб6', 11), ('бб7', 11), ('бб8', 11),
                 ('ввв', 12), ('вв1', 12), ('вв2', 12), ('вв3', 12), ('вв4', 12), ('вв5', 12), ('вв6', 12), ('вв7', 12), ('вв8', 12),
                 ('ггг', 13), ('гг1', 13), ('гг2', 13), ('гг3', 13), ('гг4', 13), ('гг5', 13), ('гг6', 13), ('гг7', 13), ('гг8', 13),
                 ('ддд', 10), ('еее', 11), ('жжж', 12), ('ззз', 13),
                 ('белорусский', 20), ('набат', 21),
                 ('спешившую', 30), ('спешившая', 30), ('месту', 31), ('съемочную', 32), ('группу', 33);",
        )
        .expect("insert lexemes");
        drop(conn);

        build_morph_fst(&morph_db, &fst_path).expect("build_morph_fst");
        let morph = Morph::open(&fst_path).expect("open morph.fst");

        // 2. Создаём тестовые файлы для индексирования
        // Doc 1: разорванная фраза
        fs::write(
            source_dir.join("1.fb2"),
            "Сельские дороги затопило. Мужики вернулись вечером.",
        )
        .expect("write 1.fb2");

        // Doc 2: точная фраза
        fs::write(
            source_dir.join("2.fb2"),
            "На площади стояли сельские мужики.",
        )
        .expect("write 2.fb2");

        // Doc 3: точная фраза в другой словоформе
        fs::write(
            source_dir.join("3.fb2"),
            "Он разговаривал с сельскими мужиками.",
        )
        .expect("write 3.fb2");

        // Doc 4: обратный порядок слов
        fs::write(
            source_dir.join("4.fb2"),
            "Мужики вернулись. Сельские дороги затопило.",
        )
        .expect("write 4.fb2");

        // Doc 5: слова разделены союзом
        fs::write(source_dir.join("5.fb2"), "Сельские и мужики.").expect("write 5.fb2");

        // Файл с дефисом
        fs::write(
            source_dir.join("ReadMe.txt"),
            "Чтобы установить игру, вставьте установочный DVD в DVD-привод.",
        )
        .expect("write ReadMe.txt");

        // Файл с префиксом
        fs::write(
            source_dir.join("sakhalin.txt"),
            "Сахалинский залив и остров на востоке.",
        )
        .expect("write sakhalin.txt");

        // 4 слова с > 512 сочетаний: точный документ
        fs::write(
            source_dir.join("exact.txt"),
            "ддд еее жжж ззз в тексте документа",
        )
        .expect("write exact.txt");

        // 4 слова с > 512 сочетаний: документ с разрывом
        fs::write(
            source_dir.join("gap.txt"),
            "ддд еее лишнее слово жжж ззз в тексте документа",
        )
        .expect("write gap.txt");

        // Документ с высокой плотностью слова
        fs::write(
            source_dir.join("dense.txt"),
            "слово слово слово слово слово слово слово слово слово слово",
        )
        .expect("write dense.txt");

        // Документ с низкой плотностью слова
        fs::write(
            source_dir.join("sparse.txt"),
            "слово и много других не связанных терминов в предложении",
        )
        .expect("write sparse.txt");

        // Копия документа для проверки группировки дубликатов
        fs::write(
            sub_source_dir.join("dense_copy.txt"),
            "слово слово слово слово слово слово слово слово слово слово",
        )
        .expect("write dense_copy.txt");

        // Файл для проверки имени и пути
        fs::write(
            source_dir.join("сельские мужики.fb2"),
            "простой нейтральный текст",
        )
        .expect("write named.fb2");

        // Файл устаревшего формата .doc
        fs::write(
            source_dir.join("legacy.doc"),
            b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1",
        )
        .expect("write legacy.doc");

        let settings = Settings {
            data_dir: data_dir.clone(),
            enable_morphology: true,
            sources: vec![
                Source {
                    path: source_dir.clone(),
                    mode: SourceMode::Full,
                    max_depth: 0,
                    enabled: true,
                    volume_serial: None,
                    extra_excludes: Vec::new(),
                },
                Source {
                    path: sub_source_dir.clone(),
                    mode: SourceMode::Full,
                    max_depth: 0,
                    enabled: true,
                    volume_serial: None,
                    extra_excludes: Vec::new(),
                },
            ],
            ..Default::default()
        };

        let paths = AppPaths {
            settings_file,
            data_dir,
            catalog_db,
            index_dir,
            logs_dir,
        };

        // 3. Индексируем пробный корпус через pipeline
        let options = PipelineOptions {
            queue_capacity: 8,
            in_process_workers: true,
            morph: Some(morph.clone()),
            ..Default::default()
        };
        sync_all_with_options(&settings, options).expect("sync_all");

        Self {
            _dir: dir,
            root,
            paths,
            settings,
            morph,
        }
    }

    fn engine(&self) -> SearchEngine {
        SearchEngine::open(&self.paths, &self.settings, Some(self.morph.clone()))
            .expect("open search engine")
    }

    fn engine_without_morph(&self) -> SearchEngine {
        SearchEngine::open(&self.paths, &self.settings, None)
            .expect("open search engine without morph")
    }
}

// ---------------------------------------------------------------------------
// R2-В-3: Форма проверки эталона: запрос -> число групп -> есть подсветка
// ---------------------------------------------------------------------------
#[test]
fn real_database_format_query_total_highlight() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    struct ExpectedCheck {
        query: &'static str,
        expected_total: usize,
        expected_highlight: bool,
    }

    let checks = [
        ExpectedCheck {
            query: "сельские мужики",
            expected_total: 3, // 2.fb2, 3.fb2, и по имени сельские мужики.fb2
            expected_highlight: true,
        },
        ExpectedCheck {
            query: "вставьте установочный DVD в DVD-привод",
            expected_total: 1,
            expected_highlight: true,
        },
        ExpectedCheck {
            query: "Сахалинск",
            expected_total: 1,
            expected_highlight: true,
        },
    ];

    for check in &checks {
        let req = SearchRequest::new(check.query);
        let resp = engine.search(&req);
        assert_eq!(
            resp.total, check.expected_total,
            "Не совпал total для запроса: {}",
            check.query
        );
        let mut highlighted = false;
        for item in &resp.items {
            let snip = engine.snippet_for(&item.item_key, &req);
            let s = snip.to_marked_string();
            if s.contains('⟦') && s.contains('⟧') {
                highlighted = true;
                break;
            }
        }
        assert_eq!(
            highlighted, check.expected_highlight,
            "Не совпало наличие подсветки для запроса: {}",
            check.query
        );
    }
}

// ---------------------------------------------------------------------------
// R2-В-22: «сельские мужики» — только документы со смежными словами в этом порядке
// ---------------------------------------------------------------------------
#[test]
fn strict_phrase_accepts_only_adjacent_ordered_word_forms() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let response = engine.search(SearchRequest::new("сельские мужики"));
    let paths: Vec<_> = response.items.iter().flat_map(|it| &it.paths).collect();

    // 1.fb2 (разрыв) и 4.fb2 (обратный порядок) и 5.fb2 (союз и) не должны находиться по тексту
    assert!(!paths.iter().any(|p| p.ends_with("1.fb2")));
    assert!(!paths.iter().any(|p| p.ends_with("4.fb2")));
    assert!(!paths.iter().any(|p| p.ends_with("5.fb2")));

    // 2.fb2 (точная фраза) и 3.fb2 (словоформа) должны находиться
    assert!(paths.iter().any(|p| p.ends_with("2.fb2")));
    assert!(paths.iter().any(|p| p.ends_with("3.fb2")));

    // Все совпадения имеют флаг phrase_hit = true
    assert!(response.items.iter().all(|it| it.phrase_hit));
}

// ---------------------------------------------------------------------------
// R2-В-23: Обратный порядок — 0; «сельскими мужиками» находит именительный падеж
// ---------------------------------------------------------------------------
#[test]
fn strict_phrase_rejects_reverse_order_and_accepts_query_word_forms() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp_rev = engine.search(SearchRequest::new("мужики сельские"));
    assert_eq!(
        resp_rev.total, 0,
        "Обратный порядок должен давать 0 результатов"
    );

    let resp_forms = engine.search(SearchRequest::new("сельскими мужиками"));
    let paths: Vec<_> = resp_forms.items.iter().flat_map(|it| &it.paths).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("2.fb2")),
        "Запрос в творительном падеже должен находить документ в именительном падеже"
    );
}

// ---------------------------------------------------------------------------
// R2-В-24: Однословный запрос — phrase_hit = false, находит все документы
// ---------------------------------------------------------------------------
#[test]
fn single_word_keeps_previous_semantics() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("мужики"));
    assert!(resp.total >= 4);
    assert!(resp.items.iter().all(|it| !it.phrase_hit));
}

// ---------------------------------------------------------------------------
// R2-В-25: Сниппет центрируется точно по фразе
// ---------------------------------------------------------------------------
#[test]
fn snippet_is_anchored_on_the_real_phrase() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let req = SearchRequest::new("сельские мужики");
    let resp = engine.search(&req);
    let item_2 = resp
        .items
        .iter()
        .find(|it| it.paths.iter().any(|p| p.ends_with("2.fb2")))
        .expect("item for 2.fb2");

    let snip = engine.snippet_for(&item_2.item_key, &req);
    let marked = snip.to_marked_string();
    assert!(
        marked.contains("⟦сельские⟧ ⟦мужики⟧"),
        "Фрагмент должен содержать точную подсветку фразы: {}",
        marked
    );
}

// ---------------------------------------------------------------------------
// R2-В-26: Путь и имя не могут поставлять разные слова фразы
// ---------------------------------------------------------------------------
#[test]
fn path_and_name_cannot_supply_different_words() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    // Создаём файл, где в пути есть 'мужики', а в имени 'сельские'
    let fake_file = fixture
        .root
        .join("source")
        .join("мужики_папка")
        .join("сельские.txt");
    fs::create_dir_all(fake_file.parent().unwrap()).unwrap();
    fs::write(&fake_file, "никаких слов внутри").unwrap();

    // Запускаем поиск "сельские мужики"
    let resp = engine.search(SearchRequest::new("сельские мужики"));
    let paths: Vec<_> = resp.items.iter().flat_map(|it| &it.paths).collect();
    assert!(
        !paths
            .iter()
            .any(|p| p.contains("мужики_папка") && p.contains("сельские.txt"))
    );
}

// ---------------------------------------------------------------------------
// R2-В-27: Фраза из 4 слов с > 512 сочетаний: кандидаты + проверка отсеивают разрыв
// ---------------------------------------------------------------------------
#[test]
fn bounded_candidate_query_still_verifies_the_whole_long_phrase() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    // 4 слова, каждое по 9 форм: 9^4 = 6561 > 512
    let resp = engine.search(SearchRequest::new("ааа ббб ввв ггг"));
    assert_eq!(
        resp.total, 1,
        "Должен быть найден ровно один документ без разрыва"
    );
    assert!(
        resp.items[0].paths[0].ends_with("exact.txt"),
        "Найденный документ должен быть exact.txt"
    );
}

// ---------------------------------------------------------------------------
// R2-В-28: Поиск фразы с дефисом
// ---------------------------------------------------------------------------
#[test]
fn strict_phrase_treats_hyphen_as_a_word_boundary() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("вставьте установочный DVD в DVD-привод"));
    assert_eq!(resp.total, 1);
    assert!(resp.items[0].phrase_hit);
    assert!(resp.items[0].paths[0].ends_with("ReadMe.txt"));
}

// ---------------------------------------------------------------------------
// R2-В-29: Повторный поиск — те же total, длина и порядок ключей
// ---------------------------------------------------------------------------
#[test]
fn repeated_search_has_identical_totals_lengths_and_key_order() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let first = engine.search(SearchRequest::new("сельские мужики"));
    let second = engine.search(SearchRequest::new("сельские мужики"));

    assert_eq!(first.total, second.total);
    assert_eq!(first.items.len(), second.items.len());
    let keys_1: Vec<_> = first.items.iter().map(|it| &it.item_key).collect();
    let keys_2: Vec<_> = second.items.iter().map(|it| &it.item_key).collect();
    assert_eq!(keys_1, keys_2);
}

// ---------------------------------------------------------------------------
// R2-В-30: Пути внутри сгруппированной карточки сортируются без учёта регистра
// ---------------------------------------------------------------------------
#[test]
fn grouped_paths_are_stable_and_sorted_case_insensitively() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("слово"));
    let dense_item = resp
        .items
        .iter()
        .find(|it| it.paths.iter().any(|p| p.ends_with("dense.txt")))
        .expect("dense item");

    assert_eq!(dense_item.paths.len(), 2);
    // Проверяем сортировку путей
    assert!(dense_item.paths[0].to_lowercase() <= dense_item.paths[1].to_lowercase());
}

// ---------------------------------------------------------------------------
// R2-В-31: Устойчивый tie-breaker при равном ранге
// ---------------------------------------------------------------------------
#[test]
fn equal_rank_uses_key_as_a_stable_tie_breaker() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp1 = engine.search(SearchRequest::new("слово"));
    let resp2 = engine.search(SearchRequest::new("слово"));
    let keys1: Vec<_> = resp1.items.iter().map(|i| &i.item_key).collect();
    let keys2: Vec<_> = resp2.items.iter().map(|i| &i.item_key).collect();
    assert_eq!(keys1, keys2);
}

// ---------------------------------------------------------------------------
// R2-В-32: Изоляция тестовой среды (удаление каталога при drop)
// ---------------------------------------------------------------------------
#[test]
fn fixture_drop_removes_its_private_directory() {
    let dir = tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    assert!(path.is_dir());
    drop(dir);
    assert!(!path.exists());
}

// ---------------------------------------------------------------------------
// R2-В-33: Пустой запрос, несуществующее слово, границы лимита 1..500
// ---------------------------------------------------------------------------
#[test]
fn empty_missing_and_limits_are_handled() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    // Пустой запрос без фильтров
    let empty = engine.search(SearchRequest::new(""));
    assert_eq!(empty.total, 0);
    assert!(empty.items.is_empty());

    // Несуществующее слово
    let missing = engine.search(SearchRequest::new("несуществующееслово123"));
    assert_eq!(missing.total, 0);
    assert!(missing.items.is_empty());

    // Прижим лимита 0 -> 1
    let req0 = SearchRequest::new("слово").with_limit(0);
    let resp0 = engine.search(&req0);
    assert_eq!(resp0.items.len(), 1);

    // Прижим лимита 10 000 -> <= 500
    let req_large = SearchRequest::new("слово").with_limit(10_000);
    let resp_large = engine.search(&req_large);
    assert!(resp_large.items.len() <= 500);
}

// ---------------------------------------------------------------------------
// R2-В-34: Ранг BM25: документ с большей плотностью терминов релевантнее
// ---------------------------------------------------------------------------
#[test]
fn rank_elapsed_and_repeated_sources_are_correct() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("слово"));
    assert!(resp.items.len() >= 2);
    // dense.txt должен идти раньше или иметь лучший ранг, чем sparse.txt
    let dense_pos = resp
        .items
        .iter()
        .position(|it| it.paths.iter().any(|p| p.ends_with("dense.txt")))
        .unwrap();
    let sparse_pos = resp
        .items
        .iter()
        .position(|it| it.paths.iter().any(|p| p.ends_with("sparse.txt")))
        .unwrap();
    assert!(dense_pos < sparse_pos);
    assert!(resp.elapsed > Duration::ZERO);
}

// ---------------------------------------------------------------------------
// R2-В-35: Совпадение в тексте и в имени объединяются в одну строку
// ---------------------------------------------------------------------------
#[test]
fn name_and_path_are_searched_and_merged_with_text() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    // Запрос "сельские мужики" совпадает и по тексту (2.fb2, 3.fb2), и по имени (сельские мужики.fb2)
    let resp = engine.search(SearchRequest::new("сельские мужики"));
    let named_count = resp
        .items
        .iter()
        .filter(|it| it.paths.iter().any(|p| p.ends_with("сельские мужики.fb2")))
        .count();
    assert_eq!(
        named_count, 1,
        "Файл должен быть представлен ровно одной строкой"
    );
}

// ---------------------------------------------------------------------------
// R2-В-36: compressed_body чётко различает присутствующий и отсутствующий блоб
// ---------------------------------------------------------------------------
#[test]
fn compressed_body_distinguishes_present_missing_and_empty() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("ReadMe.txt"));
    let sha = resp.items[0].text_sha.as_deref().expect("has text_sha");
    let bytes = engine.compressed_body(sha).unwrap();
    assert!(bytes.is_some());
    assert!(!bytes.unwrap().is_empty());

    let missing = engine
        .compressed_body("ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff")
        .unwrap();
    assert!(missing.is_none());
}

// ---------------------------------------------------------------------------
// R1-Б-45: Формат .doc возвращает статус с пояснением о встроенном извлекателе
// ---------------------------------------------------------------------------
#[test]
fn test_legacy_formats_report_the_missing_text_extractor() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let req = SearchRequest::new("legacy.doc");
    let resp = engine.search(&req);
    assert_eq!(resp.total, 1);
    let item = &resp.items[0];
    assert!(item.status.is_some());
    let status_str = item.status.as_ref().unwrap();
    assert!(
        status_str.contains("не извлечён") && status_str.contains("не встроен"),
        "Статус должен пояснять причину: {}",
        status_str
    );
}

// ---------------------------------------------------------------------------
// R1-Б-51: Пустой запрос без фильтров -> пусто, total 0
// ---------------------------------------------------------------------------
#[test]
fn test_empty_query_without_filters_returns_empty() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("   "));
    assert_eq!(resp.total, 0);
    assert!(resp.items.is_empty());
}

// ---------------------------------------------------------------------------
// R1-Б-52: Пустой запрос с фильтрами -> выдача по фильтрам с точным total
// ---------------------------------------------------------------------------
#[test]
fn test_browser_filter_request_shows_results_table() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let mut req = SearchRequest::new("");
    req.filters.extension = Some("fb2".into());
    let resp = engine.search(&req);
    assert!(resp.total >= 5);
    assert!(
        resp.items
            .iter()
            .all(|it| it.extension.eq_ignore_ascii_case("fb2"))
    );
}

// ---------------------------------------------------------------------------
// R1-Б-53: Поиск русских словоформ возвращает сгруппированный total
// ---------------------------------------------------------------------------
#[test]
fn test_search_finds_russian_word_forms_and_reports_grouped_total() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("сельские"));
    assert!(resp.total >= 4);
}

// ---------------------------------------------------------------------------
// R1-Б-59: Фильтрация по источнику и расширению
// ---------------------------------------------------------------------------
#[test]
fn test_search_filter_by_root_and_extension() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let mut req = SearchRequest::new("слово");
    req.filters.extension = Some("txt".into());
    let resp = engine.search(&req);
    assert!(resp.items.iter().all(|it| it.extension == "txt"));
}

// ---------------------------------------------------------------------------
// R1-Б-60: Фильтрация по категории и минимальному размеру
// ---------------------------------------------------------------------------
#[test]
fn test_search_filter_by_category_and_min_size() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let mut req = SearchRequest::new("слово");
    req.filters.min_size = Some(10);
    let resp = engine.search(&req);
    assert!(resp.items.iter().all(|it| it.size >= 10));
}

// ---------------------------------------------------------------------------
// R1-Б-61: Совпадение в тексте и в имени файла объединяются в одну строку
// ---------------------------------------------------------------------------
#[test]
fn test_public_search_merges_path_and_text_hit_without_duplicate_row() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("ReadMe"));
    assert_eq!(resp.total, 1);
    assert_eq!(resp.items.len(), 1);
}

// ---------------------------------------------------------------------------
// R1-Б-66: Одинаковые тексты объединяются в одну карточку
// ---------------------------------------------------------------------------
#[test]
fn test_identical_texts_share_one_blob_and_one_search_result() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("dense"));
    let dense_items: Vec<_> = resp
        .items
        .into_iter()
        .filter(|it| it.paths.iter().any(|p| p.contains("dense")))
        .collect();
    assert_eq!(dense_items.len(), 1);
    assert_eq!(dense_items[0].paths.len(), 2);
}

// ---------------------------------------------------------------------------
// R1-Б-67: Фрагмент строится один раз на группу
// ---------------------------------------------------------------------------
#[test]
fn test_middle_snippet_is_built_once_for_shared_text() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let req = SearchRequest::new("слово");
    let resp = engine.search(&req);
    let item = resp
        .items
        .iter()
        .find(|it| it.paths.iter().any(|p| p.contains("dense")))
        .expect("найден документ dense");
    let snip = engine.snippet_for(&item.item_key, &req);
    assert!(!snip.is_empty());
}

// ---------------------------------------------------------------------------
// R1-Б-68: В режиме «все слова» точная фраза ранжируется выше
// ---------------------------------------------------------------------------
#[test]
fn test_exact_phrase_is_ranked_before_separate_word_forms() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let req = SearchRequest::new("сельские мужики").with_mode(MultiWordMode::AllWords);
    let resp = engine.search(&req);

    // Документы с точной фразой должны иметь phrase_hit = true и стоять перед документами с phrase_hit = false
    let phrase_hits: Vec<_> = resp.items.iter().map(|it| it.phrase_hit).collect();
    assert!(
        phrase_hits[0],
        "Первым должен идти документ с точной фразой"
    );
    assert!(
        phrase_hits.windows(2).all(|w| !(!w[0] && w[1])),
        "Все документы с phrase_hit=true должны предшествовать phrase_hit=false"
    );
}

// ---------------------------------------------------------------------------
// Неизвестное слово префиксом: «Сахалинск» находит «Сахалинский»
// ---------------------------------------------------------------------------
#[test]
fn test_unknown_word_prefix_fallback() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp = engine.search(SearchRequest::new("Сахалинск"));
    assert_eq!(resp.total, 1);
    assert!(resp.items[0].paths[0].ends_with("sakhalin.txt"));
}

// ---------------------------------------------------------------------------
// Спецсимволы и сверхдлинные запросы: AND, ", ***, 10 000 слов без паники
// ---------------------------------------------------------------------------
#[test]
fn test_special_queries_no_panic() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let resp_and = engine.search(SearchRequest::new("AND"));
    let _ = resp_and.total;

    let resp_quotes = engine.search(SearchRequest::new("\"\"\"\""));
    assert_eq!(resp_quotes.total, 0);

    let resp_stars = engine.search(SearchRequest::new("***"));
    assert_eq!(resp_stars.total, 0);

    let long_query = "слово ".repeat(10_000);
    let resp_long = engine.search(SearchRequest::new(long_query));
    let _ = resp_long.total;
}

// ---------------------------------------------------------------------------
// Тест О3: Каждый фильтр и каждая сортировка меняют выдачу так, как должны
// ---------------------------------------------------------------------------
#[test]
fn test_all_filters_and_sorts_affect_output() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    // 1. Фильтр расширения: txt vs fb2
    let req_txt = SearchRequest::new("").with_filters(SearchFilters {
        extension: Some("txt".into()),
        ..Default::default()
    });
    let resp_txt = engine.search(&req_txt);
    assert!(resp_txt.total > 0);
    assert!(resp_txt.items.iter().all(|i| i.extension == "txt"));

    let req_fb2 = SearchRequest::new("").with_filters(SearchFilters {
        extension: Some("fb2".into()),
        ..Default::default()
    });
    let resp_fb2 = engine.search(&req_fb2);
    assert!(resp_fb2.total > 0);
    assert!(resp_fb2.items.iter().all(|i| i.extension == "fb2"));

    // 2. Фильтр duplicates_only
    let req_all = SearchRequest::new("");
    let req_dups = SearchRequest::new("").with_filters(SearchFilters {
        duplicates_only: true,
        ..Default::default()
    });
    let resp_all = engine.search(&req_all);
    let resp_dups = engine.search(&req_dups);
    assert!(resp_all.total == 0); // без фильтров пустой запрос = 0
    assert!(resp_dups.total > 0);
    assert!(resp_dups.items.iter().all(|i| i.is_duplicate));

    // 3. Сортировки: SizeDesc vs NameAsc
    let req_size = SearchRequest::new("сельские").with_sort(SortMode::SizeDesc);
    let req_name = SearchRequest::new("сельские").with_sort(SortMode::NameAsc);
    let resp_size = engine.search(&req_size);
    let resp_name = engine.search(&req_name);

    let sizes: Vec<_> = resp_size.items.iter().map(|i| i.size).collect();
    assert!(sizes.windows(2).all(|w| w[0] >= w[1]));

    let names: Vec<_> = resp_name
        .items
        .iter()
        .map(|i| i.name.to_lowercase())
        .collect();
    assert!(names.windows(2).all(|w| w[0] <= w[1]));
}

// ---------------------------------------------------------------------------
// Тест О3: Режим «все слова» из настроек меняет поведение
// ---------------------------------------------------------------------------
#[test]
fn test_multi_word_mode_setting_changes_behavior() {
    let fixture = TestFixture::new();

    let mut settings_strict = fixture.settings.clone();
    settings_strict.multi_word_mode = MultiWordMode::StrictPhrase;
    let engine_strict = SearchEngine::open(
        &fixture.paths,
        &settings_strict,
        Some(fixture.morph.clone()),
    )
    .unwrap();

    let mut settings_all = fixture.settings.clone();
    settings_all.multi_word_mode = MultiWordMode::AllWords;
    let engine_all =
        SearchEngine::open(&fixture.paths, &settings_all, Some(fixture.morph.clone())).unwrap();

    let resp_strict = engine_strict.search("ддд еее жжж ззз");
    assert_eq!(
        resp_strict.total, 1,
        "В StrictPhrase только документ без разрыва"
    );

    let resp_all = engine_all.search("ддд еее жжж ззз");
    assert_eq!(
        resp_all.total, 2,
        "В AllWords оба документа (с разрывом и без)"
    );
}

// ---------------------------------------------------------------------------
// Требование 6: Предупреждение об отсутствии словаря словоформ
// ---------------------------------------------------------------------------
#[test]
fn test_morphology_warning_when_not_found() {
    let fixture = TestFixture::new();
    let engine_no_morph = fixture.engine_without_morph();

    let resp = engine_no_morph.search(SearchRequest::new("слово"));
    assert!(
        !resp.warnings.is_empty(),
        "Должно присутствовать предупреждение"
    );
    assert!(
        resp.warnings[0].contains("Словарь словоформ не найден:")
            && resp.warnings[0].contains("Поиск работает без словоформ"),
        "Текст предупреждения не совпадает: {}",
        resp.warnings[0]
    );
}

// ---------------------------------------------------------------------------
// Бенчмарк: 100 тыс. документов, замер 7 прогонов (требование 7, #[ignore])
// ---------------------------------------------------------------------------
#[test]
#[ignore = "тест-замер perf_search_text на 100 000 документов"]
fn perf_search_text() {
    println!("=== Генерация и замер perf_search_text на 100 000 документов ===");
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let catalog_db = data_dir.join("catalog.sqlite3");
    let index_dir = data_dir.join("index");
    let logs_dir = data_dir.join("logs");
    let settings_file = data_dir.join("settings.toml");

    fs::create_dir_all(&data_dir).unwrap();
    fs::create_dir_all(&logs_dir).unwrap();
    fs::create_dir_all(&index_dir).unwrap();

    let morph_db = dir.path().join("morph.sqlite3");
    let fst_path = dir.path().join("morph.fst");
    let conn = Connection::open(&morph_db).unwrap();
    conn.execute_batch(
        "CREATE TABLE word_primary (word TEXT NOT NULL, lexeme_id INTEGER NOT NULL);
         INSERT INTO word_primary VALUES
             ('сельские', 1), ('сельский', 1), ('сельскими', 1),
             ('мужики', 2), ('мужик', 2), ('мужиками', 2),
             ('спешившую', 3), ('спешившая', 3),
             ('месту', 4), ('место', 4),
             ('съемочную', 5), ('съемочная', 5),
             ('группу', 6), ('группа', 6);",
    )
    .unwrap();
    drop(conn);

    build_morph_fst(&morph_db, &fst_path).unwrap();
    let morph = Morph::open(&fst_path).unwrap();

    // Инициализируем Catalog и Tantivy IndexWriter напрямую для максимальной скорости генерации
    {
        let _ = Catalog::open_or_create(&catalog_db).unwrap();
    }
    let mut writer_handle =
        poiskovichek::index::open_or_create(&index_dir, Some(morph.clone())).unwrap();

    println!("Генерация 100 000 записей базы и индекса...");
    let t_gen = Instant::now();

    let mut conn = Connection::open(&catalog_db).unwrap();
    let tx = conn.transaction().unwrap();

    {
        let mut file_stmt = tx.prepare(
            "INSERT INTO files (id, path, rel_path, name, extension, size, modified_ns, created_ns, category, inside_archive, text_sha, available)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 1);"
        ).unwrap();

        let mut text_stmt = tx
            .prepare("INSERT INTO texts (sha, chars, body) VALUES (?1, ?2, ?3);")
            .unwrap();

        for i in 0..100_000 {
            let doc_text = if i == 42 {
                "это редчайшееслово для точечной проверки поиска"
            } else if i < 200 {
                "на площади стояли сельские мужики и громко обсуждали новости"
            } else if i < 400 {
                "спешившую к месту съемочную группу встретили у входа"
            } else if i % 5 == 0 {
                "обычный документ содержащий частое слово для проверки производительности tantivy индекса"
            } else {
                "просто типовой текстовый контент синтетического корпуса документов"
            };

            let sha = format!("{:064x}", i);
            let compressed = zstd::encode_all(doc_text.as_bytes(), 1).unwrap();

            text_stmt
                .execute(rusqlite::params![
                    &sha,
                    doc_text.chars().count() as i64,
                    &compressed
                ])
                .unwrap();

            let path = format!("C:\\Corpus\\Doc_{:06}.txt", i);
            let name = format!("Doc_{:06}.txt", i);
            file_stmt
                .execute(rusqlite::params![
                    (i + 1) as i64,
                    &path,
                    &name,
                    &name,
                    "txt",
                    doc_text.len() as i64,
                    1_700_000_000_000_000_000i64 + i as i64,
                    1_700_000_000_000_000_000i64,
                    "документ",
                    0,
                    &sha
                ])
                .unwrap();

            writer_handle.add(&sha, doc_text).unwrap();
        }
    }

    tx.commit().unwrap();
    drop(conn);
    writer_handle.commit().unwrap();
    println!(
        "Генерация 100 000 документов завершена за {:?}",
        t_gen.elapsed()
    );

    let paths = AppPaths {
        settings_file,
        data_dir,
        catalog_db,
        index_dir,
        logs_dir,
    };
    let settings = Settings::default();
    let engine = SearchEngine::open(&paths, &settings, Some(morph.clone())).unwrap();

    // Замер медианы 7 прогонов для каждой операции
    fn median_7(mut f: impl FnMut() -> Duration) -> Duration {
        let mut times = Vec::with_capacity(7);
        for _ in 0..7 {
            times.push(f());
        }
        times.sort();
        times[3]
    }

    // 1. Частое слово
    let req_frequent = SearchRequest::new("слово");
    let med_frequent = median_7(|| {
        let t = Instant::now();
        let resp = engine.search(&req_frequent);
        assert!(resp.total > 10_000);
        t.elapsed()
    });

    // 2. Редкое слово
    let req_rare = SearchRequest::new("редчайшееслово");
    let med_rare = median_7(|| {
        let t = Instant::now();
        let resp = engine.search(&req_rare);
        assert_eq!(resp.total, 1);
        t.elapsed()
    });

    // 3. Фраза из 2 слов
    let req_phrase2 = SearchRequest::new("сельские мужики");
    let med_phrase2 = median_7(|| {
        let t = Instant::now();
        let resp = engine.search(&req_phrase2);
        assert!(resp.total > 0);
        t.elapsed()
    });

    // 4. Фраза из 4 слов
    let req_phrase4 = SearchRequest::new("спешившую к месту съемочную группу");
    let med_phrase4 = median_7(|| {
        let t = Instant::now();
        let resp = engine.search(&req_phrase4);
        assert!(resp.total > 0);
        t.elapsed()
    });

    // 5. Поиск по имени на 1 000 000 записей через names
    println!("Подготовка 1 000 000 записей для теста поиска по имени...");
    let mut names_1m = NamesIndex::new();
    for i in 0..1_000_000 {
        let path = format!("D:\\Photos\\2026\\IMG_{:07}.jpg", i);
        let name = format!("IMG_{:07}.jpg", i);
        let rec = poiskovichek::FileRecord {
            id: Some((i + 1) as i64),
            path,
            name,
            extension: "jpg".to_string(),
            size: 2_048_000,
            modified_ns: 1_700_000_000_000_000_000i64,
            created_ns: 1_700_000_000_000_000_000i64,
            category: "изображение".to_string(),
            inside_archive: false,
            container_id: None,
            content_status: None,
            extraction_version: 0,
            text_sha: None,
            quick_hash: None,
            sha256: None,
            error: None,
            scan_run: None,
            source_id: Some(1),
            volume_id: Some(1),
            available: true,
            rel_path: String::new(),
        };
        names_1m.add_file(&rec, Some(&morph));
    }

    let parsed_name = poiskovichek::query::parse_query("IMG 0050000", Some(&morph), false);
    let med_name_1m = median_7(|| {
        let t = Instant::now();
        let hits = names_1m.search_limited(
            &parsed_name,
            &NameFilter::default(),
            SearchMode::StrictPhrase,
            200,
        );
        assert!(!hits.is_empty());
        t.elapsed()
    });

    println!("\n================== ИТОГИ perf_search_text ==================");
    println!(
        "Медиана поиска частого слова (100k документов):   {:?}",
        med_frequent
    );
    println!(
        "Медиана поиска редкого слова (100k документов):    {:?}",
        med_rare
    );
    println!(
        "Медиана фразы из 2 слов (100k документов):         {:?}",
        med_phrase2
    );
    println!(
        "Медиана фразы из 4 слов (100k документов):         {:?}",
        med_phrase4
    );
    println!(
        "Медиана поиска по имени (1 000 000 записей names): {:?}",
        med_name_1m
    );
    println!("============================================================");

    assert!(
        med_frequent <= Duration::from_millis(80),
        "Цель: текст <= 80 мс, факт: {:?}",
        med_frequent
    );
    assert!(
        med_phrase4 <= Duration::from_millis(100),
        "Цель: фраза из 4 слов <= 100 мс, факт: {:?}",
        med_phrase4
    );
    assert!(
        med_name_1m <= Duration::from_millis(30),
        "Цель: имя <= 30 мс, факт: {:?}",
        med_name_1m
    );
}

#[test]
fn test_card_status_formatting_all_variants() {
    use poiskovichek::search::format_extraction_status;

    // 1. unsupported-text-<ext>
    assert_eq!(
        format_extraction_status(Some("unsupported-text-doc"), None),
        Some("текст внутри .doc не извлечён: извлекатель не встроен".to_string())
    );
    assert_eq!(
        format_extraction_status(Some("unsupported-text-djvu"), None),
        Some("текст внутри .djvu не извлечён: извлекатель не встроен".to_string())
    );

    // 2. scan-needs-ocr
    assert_eq!(
        format_extraction_status(Some("scan-needs-ocr"), None),
        Some("PDF без текстового слоя (скан): нужен OCR".to_string())
    );

    // 3. failed
    assert_eq!(
        format_extraction_status(Some("failed"), Some("файл повреждён")),
        Some("ошибка извлечения: файл повреждён".to_string())
    );
    assert_eq!(
        format_extraction_status(Some("failed"), None),
        Some("ошибка извлечения".to_string())
    );
    assert_eq!(
        format_extraction_status(Some("failed: таймаут"), None),
        Some("ошибка извлечения: таймаут".to_string())
    );

    // 4. sensitive-metadata-only
    assert_eq!(
        format_extraction_status(Some("sensitive-metadata-only"), None),
        Some("закрытые данные: содержимое не читается".to_string())
    );

    // 5. archive-member-too-large
    assert_eq!(
        format_extraction_status(Some("archive-member-too-large"), None),
        Some("внутренний файл архива больше 20 МБ".to_string())
    );
    assert_eq!(
        format_extraction_status(Some("archive-member-too-large"), Some("50")),
        Some("внутренний файл архива больше 50 МБ".to_string())
    );

    // 6. text (извлечён) — без пометки
    assert_eq!(format_extraction_status(Some("text"), None), None);

    // 7. metadata-only (картинка, видео и т.д.) — без пометки
    assert_eq!(format_extraction_status(Some("metadata-only"), None), None);
    assert_eq!(format_extraction_status(Some("ok"), None), None);
}

// ---------------------------------------------------------------------------
// №224: после повторной индексации reload показывает новый файл по имени без перезапуска
// ---------------------------------------------------------------------------
#[test]
fn reload_makes_new_file_findable_by_name_and_text() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();

    let req_name = SearchRequest::new("уникальноеимя");
    let req_text = SearchRequest::new("редкоепятно");
    assert_eq!(engine.search(&req_name).total, 0);

    let source_dir = fixture.root.join("source");
    fs::write(
        source_dir.join("уникальноеимя.txt"),
        "в этом файле есть редкоепятно для проверки",
    )
    .expect("write new file");
    let options = PipelineOptions {
        queue_capacity: 8,
        in_process_workers: true,
        morph: Some(fixture.morph.clone()),
        ..Default::default()
    };
    sync_all_with_options(&fixture.settings, options).expect("повторная индексация");

    // Имена в памяти ещё прежние
    assert_eq!(engine.search(&req_name).total, 0);

    engine.reload().expect("reload");
    assert_eq!(engine.search(&req_name).total, 1);
    assert_eq!(engine.search(&req_text).total, 1);
}

// Поиск во время reload не падает и видит либо старое, либо новое состояние целиком
#[test]
fn search_during_reload_sees_consistent_state() {
    let fixture = TestFixture::new();
    let engine = fixture.engine();
    let before = engine.search(SearchRequest::new("сельские")).total;

    std::thread::scope(|s| {
        s.spawn(|| {
            for _ in 0..30 {
                engine.reload().expect("reload");
            }
        });
        s.spawn(|| {
            for _ in 0..100 {
                let total = engine.search(SearchRequest::new("сельские")).total;
                assert_eq!(total, before);
            }
        });
    });
}

// ---------------------------------------------------------------------------
// Задача №239: режим словоформ запроса «все формы» / «точно как набрано»
// ---------------------------------------------------------------------------
struct FormsFixture {
    _dir: tempfile::TempDir,
    paths: AppPaths,
    settings: Settings,
    morph: Morph,
}

impl FormsFixture {
    fn new() -> Self {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let data_dir = root.join("data");
        let source_dir = root.join("source");
        let logs_dir = root.join("logs");
        fs::create_dir_all(&source_dir).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        fs::create_dir_all(&logs_dir).unwrap();

        let morph_db = root.join("morph.sqlite3");
        let fst_path = root.join("morph.fst");
        let conn = Connection::open(&morph_db).unwrap();
        conn.execute_batch(
            "CREATE TABLE word_primary (word TEXT NOT NULL, lexeme_id INTEGER NOT NULL);
             INSERT INTO word_primary VALUES
                 ('книга', 1), ('книги', 1), ('книгами', 1),
                 ('тетрадь', 2), ('тетради', 2);",
        )
        .unwrap();
        drop(conn);
        build_morph_fst(&morph_db, &fst_path).unwrap();
        let morph = Morph::open(&fst_path).unwrap();

        for (name, body) in [
            ("t1.txt", "книги"),
            ("t2.txt", "книгами"),
            ("t3.txt", "книга"),
            ("t4.txt", "книгалово"),
            ("t5.txt", "Ёлка"),
            ("t6.txt", "report"),
            ("t7.txt", "reportage"),
            ("t8.txt", "синяя тетрадь"),
            ("t9.txt", "синяя тетради"),
            ("книгами.txt", "нейтральный текст"),
        ] {
            fs::write(source_dir.join(name), body).unwrap();
        }

        let settings = Settings {
            data_dir: data_dir.clone(),
            enable_morphology: true,
            sources: vec![Source {
                path: source_dir,
                mode: SourceMode::Full,
                max_depth: 0,
                enabled: true,
                volume_serial: None,
                extra_excludes: Vec::new(),
            }],
            ..Default::default()
        };
        let paths = AppPaths {
            settings_file: data_dir.join("settings.toml"),
            catalog_db: data_dir.join("catalog.sqlite3"),
            index_dir: data_dir.join("index"),
            data_dir,
            logs_dir,
        };
        let options = PipelineOptions {
            queue_capacity: 8,
            in_process_workers: true,
            morph: Some(morph.clone()),
            ..Default::default()
        };
        sync_all_with_options(&settings, options).expect("sync_all");
        Self {
            _dir: dir,
            paths,
            settings,
            morph,
        }
    }

    fn engine(&self, with_morph: bool) -> SearchEngine {
        let morph = with_morph.then(|| self.morph.clone());
        SearchEngine::open(&self.paths, &self.settings, morph).expect("open search engine")
    }
}

/// Имена файлов в выдаче, отсортированные.
fn found_files(engine: &SearchEngine, text: &str, forms: WordFormsMode) -> Vec<String> {
    let resp = engine.search(SearchRequest::new(text).with_forms(forms));
    let mut names: Vec<String> = resp
        .items
        .iter()
        .flat_map(|it| it.paths.iter())
        .map(|p| {
            std::path::Path::new(p)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn exact_forms_mode_finds_only_whole_typed_word_in_text() {
    let fixture = FormsFixture::new();
    let engine = fixture.engine(true);

    // Точно: только «книга» (ни «книги», ни «книгами», ни «книгалово», ни имя «книгами.txt»)
    assert_eq!(
        found_files(&engine, "книга", WordFormsMode::Exact),
        vec!["t3.txt"]
    );
    assert_eq!(
        found_files(&engine, "КНИГА", WordFormsMode::Exact),
        vec!["t3.txt"]
    );
    // Другая форма ищется ровно в своём виде (текст t2 и имя файла «книгами.txt»)
    assert_eq!(
        found_files(&engine, "книгами", WordFormsMode::Exact),
        vec!["t2.txt", "книгами.txt"]
    );
    // Все формы со словарём: книги, книгами, книга и файл «книгами.txt»; «книгалово» не форма слова
    assert_eq!(
        found_files(&engine, "книга", WordFormsMode::AllForms),
        vec!["t1.txt", "t2.txt", "t3.txt", "книгами.txt"]
    );
    // Умолчание запроса — все формы
    assert_eq!(SearchRequest::new("книга").forms, WordFormsMode::AllForms);

    // «ё» нормализуется как при индексации
    assert_eq!(
        found_files(&engine, "елка", WordFormsMode::Exact),
        vec!["t5.txt"]
    );
    assert_eq!(
        found_files(&engine, "ёлка", WordFormsMode::Exact),
        vec!["t5.txt"]
    );
}

#[test]
fn exact_forms_mode_is_whole_word_without_dictionary_and_for_latin() {
    let fixture = FormsFixture::new();
    for with_morph in [true, false] {
        let engine = fixture.engine(with_morph);
        // Латиница: целое слово, без «reportage»
        assert_eq!(
            found_files(&engine, "report", WordFormsMode::Exact),
            vec!["t6.txt"]
        );
        // Без префикса: «книг» — не целое слово
        assert!(found_files(&engine, "книг", WordFormsMode::Exact).is_empty());
        assert_eq!(
            found_files(&engine, "книга", WordFormsMode::Exact),
            vec!["t3.txt"]
        );
    }
    // Старое поведение без словаря: префикс находит продолжения
    let engine = fixture.engine(false);
    assert_eq!(
        found_files(&engine, "report", WordFormsMode::AllForms),
        vec!["t6.txt", "t7.txt"]
    );
    assert_eq!(
        found_files(&engine, "книга", WordFormsMode::AllForms),
        vec!["t2.txt", "t3.txt", "t4.txt", "книгами.txt"]
    );
}

#[test]
fn exact_forms_mode_applies_to_each_word_of_phrase() {
    let fixture = FormsFixture::new();
    let engine = fixture.engine(true);
    for mode in [MultiWordMode::StrictPhrase, MultiWordMode::AllWords] {
        let exact = engine.search(
            SearchRequest::new("синяя тетрадь")
                .with_mode(mode)
                .with_forms(WordFormsMode::Exact),
        );
        assert_eq!(exact.total, 1, "{mode:?}");
        let all = engine.search(
            SearchRequest::new("синяя тетрадь")
                .with_mode(mode)
                .with_forms(WordFormsMode::AllForms),
        );
        assert_eq!(all.total, 2, "{mode:?}");
    }
}

#[test]
fn exact_forms_mode_from_settings_applies_to_plain_string_queries() {
    let fixture = FormsFixture::new();
    let mut settings = fixture.settings.clone();
    settings.word_forms_mode = WordFormsMode::Exact;
    let engine =
        SearchEngine::open(&fixture.paths, &settings, Some(fixture.morph.clone())).unwrap();
    assert_eq!(engine.search("книга").total, 1);

    let engine = fixture.engine(true);
    assert_eq!(engine.search("книга").total, 4);
}

#[test]
fn exact_forms_mode_snippet_highlights_only_typed_form() {
    let fixture = FormsFixture::new();
    let engine = fixture.engine(true);
    let req = SearchRequest::new("книга").with_forms(WordFormsMode::Exact);
    let resp = engine.search(&req);
    assert_eq!(resp.total, 1);
    let snip = engine.snippet_for(&resp.items[0].item_key, &req);
    assert!(snip.to_marked_string().contains("⟦книга⟧"));
}

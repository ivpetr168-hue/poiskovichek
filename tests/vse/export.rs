use std::fs;
use tempfile::tempdir;

use poiskovichek::catalog::{Catalog, FileRecord};
use poiskovichek::export::{export_reports, format_iso8601_utc};
use windows::Win32::Foundation::SYSTEMTIME;

fn make_test_record(id: i64, path: &str, name: &str, category: &str, size: u64) -> FileRecord {
    FileRecord {
        id: Some(id),
        path: path.to_string(),
        source_id: None,
        volume_id: None,
        rel_path: name.to_string(),
        name: name.to_string(),
        extension: if name.contains('.') {
            format!(".{}", name.split('.').next_back().unwrap())
        } else {
            String::new()
        },
        size,
        modified_ns: 1_700_000_000_000_000_000,
        created_ns: 1_700_000_000_000_000_000,
        category: category.to_string(),
        inside_archive: false,
        container_id: None,
        content_status: Some("text".to_string()),
        extraction_version: 1,
        text_sha: None,
        quick_hash: None,
        sha256: None,
        error: None,
        scan_run: Some(1),
        available: true,
    }
}

#[test]
fn test_export_reports_contains_catalogued_file() {
    // R1-Б-54: вызов export_reports -> формируются files.csv, duplicates.csv, summary.json, report.html,
    // в files.csv нет колонки text_excerpt, присутствуют файлы из базы.
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let rec = make_test_record(1, r"C:\Книга.pdf", "Книга.pdf", "книга", 1024);
    catalog.upsert_files(&[rec]).unwrap();
    drop(catalog);

    let output_dir = dir.path().join("reports");
    let summary = export_reports(&db_path, &output_dir).expect("export_reports");

    assert_eq!(summary.total_files, 1);
    assert_eq!(summary.total_bytes, 1024);

    // files.csv: UTF-8 с BOM, разделитель ;
    let files_csv_bytes = fs::read(output_dir.join("files.csv")).unwrap();
    assert_eq!(
        &files_csv_bytes[..3],
        &[0xEF, 0xBB, 0xBF],
        "Файл files.csv должен начинаться с UTF-8 BOM"
    );
    let files_csv_str = String::from_utf8(files_csv_bytes[3..].to_vec()).unwrap();
    assert!(files_csv_str.contains(r"C:\Книга.pdf"));
    let first_line = files_csv_str.lines().next().unwrap();
    assert!(
        !first_line.contains("text_excerpt"),
        "В заголовке files.csv не должно быть колонки text_excerpt"
    );
    assert!(first_line.contains("path;root;name;extension;size;"));

    // duplicates.csv
    let dup_csv_bytes = fs::read(output_dir.join("duplicates.csv")).unwrap();
    assert_eq!(&dup_csv_bytes[..3], &[0xEF, 0xBB, 0xBF]);

    // summary.json
    let summary_json = fs::read_to_string(output_dir.join("summary.json")).unwrap();
    assert!(summary_json.contains("\"total_files\": 1"));
    assert!(summary_json.contains("\"total_bytes\": 1024"));
    assert!(summary_json.contains(&format!("\"generated_at\": \"{}\"", summary.generated_at)));

    // report.html
    let html = fs::read_to_string(output_dir.join("report.html")).unwrap();
    assert!(html.contains("Открыть поиск.cmd"));
    assert!(html.contains("files.csv"));
    assert!(html.contains("#020507")); // темный терминальный фон
    assert!(html.contains("#22f2ff")); // цвет текста
}

#[test]
fn test_cli_export_runs_without_scanning_roots() {
    // R1-Б-55: выгрузка отчетов работает исключительно по базе данных без обращения к дискам
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("catalog.sqlite3");

    // Создаем базу с файлом, путь которого заведомо не существует на диске
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();
    let rec = make_test_record(
        1,
        r"Z:\NonExistentDrive\file.txt",
        "file.txt",
        "документ",
        500,
    );
    catalog.upsert_files(&[rec]).unwrap();
    drop(catalog);

    let output_dir = dir.path().join("reports_offline");
    // Вызов не должен упасть из-за отсутствия диска Z:
    let summary = export_reports(&db_path, &output_dir).unwrap();
    assert_eq!(summary.total_files, 1);
    assert!(output_dir.join("report.html").exists());
}

#[test]
fn test_csv_escaping_semicolon_and_quotes() {
    // CSV открывается как UTF-8 с BOM, разделитель ;, кавычки и ; в имени экранированы
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let tricky_name = r#"отчет; за "2026" год.txt"#;
    let tricky_path = format!(r#"C:\docs\{tricky_name}"#);

    let rec = make_test_record(1, &tricky_path, tricky_name, "документ", 100);
    catalog.upsert_files(&[rec]).unwrap();
    drop(catalog);

    let output_dir = dir.path().join("reports");
    export_reports(&db_path, &output_dir).unwrap();

    let bytes = fs::read(output_dir.join("files.csv")).unwrap();
    let text = String::from_utf8(bytes[3..].to_vec()).unwrap();

    // Проверяем экранирование: кавычки удваиваются, поле оборачивается в кавычки
    assert!(
        text.contains(r#""отчет; за ""2026"" год.txt""#),
        "Кавычки и точки с запятой должны быть экранированы по стандарту CSV: {text}"
    );
}

#[test]
fn test_html_escaping_script_tag() {
    // HTML: имя <script> выводится экранированным (&lt;script&gt;)
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("catalog.sqlite3");
    let mut catalog = Catalog::open_or_create(&db_path).unwrap();

    let xss_name = "<script>alert('xss')</script>.pdf";
    let xss_path = format!(r"C:\test\{xss_name}");

    let rec = make_test_record(1, &xss_path, xss_name, "<кат>", 2048);
    catalog.upsert_files(&[rec]).unwrap();
    drop(catalog);

    let output_dir = dir.path().join("reports");
    export_reports(&db_path, &output_dir).unwrap();

    let html = fs::read_to_string(output_dir.join("report.html")).unwrap();

    assert!(
        !html.contains("<script>alert"),
        "HTML не должен содержать сырой тег <script>"
    );
    assert!(
        html.contains("&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;.pdf"),
        "Имя со скриптом должно быть корректно HTML-экранировано: {html}"
    );
    assert!(
        html.contains("&lt;кат&gt;"),
        "Категория должна быть экранирована"
    );
}

#[test]
fn test_format_iso8601_utc_fixed_time() {
    let st = SYSTEMTIME {
        wYear: 2026,
        wMonth: 10,
        wDayOfWeek: 2,
        wDay: 7,
        wHour: 9,
        wMinute: 5,
        wSecond: 3,
        wMilliseconds: 999,
    };
    assert_eq!(format_iso8601_utc(&st), "2026-10-07T09:05:03Z");
}

#[test]
fn test_generated_at_is_real_iso8601_utc() {
    // Раньше значение выглядело как «1970-01-01T00:00:00Z+1798000000».
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("catalog.sqlite3");
    drop(Catalog::open_or_create(&db_path).unwrap());

    let summary = export_reports(&db_path, &dir.path().join("reports")).unwrap();
    let s = summary.generated_at;
    let b = s.as_bytes();
    assert_eq!(s.len(), 20, "формат ГГГГ-ММ-ДДTЧЧ:ММ:ССZ: {s}");
    assert!(s.ends_with('Z'), "{s}");
    for (i, c) in b.iter().enumerate() {
        match i {
            4 | 7 => assert_eq!(*c, b'-', "{s}"),
            10 => assert_eq!(*c, b'T', "{s}"),
            13 | 16 => assert_eq!(*c, b':', "{s}"),
            19 => assert_eq!(*c, b'Z', "{s}"),
            _ => assert!(c.is_ascii_digit(), "{s}"),
        }
    }
    let year: u32 = s[..4].parse().unwrap();
    assert!(year >= 2026, "год не должен быть 1970: {s}");
    assert!(s[5..7].parse::<u32>().unwrap() <= 12);
    assert!(s[8..10].parse::<u32>().unwrap() <= 31);
    assert!(s[11..13].parse::<u32>().unwrap() <= 23);
}

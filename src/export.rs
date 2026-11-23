use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use rusqlite::Connection;
use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::SystemInformation::GetSystemTime;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RootStats {
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CategoryStats {
    pub files: u64,
    pub bytes: u64,
}

/// Сводная статистика каталога для summary.json
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExportSummary {
    pub generated_at: String,
    pub total_files: u64,
    pub total_bytes: u64,
    pub by_root: BTreeMap<String, RootStats>,
    pub by_category: BTreeMap<String, CategoryStats>,
    pub errors: u64,
    pub needs_ocr: u64,
    pub duplicate_groups: u64,
    pub duplicate_files: u64,
    pub duplicate_reclaimable_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("Ошибка базы данных SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Ошибка ввода-вывода при выгрузке отчетов: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ошибка сериализации JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Форматирует время UTC в ISO 8601: `ГГГГ-ММ-ДДTЧЧ:ММ:ССZ`, например `2026-10-27T09:05:03Z`.
/// Время берётся тем же способом, что и в журнале (SYSTEMTIME из Windows), но в UTC.
pub fn format_iso8601_utc(st: &SYSTEMTIME) -> String {
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

/// Экранирует строковое значение поля для CSV с разделителем `;`.
/// Если поле содержит точку с запятой, кавычки, перевод строки или возврат каретки,
/// оно оборачивается в двойные кавычки, а внутренние кавычки удваиваются.
pub fn escape_csv(val: &str) -> String {
    if val.contains(';') || val.contains('"') || val.contains('\n') || val.contains('\r') {
        let mut escaped = String::with_capacity(val.len() + 8);
        escaped.push('"');
        for ch in val.chars() {
            if ch == '"' {
                escaped.push('"');
                escaped.push('"');
            } else {
                escaped.push(ch);
            }
        }
        escaped.push('"');
        escaped
    } else {
        val.to_string()
    }
}

/// Экранирует HTML-спецсимволы для безопасного вывода в отчет.
pub fn escape_html(val: &str) -> String {
    let mut escaped = String::with_capacity(val.len());
    for ch in val.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

/// Выгружает перечень файлов и отчеты по данным базы данных каталога (R1-А-151...153, R1-Б-54, 55).
///
/// Создает файлы в `out_dir`:
/// - `files.csv` (разделитель `;`, UTF-8 с BOM, без текста документа);
/// - `duplicates.csv` (разделитель `;`, UTF-8 с BOM);
/// - `summary.json` (сводка по источникам, категориям, ошибкам, дубликатам, сканам без текста);
/// - `report.html` (терминальный стиль: фон `#020507`, текст `#22f2ff`, моноширинный шрифт,
///   таблицы категорий и 100 крупнейших файлов, все экранировано).
///
/// Источники и файлы на диске НЕ читаются — только SQLite база.
pub fn export_reports(catalog_path: &Path, out_dir: &Path) -> Result<ExportSummary, ExportError> {
    fs::create_dir_all(out_dir)?;

    let conn = Connection::open_with_flags(
        catalog_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(30))?;

    // 1. Сбор агрегатов для summary.json
    let (total_files, total_bytes): (i64, i64) = conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(size), 0) FROM files;",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;

    // by_root
    let mut by_root: BTreeMap<String, RootStats> = BTreeMap::new();
    {
        let mut root_stmt = conn.prepare(
            r#"
            SELECT COALESCE(s.path, SUBSTR(f.path, 1, 2)) AS root,
                   COUNT(*) AS n,
                   COALESCE(SUM(f.size), 0) AS bytes
            FROM files f
            LEFT JOIN sources s ON f.source_id = s.id
            GROUP BY root
            ORDER BY root;
            "#,
        )?;

        let mut rows = root_stmt.query([])?;
        while let Some(row) = rows.next()? {
            let root: String = row.get(0)?;
            let files: i64 = row.get(1)?;
            let bytes: i64 = row.get(2)?;
            by_root.insert(
                root,
                RootStats {
                    files: files as u64,
                    bytes: bytes as u64,
                },
            );
        }
    }

    // by_category
    let mut by_category: BTreeMap<String, CategoryStats> = BTreeMap::new();
    let mut category_order: Vec<(String, u64, u64)> = Vec::new();
    {
        let mut cat_stmt = conn.prepare(
            r#"
            SELECT category, COUNT(*) AS n, COALESCE(SUM(size), 0) AS bytes
            FROM files
            GROUP BY category
            ORDER BY bytes DESC, category ASC;
            "#,
        )?;

        let mut rows = cat_stmt.query([])?;
        while let Some(row) = rows.next()? {
            let cat: String = row.get(0)?;
            let files: i64 = row.get(1)?;
            let bytes: i64 = row.get(2)?;
            category_order.push((cat.clone(), files as u64, bytes as u64));
            by_category.insert(
                cat,
                CategoryStats {
                    files: files as u64,
                    bytes: bytes as u64,
                },
            );
        }
    }

    let errors: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE error IS NOT NULL;",
        [],
        |row| row.get(0),
    )?;

    let needs_ocr: i64 = conn.query_row(
        "SELECT COUNT(*) FROM files WHERE content_status = 'scan-needs-ocr';",
        [],
        |row| row.get(0),
    )?;

    let (duplicate_groups, duplicate_files, duplicate_reclaimable_bytes): (i64, i64, i64) = conn
        .query_row(
            r#"
            SELECT COUNT(*),
                   COALESCE(SUM(file_count), 0),
                   COALESCE(SUM(reclaimable_bytes), 0)
            FROM duplicate_groups;
            "#,
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap_or((0, 0, 0));

    let summary = ExportSummary {
        generated_at: format_iso8601_utc(&unsafe { GetSystemTime() }),
        total_files: total_files as u64,
        total_bytes: total_bytes as u64,
        by_root,
        by_category,
        errors: errors as u64,
        needs_ocr: needs_ocr as u64,
        duplicate_groups: duplicate_groups as u64,
        duplicate_files: duplicate_files as u64,
        duplicate_reclaimable_bytes: duplicate_reclaimable_bytes as u64,
    };

    // 2. Генерация files.csv (UTF-8 BOM, разделитель ;, без text_excerpt)
    const UTF8_BOM: &[u8] = &[0xEF, 0xBB, 0xBF];
    {
        let mut files_csv = File::create(out_dir.join("files.csv"))?;
        files_csv.write_all(UTF8_BOM)?;

        let header = "path;root;name;extension;size;modified_ns;category;mime_type;sha256;content_status;error\r\n";
        files_csv.write_all(header.as_bytes())?;

        let mut files_stmt = conn.prepare(
            r#"
            SELECT
                f.path,
                COALESCE(s.path, SUBSTR(f.path, 1, 2)) AS root,
                f.name,
                f.extension,
                f.size,
                f.modified_ns,
                f.category,
                '' AS mime_type,
                COALESCE(f.sha256, '') AS sha256,
                COALESCE(f.content_status, '') AS content_status,
                COALESCE(f.error, '') AS error
            FROM files f
            LEFT JOIN sources s ON f.source_id = s.id
            ORDER BY f.path ASC;
            "#,
        )?;

        let mut rows = files_stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let root: String = row.get(1)?;
            let name: String = row.get(2)?;
            let ext: String = row.get(3)?;
            let size: i64 = row.get(4)?;
            let mtime: i64 = row.get(5)?;
            let cat: String = row.get(6)?;
            let mime: String = row.get(7)?;
            let sha: String = row.get(8)?;
            let status: String = row.get(9)?;
            let err: String = row.get(10)?;

            let line = format!(
                "{};{};{};{};{};{};{};{};{};{};{}\r\n",
                escape_csv(&path),
                escape_csv(&root),
                escape_csv(&name),
                escape_csv(&ext),
                size,
                mtime,
                escape_csv(&cat),
                escape_csv(&mime),
                escape_csv(&sha),
                escape_csv(&status),
                escape_csv(&err)
            );
            files_csv.write_all(line.as_bytes())?;
        }
    }

    // 3. Генерация duplicates.csv (UTF-8 BOM, разделитель ;)
    {
        let mut dup_csv = File::create(out_dir.join("duplicates.csv"))?;
        dup_csv.write_all(UTF8_BOM)?;

        let header = "sha256;size;path\r\n";
        dup_csv.write_all(header.as_bytes())?;

        let mut dup_stmt = conn.prepare(
            r#"
            SELECT sha256, size, path
            FROM files
            WHERE sha256 IN (SELECT sha256 FROM duplicate_groups)
            ORDER BY sha256 ASC, path ASC;
            "#,
        )?;

        let mut rows = dup_stmt.query([])?;
        while let Some(row) = rows.next()? {
            let sha: String = row.get(0)?;
            let size: i64 = row.get(1)?;
            let path: String = row.get(2)?;

            let line = format!("{};{};{}\r\n", escape_csv(&sha), size, escape_csv(&path));
            dup_csv.write_all(line.as_bytes())?;
        }
    }

    // 4. Генерация summary.json
    {
        let json_str = serde_json::to_string_pretty(&summary)?;
        fs::write(out_dir.join("summary.json"), json_str.as_bytes())?;
    }

    // 5. Генерация report.html (терминальный стиль: #020507, текст #22f2ff)
    {
        let mut largest_stmt = conn.prepare(
            r#"
            SELECT path, size, category
            FROM files
            ORDER BY size DESC
            LIMIT 100;
            "#,
        )?;

        let mut largest_rows_html = String::new();
        let mut rows = largest_stmt.query([])?;
        while let Some(row) = rows.next()? {
            let path: String = row.get(0)?;
            let size: i64 = row.get(1)?;
            let cat: String = row.get(2)?;

            largest_rows_html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                escape_html(&path),
                size,
                escape_html(&cat)
            ));
        }

        let mut category_rows_html = String::new();
        for (cat, count, bytes) in &category_order {
            category_rows_html.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                escape_html(cat),
                count,
                bytes
            ));
        }

        let html = format!(
            r#"<!doctype html>
<html lang="ru">
<head>
  <meta charset="utf-8">
  <title>ПоисКовичеК — Каталог файлов</title>
  <style>
    body {{
      background-color: #020507;
      color: #22f2ff;
      font-family: Consolas, 'Courier New', monospace;
      padding: 24px;
      margin: 0;
      line-height: 1.5;
    }}
    h1, h2 {{
      color: #b6ff3b;
      font-family: inherit;
      border-bottom: 1px solid #1a3a40;
      padding-bottom: 6px;
    }}
    table {{
      border-collapse: collapse;
      width: 100%;
      margin: 16px 0 32px 0;
      font-size: 14px;
    }}
    th, td {{
      border: 1px solid #14353d;
      padding: 8px 12px;
      text-align: left;
    }}
    th {{
      background-color: #051419;
      color: #b6ff3b;
    }}
    tr:nth-child(even) {{
      background-color: #040c10;
    }}
    b {{
      color: #b6ff3b;
    }}
    code {{
      background-color: #051820;
      color: #22f2ff;
      padding: 2px 6px;
      border-radius: 3px;
      white-space: nowrap;
    }}
  </style>
</head>
<body>
  <h1>Каталог информационного архива</h1>
  <p>
    Всего файлов: <b>{total_files}</b> ({total_bytes} байт).
    Ошибок чтения: <b>{errors}</b>.
    Групп дубликатов: <b>{duplicate_groups}</b> (высвобождаемо: <b>{duplicate_reclaimable_bytes}</b> байт).
  </p>

  <h2>Категории</h2>
  <table>
    <thead>
      <tr><th>Категория</th><th>Файлов</th><th>Байт</th></tr>
    </thead>
    <tbody>
{category_rows_html}    </tbody>
  </table>

  <h2>100 крупнейших файлов</h2>
  <table>
    <thead>
      <tr><th>Путь</th><th>Байт</th><th>Категория</th></tr>
    </thead>
    <tbody>
{largest_rows_html}    </tbody>
  </table>

  <p>
    <b>Поиск:</b> откройте файл <code>Открыть поиск.cmd</code> в папке проекта.
    Полный перечень: <code>files.csv</code>.
    Точные дубликаты: <code>duplicates.csv</code>.
    Рабочая база: <code>catalog.sqlite3</code>.
  </p>
</body>
</html>
"#,
            total_files = summary.total_files,
            total_bytes = summary.total_bytes,
            errors = summary.errors,
            duplicate_groups = summary.duplicate_groups,
            duplicate_reclaimable_bytes = summary.duplicate_reclaimable_bytes,
            category_rows_html = category_rows_html,
            largest_rows_html = largest_rows_html,
        );

        fs::write(out_dir.join("report.html"), html.as_bytes())?;
    }

    Ok(summary)
}

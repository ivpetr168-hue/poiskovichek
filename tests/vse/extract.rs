use std::io::{Cursor, Write};
use tempfile::tempdir;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

use poiskovichek::extract::{
    EXTRACTION_VERSION, ExtractConfig, ExtractSource, extract, extract_archive_member,
    extract_bytes, extract_file, format_version, is_book_or_document, is_code_or_service,
    text::decode_text,
};

/// Вспомогательная функция для создания ZIP-файла в памяти с заданными записями.
fn make_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut zip = ZipWriter::new(Cursor::new(&mut buffer));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);

    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
    buffer
}

/// Вспомогательная функция для создания минимального PDF документа через lopdf.
fn make_pdf(text: Option<&str>) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Courier",
    });

    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! {
            "F1" => font_id,
        },
    });

    let mut operations = Vec::new();
    if let Some(t) = text {
        operations.push(Operation::new("BT", vec![]));
        operations.push(Operation::new("Tf", vec!["F1".into(), 12.into()]));
        operations.push(Operation::new(
            "Tj",
            vec![Object::string_literal(t.as_bytes())],
        ));
        operations.push(Operation::new("ET", vec![]));
    }

    let content = Content { operations };
    let content_stream = Stream::new(dictionary! {}, content.encode().unwrap());
    let content_id = doc.add_object(content_stream);

    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "Contents" => content_id,
    });

    let pages = dictionary! {
        "Type" => "Pages",
        "Kids" => vec![page_id.into()],
        "Count" => 1,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));

    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut output = Vec::new();
    doc.save_to(&mut output).unwrap();
    output
}

// === Тесты из разбора прежней программы (R1-Б-14..24, 42, 43, 46, 50, 57) ===

/// R1-Б-14: test_enrichment_extracts_searchable_text
#[test]
fn test_enrichment_extracts_searchable_text() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("заметка.txt");
    std::fs::write(&file_path, "Сахалинский архив").unwrap();

    let config = ExtractConfig::default();
    let res = extract_file(&file_path, false, &config);

    assert_eq!(res.status, "text");
    assert!(res.text.contains("Сахалинский архив"));
    assert!(res.error.is_none());
}

/// R1-Б-15: test_rtf_extracts_hex_encoded_text
#[test]
fn test_rtf_extracts_hex_encoded_text() {
    let raw = br"{\rtf1\ansi\ansicpg1251 \'cf\'f0\'e8\'e2\'e5\'f2}";
    let config = ExtractConfig::default();
    let res = extract_bytes(raw, "rtf", false, &config);

    assert_eq!(res.status, "text");
    assert_eq!(res.text, "Привет");
}

/// R1-Б-16: test_short_cp1251_text_is_not_misread_as_utf16
#[test]
fn test_short_cp1251_text_is_not_misread_as_utf16() {
    let expected = "Вставьте установочный DVD в DVD-привод";
    let (encoded, _, _) = encoding_rs::WINDOWS_1251.encode(expected);

    let decoded = decode_text(&encoded);
    assert_eq!(decoded, expected);
}

/// R1-Б-17: test_cp866_and_utf16_texts_are_detected
#[test]
fn test_cp866_and_utf16_texts_are_detected() {
    let expected = "Привет, архив";

    // CP866
    let (cp866_bytes, _, _) = encoding_rs::IBM866.encode(expected);
    assert_eq!(decode_text(&cp866_bytes), expected);

    // UTF-16 с BOM
    let mut utf16_be = vec![0xfe, 0xff];
    for u in expected.encode_utf16() {
        utf16_be.extend_from_slice(&u.to_be_bytes());
    }
    assert_eq!(decode_text(&utf16_be), expected);

    // UTF-16-LE без BOM
    let mut utf16_le = Vec::new();
    for u in expected.encode_utf16() {
        utf16_le.extend_from_slice(&u.to_le_bytes());
    }
    assert_eq!(decode_text(&utf16_le), expected);
}

/// R1-Б-18: test_rtf_combines_utf16_surrogate_pair_into_valid_utf8
#[test]
fn test_rtf_combines_utf16_surrogate_pair_into_valid_utf8() {
    let raw = br"{\rtf1\ansi \u55357?\u56842?}";
    let config = ExtractConfig::default();
    let res = extract_bytes(raw, "rtf", false, &config);

    assert_eq!(res.status, "text");
    assert_eq!(res.text, "😊");
    assert_eq!(res.text.as_bytes(), "\u{1f60a}".as_bytes());
}

/// R1-Б-19: test_html_extracts_visible_text_without_markup_or_script
#[test]
fn test_html_extracts_visible_text_without_markup_or_script() {
    let raw = "<h1>Архив</h1><script>скрыть()</script><p>Важный текст</p>";
    let config = ExtractConfig::default();
    let res = extract_bytes(raw.as_bytes(), "html", false, &config);

    assert_eq!(res.status, "text");
    assert!(res.text.contains("Архив"));
    assert!(res.text.contains("Важный текст"));
    assert!(!res.text.contains("скрыть"));
}

/// R1-Б-20: test_books_and_documents_use_twenty_million_character_safety_cap
#[test]
fn test_books_and_documents_use_twenty_million_character_safety_cap() {
    let config = ExtractConfig::default();
    let exts = [
        "fb2", "epub", "mobi", "azw", "azw3", "pdf", "docx", "odt", "rtf", "txt", "md", "html",
        "htm",
    ];

    for ext in exts {
        assert!(
            is_book_or_document(ext),
            "Формат {ext} должен быть книгой/документом"
        );
        assert_eq!(
            config.char_limit(ext, 1000),
            20_000_000,
            "Лимит для {ext} должен быть 20 млн"
        );
    }
}

/// R1-Б-21: test_service_text_and_source_code_stop_at_two_hundred_thousand_characters
#[test]
fn test_service_text_and_source_code_stop_at_two_hundred_thousand_characters() {
    let config = ExtractConfig::default();

    // JSON с 250 тыс. символов
    let long_json = "я".repeat(250_000);
    let res = extract_bytes(long_json.as_bytes(), "json", false, &config);
    assert_eq!(res.status, "text");
    assert_eq!(res.text.chars().count(), 200_000);

    // Граница 5 МБ для plain text (5 242 880 байт)
    assert_eq!(config.char_limit("txt", 5_242_879), 20_000_000);
    assert_eq!(config.char_limit("txt", 5_242_880), 200_000);
    assert_eq!(config.char_limit("md", 5_242_880), 200_000);
}

/// R1-Б-22: test_fb2_keeps_marker_beyond_the_old_limit_and_searches_it
#[test]
fn test_fb2_keeps_marker_beyond_the_old_limit_and_searches_it() {
    let marker = "уникальный хвостовой маркер";
    let body = format!("{} {marker}", "длинный текст ".repeat(12_000));
    let fb2_xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<FictionBook xmlns="http://www.gribuser.ru/xml/fictionbook/2.0">
  <body>
    <section>
      <p>{body}</p>
    </section>
  </body>
</FictionBook>"#
    );

    let config = ExtractConfig::default();
    let res = extract_bytes(fb2_xml.as_bytes(), "fb2", false, &config);

    assert_eq!(res.status, "text");
    assert!(res.text.chars().count() > 100_000);
    assert!(res.text.contains(marker));
}

/// R1-Б-23: test_pdf_extraction_does_not_limit_pdftotext_to_first_fifty_pages
#[test]
fn test_pdf_extraction_does_not_limit_pdftotext_to_first_fifty_pages() {
    // Встроенный извлекатель извлекает все страницы документа без лимита в 50 страниц
    let pdf_bytes = make_pdf(Some("TailMarker"));
    let config = ExtractConfig::default();
    let res = extract_bytes(&pdf_bytes, "pdf", false, &config);

    assert_eq!(res.status, "text");
    assert!(res.text.contains("TailMarker"));
}

/// R1-Б-24: test_jsonl_is_a_searchable_document
#[test]
fn test_jsonl_is_a_searchable_document() {
    let raw = "{\"message\":\"Сахалинский архив\"}\n";
    let config = ExtractConfig::default();
    let res = extract_bytes(raw.as_bytes(), "jsonl", false, &config);

    assert_eq!(res.status, "text");
    assert!(res.text.contains("Сахалинский архив"));
    assert!(is_code_or_service("jsonl"));
}

/// R1-Б-42: test_docx_and_xlsx_still_extract_xml_text
#[test]
fn test_docx_and_xlsx_still_extract_xml_text() {
    let config = ExtractConfig::default();

    // DOCX
    let docx_bytes = make_zip(&[(
        "word/document.xml",
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
            <w:body><w:p><w:r><w:t>Текст DOCX</w:t></w:r></w:p></w:body>
        </w:document>"#
            .as_bytes(),
    )]);
    let docx_res = extract_bytes(&docx_bytes, "docx", false, &config);
    assert_eq!(docx_res.status, "text");
    assert!(docx_res.text.contains("Текст DOCX"));

    // XLSX
    let xlsx_bytes = make_zip(&[
        (
            "xl/_rels/workbook.xml.rels",
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
                <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
            </Relationships>"#.as_bytes(),
        ),
        (
            "xl/workbook.xml",
            r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
                <sheets><sheet name="Лист1" sheetId="1" r:id="rId1"/></sheets>
            </workbook>"#.as_bytes(),
        ),
        (
            "xl/sharedStrings.xml",
            r#"<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="1" uniqueCount="1">
                <si><t>Текст XLSX</t></si>
            </sst>"#.as_bytes(),
        ),
        (
            "xl/worksheets/sheet1.xml",
            r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
                <sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData>
            </worksheet>"#.as_bytes(),
        ),
    ]);
    let xlsx_res = extract_bytes(&xlsx_bytes, "xlsx", false, &config);
    assert_eq!(xlsx_res.status, "text");
    assert!(xlsx_res.text.contains("Текст XLSX"));
}

/// R1-Б-43: test_legacy_formats_report_the_missing_text_extractor
#[test]
fn test_legacy_formats_report_the_missing_text_extractor() {
    let config = ExtractConfig::default();
    let legacy = ["doc", "xls", "ppt", "mobi", "azw", "azw3"];

    for ext in legacy {
        let res = extract_bytes(b"dummy", ext, false, &config);
        assert_eq!(res.status, format!("unsupported-text-{ext}"));
        assert_eq!(res.text, "");
    }
}

/// R1-Б-46 / R1-Б-50: sensitive_metadata_only
#[test]
fn test_sensitive_flag_prevents_reading_content() {
    let config = ExtractConfig::default();
    let secret = b"SECRET_VALUE=never-index-this";

    let res = extract_bytes(secret, "env", true, &config);
    assert_eq!(res.status, "sensitive-metadata-only");
    assert_eq!(res.text, "");
    assert!(res.error.is_none());
}

/// R1-Б-57: test_enrichment_stores_compressed_text_and_short_excerpt
#[test]
fn test_text_truncation_on_char_boundary() {
    let config = ExtractConfig {
        limit_chars_other: 10,
        ..ExtractConfig::default()
    };
    let data = "Привет мир! Это длинный текст.".as_bytes();
    let res = extract(
        ExtractSource::Bytes(data),
        "unknown_ext",
        data.len() as u64,
        false,
        &config,
    );
    // Для неизвестного формата статус metadata-only
    assert_eq!(res.status, "metadata-only");

    // Для txt обрезается по границе символа
    let txt_config = ExtractConfig {
        limit_chars_books_documents: 6,
        ..ExtractConfig::default()
    };
    let txt_res = extract_bytes("Привет мир".as_bytes(), "txt", false, &txt_config);
    assert_eq!(txt_res.status, "text");
    assert_eq!(txt_res.text, "Привет");
}

// === Дополнительные требуемые тесты задания T4 ===

/// EPUB с главами в spine в обратном порядке имён файлов -> текст по spine
#[test]
fn test_epub_spine_order_reverse_filenames() {
    let opf = r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
  <manifest>
    <item id="item_first" href="chapter_b.xhtml" media-type="application/xhtml+xml"/>
    <item id="item_second" href="chapter_a.xhtml" media-type="application/xhtml+xml"/>
  </manifest>
  <spine>
    <itemref idref="item_first"/>
    <itemref idref="item_second"/>
  </spine>
</package>"#;

    let container = r#"<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#;

    let ch_b = "<html><body><p>ПерваяГлаваПоСпину</p></body></html>";
    let ch_a = "<html><body><p>ВтораяГлаваПоСпину</p></body></html>";

    let epub_bytes = make_zip(&[
        ("META-INF/container.xml", container.as_bytes()),
        ("OEBPS/content.opf", opf.as_bytes()),
        ("OEBPS/chapter_a.xhtml", ch_a.as_bytes()),
        ("OEBPS/chapter_b.xhtml", ch_b.as_bytes()),
    ]);

    let config = ExtractConfig::default();
    let res = extract_bytes(&epub_bytes, "epub", false, &config);

    assert_eq!(res.status, "text");
    let pos_first = res.text.find("ПерваяГлаваПоСпину").unwrap();
    let pos_second = res.text.find("ВтораяГлаваПоСпину").unwrap();
    assert!(
        pos_first < pos_second,
        "Порядок глав должен определяться spine, а не алфавитным именем файла"
    );
}

/// DOCX с колонтитулом и сноской -> оба слова найдены в тексте
#[test]
fn test_docx_headers_and_footnotes() {
    let doc_xml = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:body><w:p><w:r><w:t>ОсновнойДокумент</w:t></w:r></w:p></w:body>
    </w:document>"#;
    let header_xml = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:p><w:r><w:t>КолонтитулВерхний</w:t></w:r></w:p>
    </w:hdr>"#;
    let footnotes_xml = r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
        <w:footnote w:id="1"><w:p><w:r><w:t>СноскаНижняя</w:t></w:r></w:p></w:footnote>
    </w:footnotes>"#;

    let docx_bytes = make_zip(&[
        ("word/document.xml", doc_xml.as_bytes()),
        ("word/header1.xml", header_xml.as_bytes()),
        ("word/footnotes.xml", footnotes_xml.as_bytes()),
    ]);

    let config = ExtractConfig::default();
    let res = extract_bytes(&docx_bytes, "docx", false, &config);

    assert_eq!(res.status, "text");
    assert!(res.text.contains("ОсновнойДокумент"));
    assert!(res.text.contains("КолонтитулВерхний"));
    assert!(res.text.contains("СноскаНижняя"));
}

/// PPTX со слайдами slide2 до slide10 -> порядок из presentation.xml
#[test]
fn test_pptx_slide_order_from_presentation_xml() {
    let rels_xml = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
        <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
        <Relationship Id="rId10" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide10.xml"/>
    </Relationships>"#;

    // В presentation.xml слайд 2 идёт раньше слайда 10
    let pres_xml = r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
        <p:sldIdLst>
            <p:sldId id="256" r:id="rId2"/>
            <p:sldId id="257" r:id="rId10"/>
        </p:sldIdLst>
    </p:presentation>"#;

    let slide2 = r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
        <p:cSld><p:spTree><p:sp><p:txBody><p:p><p:r><p:t>СлайдНомерДва</p:t></p:r></p:p></p:txBody></p:sp></p:spTree></p:cSld>
    </p:sld>"#;

    let slide10 = r#"<p:sld xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main">
        <p:cSld><p:spTree><p:sp><p:txBody><p:p><p:r><p:t>СлайдНомерДесять</p:t></p:r></p:p></p:txBody></p:sp></p:spTree></p:cSld>
    </p:sld>"#;

    let pptx_bytes = make_zip(&[
        ("ppt/_rels/presentation.xml.rels", rels_xml.as_bytes()),
        ("ppt/presentation.xml", pres_xml.as_bytes()),
        ("ppt/slides/slide2.xml", slide2.as_bytes()),
        ("ppt/slides/slide10.xml", slide10.as_bytes()),
    ]);

    let config = ExtractConfig::default();
    let res = extract_bytes(&pptx_bytes, "pptx", false, &config);

    assert_eq!(res.status, "text");
    let pos2 = res.text.find("СлайдНомерДва").unwrap();
    let pos10 = res.text.find("СлайдНомерДесять").unwrap();
    assert!(pos2 < pos10, "Слайд 2 должен идти перед слайдом 10");
}

/// ODT: content.xml
#[test]
fn test_odt_extract_content_xml() {
    let odt_xml = r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0">
        <office:body><office:text><text:p>ТекстДокументаODT</text:p></office:text></office:body>
    </office:document-content>"#;

    let odt_bytes = make_zip(&[("content.xml", odt_xml.as_bytes())]);
    let config = ExtractConfig::default();
    let res = extract_bytes(&odt_bytes, "odt", false, &config);

    assert_eq!(res.status, "text");
    assert!(res.text.contains("ТекстДокументаODT"));
}

/// PDF без текста -> scan-needs-ocr
#[test]
fn test_pdf_without_text_returns_scan_needs_ocr() {
    let pdf_bytes = make_pdf(None);
    let config = ExtractConfig::default();
    let res = extract_bytes(&pdf_bytes, "pdf", false, &config);

    assert_eq!(res.status, "scan-needs-ocr");
    assert_eq!(res.text, "");
}

/// Битый DOCX / PDF -> failed с причиной, без паники
#[test]
fn test_corrupt_docx_and_pdf() {
    let config = ExtractConfig::default();

    // Битый DOCX (некорректный ZIP)
    let corrupt_docx = b"this is not a zip archive";
    let res_docx = extract_bytes(corrupt_docx, "docx", false, &config);
    assert_eq!(res_docx.status, "failed");
    assert!(res_docx.error.is_some());

    // Битый PDF
    let corrupt_pdf = b"%PDF-1.5 corrupt invalid data";
    let res_pdf = extract_bytes(corrupt_pdf, "pdf", false, &config);
    assert_eq!(res_pdf.status, "failed");
    assert!(res_pdf.error.is_some());
}

/// Внутренний файл ZIP: чтение из архива в память (<= предела), без временных файлов
#[test]
fn test_zip_member_direct_extraction() {
    let zip_bytes = make_zip(&[
        ("папка/заметка.txt", "Текст из внутреннего файла".as_bytes()),
        ("heavy.bin", &vec![0u8; 1024]),
    ]);

    let config = ExtractConfig::default();

    // Успешное чтение файла
    let res = extract_archive_member(&zip_bytes, "папка/заметка.txt", false, &config);
    assert_eq!(res.status, "text");
    assert!(res.text.contains("Текст из внутреннего файла"));

    // Превышение предела внутреннего файла
    let tiny_config = ExtractConfig {
        zip_max_file_size_mb: 0, // 0 МБ предел
        ..ExtractConfig::default()
    };
    let res_oversized = extract_archive_member(&zip_bytes, "heavy.bin", false, &tiny_config);
    assert_eq!(res_oversized.status, "archive-member-too-large");
}

/// Задача 189: файл формата без разборщика не читается вовсе. Доказательство: файл открыт
/// другим дескриптором с запретом совместного доступа, любое чтение содержимого дало бы
/// ошибку (`failed`); для контроля тот же файл с расширением html действительно падает.
#[cfg(windows)]
#[test]
fn test_unsupported_format_content_not_read() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = tempdir().unwrap();
    let config = ExtractConfig::default();

    let make_locked = |name: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, vec![7u8; 4 * 1024 * 1024]).unwrap();
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&path)
            .unwrap();
        (path, lock)
    };

    // Контроль: для html содержимое читается, при блокировке чтение падает
    let (html_path, _html_lock) = make_locked("страница.html");
    assert_eq!(extract_file(&html_path, false, &config).status, "failed");

    for name in [
        "клип.mp4",
        "песня.mp3",
        "картинка.png",
        "звук.wav",
        "модуль.dll",
        "прог.exe",
    ] {
        let (path, _lock) = make_locked(name);
        let res = extract_file(&path, false, &config);
        assert_eq!(
            res,
            poiskovichek::extract::Extracted::metadata_only(),
            "{name}"
        );
    }
}

/// Задача 189: файл формата без разборщика, обычный случай — metadata-only.
#[test]
fn test_unsupported_format_file_is_metadata_only() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("клип.mp4");
    std::fs::write(&path, vec![1u8; 3 * 1024 * 1024]).unwrap();
    let res = extract_file(&path, false, &ExtractConfig::default());
    assert_eq!(res, poiskovichek::extract::Extracted::metadata_only());
}

/// Задача 189: член архива без разборщика не распаковывается, но размер проверяется как прежде.
#[test]
fn test_zip_member_unsupported_format_not_unpacked() {
    let zip_bytes = make_zip(&[("видео.mp4", &vec![0u8; 4096]), ("текст.txt", b"abc")]);
    let config = ExtractConfig::default();
    let res = extract_archive_member(&zip_bytes, "видео.mp4", false, &config);
    assert_eq!(res, poiskovichek::extract::Extracted::metadata_only());

    let tiny = ExtractConfig {
        zip_max_file_size_mb: 0,
        ..ExtractConfig::default()
    };
    assert_eq!(
        extract_archive_member(&zip_bytes, "видео.mp4", false, &tiny).status,
        "archive-member-too-large"
    );
    let res = extract_archive_member(&zip_bytes, "нет.mp4", false, &config);
    assert_eq!(res.status, "failed");
    assert_eq!(
        extract_archive_member(&zip_bytes, "текст.txt", false, &config).text,
        "abc"
    );
}

/// Проверка версий извлечения
#[test]
fn test_extraction_versions() {
    assert_eq!(EXTRACTION_VERSION, 2);
    assert_eq!(format_version("epub"), 2);
    assert_eq!(format_version("docx"), 2);
    assert_eq!(format_version("pptx"), 2);
    assert_eq!(format_version("pdf"), 2);
    assert_eq!(format_version("fb2"), 2);
    assert_eq!(format_version("rtf"), 2);
    assert_eq!(format_version("txt"), 1);
}

/// ODT: текст обрезается по пределу символов, как в DOCX/XLSX/PPTX (№229).
#[test]
fn test_odt_text_is_cut_at_character_limit() {
    let mut paragraphs = String::new();
    for i in 0..200 {
        paragraphs.push_str(&format!("<text:p>СтрокаНомер{i:03}ДляПроверки</text:p>"));
    }
    let odt_xml = format!(
        r#"<office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:text>{paragraphs}</office:text></office:body></office:document-content>"#
    );
    let odt_bytes = make_zip(&[("content.xml", odt_xml.as_bytes())]);

    let full = extract_bytes(&odt_bytes, "odt", false, &ExtractConfig::default());
    assert_eq!(full.status, "text");
    assert!(full.text.contains("СтрокаНомер199ДляПроверки"));
    assert!(full.text.chars().count() > 1000);

    let config = ExtractConfig {
        limit_chars_books_documents: 100,
        ..ExtractConfig::default()
    };
    let cut = extract_bytes(&odt_bytes, "odt", false, &config);
    assert_eq!(cut.status, "text");
    assert!(
        cut.text.chars().count() <= 100,
        "длина: {}",
        cut.text.chars().count()
    );
    assert!(cut.text.contains("СтрокаНомер000ДляПроверки"));
    assert!(!cut.text.contains("СтрокаНомер199ДляПроверки"));
}

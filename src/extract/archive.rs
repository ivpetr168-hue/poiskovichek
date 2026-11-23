//! Извлечение текста из архивных контейнеров:
//! EPUB (по порядку spine из OPF, E-1), DOCX (document.xml + колонтитулы + сноски + примечания + таблицы, E-2),
//! XLSX (sharedStrings + порядок листов), PPTX (порядок слайдов из presentation.xml),
//! ODT/ODS/ODP (content.xml), а также чтение отдельных файлов из ZIP-архивов в память.

use std::collections::HashMap;
use std::io::{Cursor, Read};
use zip::ZipArchive;

use super::fb2::node_text;
use super::html::extract_html_text;
use super::text::decode_text;

/// Разрешает относительный путь внутри архива относительно базового файла.
pub fn resolve_relative_path(base_file: &str, target: &str) -> String {
    let clean_target = target.split('#').next().unwrap_or(target);
    if clean_target.starts_with('/') {
        return clean_target.trim_start_matches('/').to_string();
    }

    let mut parts: Vec<&str> = base_file
        .rsplit_once('/')
        .map(|(dir, _)| dir.split('/').collect())
        .unwrap_or_default();

    for segment in clean_target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(segment),
        }
    }
    parts.join("/")
}

/// Читает байты файла из zip-архива по имени (с нормализацией слэшей).
fn read_zip_entry<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    name: &str,
    limit_bytes: u64,
) -> Result<Vec<u8>, String> {
    let normalized = name.replace('\\', "/");
    let clean = normalized.trim_start_matches('/');

    let mut found_index = None;
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            let file_name = file.name().replace('\\', "/");
            let file_clean = file_name.trim_start_matches('/');
            if file_clean == clean {
                found_index = Some(i);
                break;
            }
        }
    }

    let index = found_index.ok_or_else(|| format!("Файл '{name}' отсутствует в архиве"))?;
    let mut file = archive
        .by_index(index)
        .map_err(|e| format!("Ошибка открытия файла '{name}': {e}"))?;

    let size = file.size();
    if size > limit_bytes {
        return Err(format!(
            "Файл '{name}' превышает предел размера ({size} байт)"
        ));
    }

    let mut buffer = Vec::with_capacity(size.min(1024 * 1024) as usize);
    file.read_to_end(&mut buffer)
        .map_err(|e| format!("Ошибка чтения файла '{name}': {e}"))?;

    Ok(buffer)
}

/// Извлекает текст из пакетов EPUB, DOCX, XLSX, PPTX, ODT, ODS, ODP.
pub fn extract_package_text(
    bytes: &[u8],
    extension: &str,
    limit_chars: usize,
) -> Result<String, String> {
    let cursor = Cursor::new(bytes);
    let mut archive =
        ZipArchive::new(cursor).map_err(|e| format!("Некорректный ZIP-архив: {e}"))?;
    let ext = extension.trim_start_matches('.').to_ascii_lowercase();

    match ext.as_str() {
        "epub" => extract_epub(&mut archive, limit_chars),
        "docx" => extract_docx(&mut archive, limit_chars),
        "xlsx" => extract_xlsx(&mut archive, limit_chars),
        "pptx" => extract_pptx(&mut archive, limit_chars),
        "odt" | "ods" | "odp" => extract_odf(&mut archive, limit_chars),
        _ => Err(format!("Неподдерживаемый формат пакета: {ext}")),
    }
}

/// Извлекает главы EPUB строго в порядке spine из OPF (странность Е-1 исправлена).
fn extract_epub<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    limit_chars: usize,
) -> Result<String, String> {
    let container_bytes = read_zip_entry(archive, "META-INF/container.xml", 10 * 1024 * 1024)?;
    let container_xml = decode_text(&container_bytes);
    let container_doc = roxmltree::Document::parse(&container_xml)
        .map_err(|e| format!("Ошибка парсинга container.xml: {e}"))?;

    let opf_path = container_doc
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "rootfile")
        .and_then(|n| n.attribute("full-path"))
        .ok_or_else(|| "В container.xml не найден путь к OPF rootfile".to_string())?
        .to_string();

    let opf_bytes = read_zip_entry(archive, &opf_path, 20 * 1024 * 1024)?;
    let opf_xml = decode_text(&opf_bytes);
    let opf_doc = roxmltree::Document::parse(&opf_xml)
        .map_err(|e| format!("Ошибка парсинга OPF ({opf_path}): {e}"))?;

    // Манифест: id -> href
    let mut manifest: HashMap<String, String> = HashMap::new();
    for node in opf_doc.descendants() {
        if node.is_element()
            && node.tag_name().name() == "item"
            && let (Some(id), Some(href)) = (node.attribute("id"), node.attribute("href"))
        {
            manifest.insert(id.to_string(), href.to_string());
        }
    }

    // Spine: главы в порядке itemref
    let mut chapter_paths = Vec::new();
    for node in opf_doc.descendants() {
        if node.is_element()
            && node.tag_name().name() == "itemref"
            && let Some(idref) = node.attribute("idref")
            && let Some(href) = manifest.get(idref)
        {
            let full_chapter_path = resolve_relative_path(&opf_path, href);
            chapter_paths.push(full_chapter_path);
        }
    }

    if chapter_paths.is_empty() {
        return Err("В spine EPUB не найдено ни одной главы".to_string());
    }

    let mut parts = Vec::new();
    let mut current_chars = 0;

    for chapter_path in chapter_paths {
        if let Ok(ch_bytes) = read_zip_entry(archive, &chapter_path, 20 * 1024 * 1024) {
            let html = decode_text(&ch_bytes);
            let text = extract_html_text(&html);
            if !text.is_empty() {
                current_chars += text.chars().count();
                parts.push(text);
                if current_chars >= limit_chars {
                    break;
                }
            }
        }
    }

    Ok(parts.join("\n"))
}

/// Извлекает текст DOCX: document.xml + колонтитулы + сноски + примечания + таблицы (Е-2).
fn extract_docx<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    limit_chars: usize,
) -> Result<String, String> {
    let mut xml_names = vec!["word/document.xml".to_string()];

    // Находим дополнительные части: колонтитулы, сноски, концевые сноски, примечания
    let mut extra_names: Vec<String> = Vec::new();
    for i in 0..archive.len() {
        if let Ok(file) = archive.by_index(i) {
            let name = file.name().replace('\\', "/");
            let clean = name.trim_start_matches('/');
            if clean.starts_with("word/")
                && (clean.starts_with("word/header")
                    || clean.starts_with("word/footer")
                    || clean == "word/footnotes.xml"
                    || clean == "word/endnotes.xml"
                    || clean == "word/comments.xml")
            {
                extra_names.push(clean.to_string());
            }
        }
    }
    extra_names.sort();
    xml_names.extend(extra_names);

    let mut parts = Vec::new();
    let mut current_chars = 0;

    for name in xml_names {
        if let Ok(bytes) = read_zip_entry(archive, &name, 20 * 1024 * 1024) {
            let xml = decode_text(&bytes);
            if let Ok(doc) = roxmltree::Document::parse(&xml) {
                let text = node_text(doc.root());
                let normalized: Vec<String> = text
                    .lines()
                    .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|l| !l.is_empty())
                    .collect();
                if !normalized.is_empty() {
                    let chunk = normalized.join("\n");
                    current_chars += chunk.chars().count();
                    parts.push(chunk);
                    if current_chars >= limit_chars {
                        break;
                    }
                }
            }
        }
    }

    if parts.is_empty() {
        return Err("В DOCX не обнаружен текст".to_string());
    }

    Ok(parts.join("\n"))
}

/// Извлекает текст ODF (ODT, ODS, ODP): content.xml.
fn extract_odf<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    limit_chars: usize,
) -> Result<String, String> {
    let bytes = read_zip_entry(archive, "content.xml", 20 * 1024 * 1024)?;
    let xml = decode_text(&bytes);
    let doc = roxmltree::Document::parse(&xml)
        .map_err(|e| format!("Ошибка парсинга content.xml: {e}"))?;
    let text = node_text(doc.root());

    // Строки собираем, пока не набрали предел символов (как в DOCX/XLSX/PPTX);
    // точную обрезку до предела выполняет вызывающий код.
    let mut normalized: Vec<String> = Vec::new();
    let mut current_chars = 0;
    for line in text.lines() {
        let l = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if l.is_empty() {
            continue;
        }
        current_chars += l.chars().count();
        normalized.push(l);
        if current_chars >= limit_chars {
            break;
        }
    }
    Ok(normalized.join("\n"))
}

/// Извлекает текст XLSX: xl/sharedStrings.xml + значения ячеек листов в порядке книги.
fn extract_xlsx<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    limit_chars: usize,
) -> Result<String, String> {
    // 1. sharedStrings.xml (может отсутствовать, если все строки inline)
    let mut shared_strings: Vec<String> = Vec::new();
    if let Ok(ss_bytes) = read_zip_entry(archive, "xl/sharedStrings.xml", 50 * 1024 * 1024) {
        let ss_xml = decode_text(&ss_bytes);
        if let Ok(doc) = roxmltree::Document::parse(&ss_xml) {
            for node in doc.descendants() {
                if node.is_element() && node.tag_name().name() == "si" {
                    let s = node_text(node);
                    shared_strings.push(s.trim().to_string());
                }
            }
        }
    }

    // 2. Связи книги xl/_rels/workbook.xml.rels
    let rels_bytes = read_zip_entry(archive, "xl/_rels/workbook.xml.rels", 10 * 1024 * 1024)?;
    let rels_xml = decode_text(&rels_bytes);
    let rels_doc = roxmltree::Document::parse(&rels_xml)
        .map_err(|e| format!("Ошибка парсинга workbook.xml.rels: {e}"))?;

    let mut rels: HashMap<String, String> = HashMap::new();
    for node in rels_doc.descendants() {
        if node.is_element()
            && node.tag_name().name() == "Relationship"
            && let (Some(id), Some(target)) = (node.attribute("Id"), node.attribute("Target"))
        {
            rels.insert(id.to_string(), target.to_string());
        }
    }

    // 3. Листы из xl/workbook.xml в порядке их следования
    let wb_bytes = read_zip_entry(archive, "xl/workbook.xml", 10 * 1024 * 1024)?;
    let wb_xml = decode_text(&wb_bytes);
    let wb_doc = roxmltree::Document::parse(&wb_xml)
        .map_err(|e| format!("Ошибка парсинга workbook.xml: {e}"))?;

    let mut parts = Vec::new();
    let mut current_chars = 0;

    for sheet_node in wb_doc.descendants() {
        if sheet_node.is_element() && sheet_node.tag_name().name() == "sheet" {
            let sheet_name = sheet_node.attribute("name").unwrap_or("");
            if !sheet_name.is_empty() {
                parts.push(sheet_name.to_string());
            }

            // Атрибут r:id (может быть в namespace отношений)
            let rel_id = sheet_node
                .attributes()
                .find(|a| a.name() == "id")
                .map(|a| a.value());

            if let Some(r_id) = rel_id
                && let Some(target) = rels.get(r_id)
            {
                let sheet_path = resolve_relative_path("xl/workbook.xml", target);
                if let Ok(sheet_bytes) = read_zip_entry(archive, &sheet_path, 50 * 1024 * 1024) {
                    let sheet_xml = decode_text(&sheet_bytes);
                    if let Ok(s_doc) = roxmltree::Document::parse(&sheet_xml) {
                        let mut cell_values = Vec::new();
                        for cell in s_doc.descendants() {
                            if cell.is_element() && cell.tag_name().name() == "c" {
                                let cell_type = cell.attribute("t").unwrap_or("");
                                let val_text = cell
                                    .children()
                                    .find(|n| n.is_element() && n.tag_name().name() == "v")
                                    .and_then(|n| n.text())
                                    .unwrap_or("");

                                if cell_type == "s" {
                                    if let Ok(idx) = val_text.parse::<usize>()
                                        && let Some(str_val) = shared_strings.get(idx)
                                    {
                                        cell_values.push(str_val.clone());
                                    }
                                } else if cell_type == "inlineStr" {
                                    let text = node_text(cell);
                                    cell_values.push(text.trim().to_string());
                                } else if !val_text.is_empty() {
                                    cell_values.push(val_text.to_string());
                                }
                            }
                        }
                        if !cell_values.is_empty() {
                            let chunk = cell_values.join(" ");
                            current_chars += chunk.chars().count();
                            parts.push(chunk);
                            if current_chars >= limit_chars {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(parts.join("\n"))
}

/// Извлекает слайды PPTX строго по порядку из ppt/presentation.xml.
fn extract_pptx<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    limit_chars: usize,
) -> Result<String, String> {
    // 1. Связи презентации ppt/_rels/presentation.xml.rels
    let rels_bytes = read_zip_entry(archive, "ppt/_rels/presentation.xml.rels", 10 * 1024 * 1024)?;
    let rels_xml = decode_text(&rels_bytes);
    let rels_doc = roxmltree::Document::parse(&rels_xml)
        .map_err(|e| format!("Ошибка парсинга presentation.xml.rels: {e}"))?;

    let mut rels: HashMap<String, String> = HashMap::new();
    for node in rels_doc.descendants() {
        if node.is_element()
            && node.tag_name().name() == "Relationship"
            && let (Some(id), Some(target)) = (node.attribute("Id"), node.attribute("Target"))
        {
            rels.insert(id.to_string(), target.to_string());
        }
    }

    // 2. Слайды из ppt/presentation.xml в порядке тегов sldId
    let pres_bytes = read_zip_entry(archive, "ppt/presentation.xml", 10 * 1024 * 1024)?;
    let pres_xml = decode_text(&pres_bytes);
    let pres_doc = roxmltree::Document::parse(&pres_xml)
        .map_err(|e| format!("Ошибка парсинга presentation.xml: {e}"))?;

    let mut parts = Vec::new();
    let mut current_chars = 0;

    for sld_node in pres_doc.descendants() {
        if sld_node.is_element()
            && sld_node.tag_name().name() == "sldId"
            && let Some(rel_id) = sld_node
                .attributes()
                .find(|a| {
                    a.name() == "id" && (a.namespace().is_some() || a.value().starts_with("rId"))
                })
                .map(|a| a.value())
            && let Some(target) = rels.get(rel_id)
        {
            let slide_path = resolve_relative_path("ppt/presentation.xml", target);
            if let Ok(slide_bytes) = read_zip_entry(archive, &slide_path, 20 * 1024 * 1024) {
                let slide_xml = decode_text(&slide_bytes);
                if let Ok(s_doc) = roxmltree::Document::parse(&slide_xml) {
                    let text = node_text(s_doc.root());
                    let normalized: Vec<String> = text
                        .lines()
                        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
                        .filter(|l| !l.is_empty())
                        .collect();
                    if !normalized.is_empty() {
                        let chunk = normalized.join("\n");
                        current_chars += chunk.chars().count();
                        parts.push(chunk);
                        if current_chars >= limit_chars {
                            break;
                        }
                    }
                }
            }
        }
    }

    Ok(parts.join("\n"))
}

/// Читает содержимое отдельного внутреннего файла ZIP в память (без временных файлов).
/// Если размер превышает предел, возвращает Err("archive-member-too-large").
pub fn read_zip_member_bytes(
    archive_bytes: &[u8],
    member_name: &str,
    max_size_bytes: u64,
) -> Result<Vec<u8>, &'static str> {
    read_zip_member(archive_bytes, member_name, max_size_bytes, true)
}

/// То же, но при `read_content == false` проверяет наличие и размер члена,
/// не распаковывая его (возвращает пустой буфер).
pub fn read_zip_member(
    archive_bytes: &[u8],
    member_name: &str,
    max_size_bytes: u64,
    read_content: bool,
) -> Result<Vec<u8>, &'static str> {
    let cursor = Cursor::new(archive_bytes);
    let mut archive = ZipArchive::new(cursor).map_err(|_| "Некорректный ZIP-архив")?;

    // То же правило имени, что при обходе и чтении из архива на диске
    // (имя с '/' приводится к виду из базы)
    let wanted = member_name.replace('/', "\\");
    let index = crate::walk::find_zip_member_index(&mut archive, &wanted)
        .ok_or("Файл не найден внутри архива")?;
    let mut file = archive
        .by_index(index)
        .map_err(|_| "Не удалось открыть член архива")?;

    if file.size() > max_size_bytes {
        return Err("archive-member-too-large");
    }

    if !read_content {
        return Ok(Vec::new());
    }

    let mut buf = Vec::with_capacity(file.size() as usize);
    file.read_to_end(&mut buf)
        .map_err(|_| "Ошибка чтения члена архива")?;

    Ok(buf)
}

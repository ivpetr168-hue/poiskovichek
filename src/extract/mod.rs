//! Модуль безопасного извлечения текста по форматам файлов (раздел 2.7 ТЗ).

pub mod archive;
pub mod fb2;
pub mod html;
pub mod pdf;
pub mod rtf;
pub mod text;
pub mod unsupported;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings::{PdfExtractorMode, Settings};
pub use archive::read_zip_member_bytes;

/// Текущая версия правил извлечения текста (для переиндексации устаревших форматов).
pub const EXTRACTION_VERSION: u32 = 2;

/// Возвращает версию извлекателя для конкретного формата (R1-А-80, 132).
pub fn format_version(extension: &str) -> u32 {
    let clean = extension.trim_start_matches('.').to_ascii_lowercase();
    match clean.as_str() {
        "epub" => 2, // чтение строго по OPF spine (странность E-1)
        "docx" => 2, // document.xml + колонтитулы + сноски + примечания + таблицы (E-2)
        "pptx" => 2, // порядок слайдов из presentation.xml
        "pdf" => 2,  // встроенный извлекатель pdf-extract
        "fb2" => 2,  // запасной путь для повреждённого XML
        "rtf" => 2,  // поддержка суррогатных пар юникода и CP1251
        "html" | "htm" => 2,
        _ => 1,
    }
}

/// Результат извлечения текста из файла.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extracted {
    /// Извлечённый текст документа (обрезанный по лимиту символов).
    pub text: String,
    /// Статус извлечения: text, metadata-only, scan-needs-ocr, unsupported-text-<ext>,
    /// sensitive-metadata-only, failed, archive-member-too-large.
    pub status: String,
    /// Описание ошибки при сбое извлечения.
    pub error: Option<String>,
}

impl Extracted {
    pub fn text(text: String) -> Self {
        Self {
            text,
            status: "text".into(),
            error: None,
        }
    }

    pub fn metadata_only() -> Self {
        Self {
            text: String::new(),
            status: "metadata-only".into(),
            error: None,
        }
    }

    pub fn scan_needs_ocr() -> Self {
        Self {
            text: String::new(),
            status: "scan-needs-ocr".into(),
            error: None,
        }
    }

    pub fn unsupported(extension: &str) -> Self {
        let clean = extension.trim_start_matches('.').to_ascii_lowercase();
        Self {
            text: String::new(),
            status: format!("unsupported-text-{clean}"),
            error: None,
        }
    }

    pub fn sensitive() -> Self {
        Self {
            text: String::new(),
            status: "sensitive-metadata-only".into(),
            error: None,
        }
    }

    pub fn archive_member_too_large() -> Self {
        Self {
            text: String::new(),
            status: "archive-member-too-large".into(),
            error: None,
        }
    }

    pub fn cloud_not_downloaded() -> Self {
        Self {
            text: String::new(),
            status: "cloud-not-downloaded".into(),
            error: None,
        }
    }

    pub fn failed(err: impl Into<String>) -> Self {
        Self {
            text: String::new(),
            status: "failed".into(),
            error: Some(err.into()),
        }
    }
}

/// Настройки извлечения текста (раздел 5 ТЗ).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractConfig {
    pub extract_text_formats: bool,
    pub extract_office_formats: bool,
    pub extract_book_formats: bool,
    pub extract_pdf_format: bool,
    pub limit_chars_books_documents: usize,
    pub limit_chars_code_service: usize,
    pub limit_chars_other: usize,
    pub limit_chars_large_txt: usize,
    pub large_txt_threshold_mb: u64,
    pub zip_max_file_size_mb: u64,
    pub pdf_extractor: PdfExtractorMode,
    pub pdf_external_path: PathBuf,
}

impl Default for ExtractConfig {
    fn default() -> Self {
        Self {
            extract_text_formats: true,
            extract_office_formats: true,
            extract_book_formats: true,
            extract_pdf_format: true,
            limit_chars_books_documents: 20_000_000,
            limit_chars_code_service: 200_000,
            limit_chars_other: 100_000,
            limit_chars_large_txt: 200_000,
            large_txt_threshold_mb: 5,
            zip_max_file_size_mb: 20,
            pdf_extractor: PdfExtractorMode::Builtin,
            pdf_external_path: PathBuf::new(),
        }
    }
}

impl From<&Settings> for ExtractConfig {
    fn from(s: &Settings) -> Self {
        Self {
            extract_text_formats: s.extract_text_formats,
            extract_office_formats: s.extract_office_formats,
            extract_book_formats: s.extract_book_formats,
            extract_pdf_format: s.extract_pdf_format,
            limit_chars_books_documents: s.limit_chars_books_documents,
            limit_chars_code_service: s.limit_chars_code_service,
            limit_chars_other: s.limit_chars_other,
            limit_chars_large_txt: s.limit_chars_large_txt,
            large_txt_threshold_mb: s.large_txt_threshold_mb,
            zip_max_file_size_mb: s.zip_max_file_size_mb,
            pdf_extractor: s.pdf_extractor,
            pdf_external_path: s.pdf_external_path.clone(),
        }
    }
}

impl ExtractConfig {
    /// Рассчитывает предел символов для файла заданного типа и размера (R1-А-116, 117).
    pub fn char_limit(&self, extension: &str, file_size: u64) -> usize {
        let clean = extension.trim_start_matches('.').to_ascii_lowercase();
        if (clean == "txt" || clean == "md")
            && file_size >= self.large_txt_threshold_mb * 1024 * 1024
        {
            return self.limit_chars_large_txt;
        }

        if is_book_or_document(&clean) {
            self.limit_chars_books_documents
        } else if is_code_or_service(&clean) {
            self.limit_chars_code_service
        } else {
            self.limit_chars_other
        }
    }
}

/// Проверяет, относится ли формат к книгам или офисным документам (лимит 20 млн).
pub fn is_book_or_document(ext: &str) -> bool {
    matches!(
        ext,
        "fb2"
            | "epub"
            | "mobi"
            | "azw"
            | "azw3"
            | "pdf"
            | "doc"
            | "docx"
            | "odt"
            | "rtf"
            | "txt"
            | "md"
            | "html"
            | "htm"
            | "xlsx"
            | "ods"
            | "pptx"
            | "odp"
    )
}

/// Проверяет, относится ли формат к исходному коду или служебным данным (лимит 200 тыс.).
pub fn is_code_or_service(ext: &str) -> bool {
    matches!(
        ext,
        "sql"
            | "jsonl"
            | "json"
            | "xml"
            | "yaml"
            | "yml"
            | "ini"
            | "cfg"
            | "log"
            | "csv"
            | "tsv"
            | "py"
            | "ps1"
            | "js"
            | "ts"
            | "tsx"
            | "jsx"
            | "java"
            | "c"
            | "cpp"
            | "h"
            | "cs"
            | "ipynb"
            | "sh"
            | "bat"
            | "cmd"
            | "rs"
            | "toml"
    )
}

/// Проверяет, относится ли формат к группе офисных документов.
pub fn is_office_extension(ext: &str) -> bool {
    matches!(
        ext,
        "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp" | "doc" | "xls" | "ppt"
    )
}

/// Проверяет, относится ли формат к группе книг и форматированного текста.
pub fn is_book_extension(ext: &str) -> bool {
    matches!(
        ext,
        "html" | "htm" | "rtf" | "fb2" | "epub" | "mobi" | "azw" | "azw3"
    )
}

/// Источник данных для извлечения (путь к файлу или байты в памяти).
#[derive(Clone, Copy)]
pub enum ExtractSource<'a> {
    Path(&'a Path),
    Bytes(&'a [u8]),
}

/// Извлекает текст из файла на диске.
pub fn extract_file(path: &Path, sensitive: bool, config: &ExtractConfig) -> Extracted {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return Extracted::failed(format!("Файл недоступен: {e}")),
    };

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if crate::walk::is_cloud_file_attributes(metadata.file_attributes()) {
            return Extracted::cloud_not_downloaded();
        }
    }

    let size = metadata.len();
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    extract(ExtractSource::Path(path), ext, size, sensitive, config)
}

/// Извлекает текст из байтов в памяти.
pub fn extract_bytes(
    bytes: &[u8],
    extension: &str,
    sensitive: bool,
    config: &ExtractConfig,
) -> Extracted {
    extract(
        ExtractSource::Bytes(bytes),
        extension,
        bytes.len() as u64,
        sensitive,
        config,
    )
}

/// Главная функция извлечения текста по формату.
pub fn extract(
    source: ExtractSource<'_>,
    extension: &str,
    size: u64,
    sensitive: bool,
    config: &ExtractConfig,
) -> Extracted {
    // Закрытые данные: содержимое не читается вовсе (R1-Б-50)
    if sensitive {
        return Extracted::sensitive();
    }

    let clean_ext = extension.trim_start_matches('.').to_ascii_lowercase();

    // Заведомо неподдерживаемые бинарные форматы прежней программы (R1-Б-43)
    if unsupported::is_unsupported_text_extension(&clean_ext) {
        return Extracted::unsupported(&clean_ext);
    }

    // Проверка настроек включения/выключения групп форматов
    if clean_ext == "pdf" && !config.extract_pdf_format {
        return Extracted::metadata_only();
    }
    if is_office_extension(&clean_ext) && !config.extract_office_formats {
        return Extracted::metadata_only();
    }
    if is_book_extension(&clean_ext) && !config.extract_book_formats {
        return Extracted::metadata_only();
    }
    if text::is_text_extension(&clean_ext) && !config.extract_text_formats {
        return Extracted::metadata_only();
    }

    let limit = config.char_limit(&clean_ext, size);

    // 1. Обычные текстовые форматы и исходный код
    if text::is_text_extension(&clean_ext) {
        let data = match source {
            ExtractSource::Bytes(b) => {
                let bytes_to_read = (limit * 4).min(b.len());
                b[..bytes_to_read].to_vec()
            }
            ExtractSource::Path(p) => match File::open(p) {
                Ok(file) => {
                    let bytes_to_read = if size > 0 {
                        ((limit * 4) as u64).min(size)
                    } else {
                        (limit * 4) as u64
                    };
                    let mut buf = Vec::new();
                    if let Err(e) = file.take(bytes_to_read).read_to_end(&mut buf) {
                        return Extracted::failed(format!("Ошибка чтения файла: {e}"));
                    }
                    buf
                }
                Err(e) => return Extracted::failed(format!("Ошибка открытия файла: {e}")),
            },
        };

        let decoded = text::decode_text(&data);
        let result_text: String = if decoded.chars().count() > limit {
            decoded.chars().take(limit).collect()
        } else {
            decoded
        };

        return Extracted::text(result_text);
    }

    // 7. Все прочие форматы — только свойства: содержимое не читаем вовсе
    if !is_whole_file_format(&clean_ext) && clean_ext != "pdf" {
        return Extracted::metadata_only();
    }

    // Читаем полные данные для сложных форматов (HTML, RTF, FB2, архивы, PDF)
    let full_data = match source {
        ExtractSource::Bytes(b) => b.to_vec(),
        ExtractSource::Path(p) => match std::fs::read(p) {
            Ok(b) => b,
            Err(e) => return Extracted::failed(format!("Ошибка чтения файла: {e}")),
        },
    };

    // 2. HTML / HTM
    if clean_ext == "html" || clean_ext == "htm" {
        let html_str = html::decode_html(&full_data);
        let extracted_text = html::extract_html_text(&html_str);
        let result_text: String = if extracted_text.chars().count() > limit {
            extracted_text.chars().take(limit).collect()
        } else {
            extracted_text
        };
        return if result_text.trim().is_empty() {
            Extracted::metadata_only()
        } else {
            Extracted::text(result_text)
        };
    }

    // 3. RTF
    if clean_ext == "rtf" {
        let extracted_text = rtf::extract_rtf_text(&full_data, limit);
        let result_text: String = if extracted_text.chars().count() > limit {
            extracted_text.chars().take(limit).collect()
        } else {
            extracted_text
        };
        return if result_text.trim().is_empty() {
            Extracted::metadata_only()
        } else {
            Extracted::text(result_text)
        };
    }

    // 4. FB2
    if clean_ext == "fb2" {
        let extracted_text = fb2::extract_fb2_text(&full_data, limit);
        let result_text: String = if extracted_text.chars().count() > limit {
            extracted_text.chars().take(limit).collect()
        } else {
            extracted_text
        };
        return if result_text.trim().is_empty() {
            Extracted::metadata_only()
        } else {
            Extracted::text(result_text)
        };
    }

    // 5. Офисные форматы и книги в ZIP (EPUB, DOCX, XLSX, PPTX, ODT, ODS, ODP)
    if matches!(
        clean_ext.as_str(),
        "epub" | "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp"
    ) {
        match archive::extract_package_text(&full_data, &clean_ext, limit) {
            Ok(extracted_text) => {
                let result_text: String = if extracted_text.chars().count() > limit {
                    extracted_text.chars().take(limit).collect()
                } else {
                    extracted_text
                };
                return if result_text.trim().is_empty() {
                    Extracted::metadata_only()
                } else {
                    Extracted::text(result_text)
                };
            }
            Err(err) => return Extracted::failed(err),
        }
    }

    // 6. PDF
    if clean_ext == "pdf" {
        let path_opt = match source {
            ExtractSource::Path(p) => Some(p),
            ExtractSource::Bytes(_) => None,
        };
        let bytes_opt = match source {
            ExtractSource::Bytes(b) => Some(b),
            ExtractSource::Path(_) => Some(full_data.as_slice()),
        };
        return pdf::extract_pdf(
            path_opt,
            bytes_opt,
            config.pdf_extractor,
            &config.pdf_external_path,
            limit,
        );
    }

    // Недостижимо: прочие форматы отсеяны до чтения содержимого
    Extracted::metadata_only()
}

/// Форматы со сложным разбором: `extract` читает файл целиком (кроме pdf, у него свои правила).
/// Единый список для `extract` и `bytes_needed`.
fn is_whole_file_format(clean_ext: &str) -> bool {
    matches!(
        clean_ext,
        "html" | "htm" | "rtf" | "fb2" | "epub" | "docx" | "xlsx" | "pptx" | "odt" | "ods" | "odp"
    )
}

/// Нужно ли `extract` содержимое файла для этого расширения (в нижнем регистре, без точки).
fn content_may_be_needed(clean_ext: &str) -> bool {
    text::is_text_extension(clean_ext) || is_whole_file_format(clean_ext) || clean_ext == "pdf"
}

/// Сколько байт начала файла читает `extract` из источника `ExtractSource::Path`.
///
/// Нужно планировщику чтения по дискам: он заранее читает ровно столько, сколько
/// возьмёт извлекатель. 0 — содержимое не нужно вовсе (закрытые данные, неподдерживаемые
/// и выключенные форматы, «только свойства», пустой файл, внешний pdftotext сам читает
/// файл по пути). Порядок проверок повторяет `extract`.
pub fn bytes_needed(extension: &str, size: u64, sensitive: bool, config: &ExtractConfig) -> u64 {
    if sensitive || size == 0 {
        return 0;
    }
    let clean_ext = extension.trim_start_matches('.').to_ascii_lowercase();
    if unsupported::is_unsupported_text_extension(&clean_ext) {
        return 0;
    }
    if clean_ext == "pdf" && !config.extract_pdf_format {
        return 0;
    }
    if is_office_extension(&clean_ext) && !config.extract_office_formats {
        return 0;
    }
    if is_book_extension(&clean_ext) && !config.extract_book_formats {
        return 0;
    }
    if text::is_text_extension(&clean_ext) {
        if !config.extract_text_formats {
            return 0;
        }
        let limit = config.char_limit(&clean_ext, size);
        return ((limit as u64).saturating_mul(4)).min(size);
    }
    if is_whole_file_format(&clean_ext) {
        return size;
    }
    if clean_ext == "pdf" && config.pdf_extractor == PdfExtractorMode::Builtin {
        return size;
    }
    0
}

/// Извлекает текст из отдельного файла внутри ZIP-архива в память (без временных файлов).
pub fn extract_archive_member(
    archive_bytes: &[u8],
    member_name: &str,
    sensitive: bool,
    config: &ExtractConfig,
) -> Extracted {
    let max_member_bytes = config.zip_max_file_size_mb * 1024 * 1024;
    let ext = member_name.rsplit('.').next().unwrap_or("");
    let clean_ext = ext.trim_start_matches('.').to_ascii_lowercase();
    // Содержимое члена распаковываем, только если оно нужно извлекателю; размер и
    // наличие члена проверяются в любом случае (статусы прежние).
    let read_content = !sensitive && content_may_be_needed(&clean_ext);
    let member_bytes = match archive::read_zip_member(
        archive_bytes,
        member_name,
        max_member_bytes,
        read_content,
    ) {
        Ok(b) => b,
        Err("archive-member-too-large") => return Extracted::archive_member_too_large(),
        Err(err) => return Extracted::failed(err),
    };

    extract_bytes(&member_bytes, ext, sensitive, config)
}

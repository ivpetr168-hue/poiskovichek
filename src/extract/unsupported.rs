//! Обработка неподдерживаемых текстовых форматов:
//! doc, xls, ppt, mobi, azw, azw3 -> unsupported-text-<ext> (R1-Б-43).

pub const UNSUPPORTED_TEXT_EXTENSIONS: &[&str] = &["doc", "xls", "ppt", "mobi", "azw", "azw3"];

/// Проверяет, является ли формат заведомо неподдерживаемым текстовым форматом прежней программы.
pub fn is_unsupported_text_extension(ext: &str) -> bool {
    let clean = ext.trim_start_matches('.').to_ascii_lowercase();
    UNSUPPORTED_TEXT_EXTENSIONS.contains(&clean.as_str())
}

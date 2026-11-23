pub mod dialogs;
pub mod results;
pub mod settings_dialog;
pub mod theme;
pub mod wizard;

use crate::settings::{SortMode, SourceMode};

/// Режимы сортировки в порядке показа в списках выбора и их названия.
pub const SORT_MODES: [(SortMode, &str); 4] = [
    (SortMode::Relevance, "по соответствию"),
    (SortMode::SizeDesc, "сначала крупные"),
    (SortMode::DateDesc, "сначала новые"),
    (SortMode::NameAsc, "по имени"),
];

/// Название режима сортировки для окна.
pub fn sort_mode_title(mode: SortMode) -> &'static str {
    SORT_MODES
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, title)| *title)
        .unwrap_or("по соответствию")
}

/// Название режима индексирования источника для окна.
pub fn source_mode_title(mode: SourceMode) -> &'static str {
    match mode {
        SourceMode::Full => "полностью",
        SourceMode::DocumentsOnly => "только документы",
        SourceMode::NamesOnly => "только имена",
    }
}

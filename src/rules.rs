use std::path::Path;

/// Набор расширений документов (R1-В-1, строки 136–140).
pub const DOCUMENT_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".rst", ".csv", ".tsv", ".json", ".jsonl", ".xml", ".yaml", ".yml", ".ini",
    ".cfg", ".log", ".pdf", ".doc", ".docx", ".odt", ".rtf", ".epub", ".xls", ".xlsx", ".ods",
    ".ppt", ".pptx", ".odp", ".html", ".htm",
];

/// Расширения электронных книг (R1-В-1, строка 141).
pub const EBOOK_EXTENSIONS: &[&str] = &[".epub", ".fb2", ".mobi", ".azw", ".azw3"];

/// Расширения файлов режима «только документы» (R1-В-1, строки 142–146).
pub const DOCUMENT_ONLY_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".rst", ".pdf", ".doc", ".docx", ".odt", ".rtf", ".epub", ".fb2", ".mobi",
    ".azw", ".azw3", ".xls", ".xlsx", ".ods", ".ppt", ".pptx", ".odp", ".html", ".htm", ".zip",
];

/// Расширения изображений (R1-В-1, строка 147).
pub const IMAGE_EXTENSIONS: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".tif", ".tiff", ".webp", ".heic", ".raw", ".cr2",
    ".cr3", ".nef", ".svg",
];

/// Расширения аудиофайлов (R1-В-1, строка 148).
pub const AUDIO_EXTENSIONS: &[&str] = &[
    ".mp3", ".wav", ".flac", ".aac", ".m4a", ".ogg", ".opus", ".wma", ".aiff",
];

/// Расширения видеофайлов (R1-В-1, строка 149).
pub const VIDEO_EXTENSIONS: &[&str] = &[
    ".mp4", ".mkv", ".avi", ".mov", ".wmv", ".webm", ".m4v", ".mts", ".m2ts", ".3gp",
];

/// Расширения архивов (R1-В-1, строка 150).
pub const ARCHIVE_EXTENSIONS: &[&str] =
    &[".zip", ".7z", ".rar", ".tar", ".gz", ".bz2", ".xz", ".iso"];

/// Расширения установщиков и программ (R1-В-1, строка 151).
pub const INSTALLER_EXTENSIONS: &[&str] = &[".exe", ".msi", ".msix", ".appx", ".apk"];

/// Расширения файлов исходного кода (R1-В-1, строка 152).
pub const SOURCE_EXTENSIONS: &[&str] = &[
    ".py", ".ps1", ".js", ".ts", ".tsx", ".jsx", ".java", ".c", ".cpp", ".h", ".cs", ".sql",
    ".ipynb", ".sh", ".bat", ".cmd",
];

/// Расширения текстовых файлов (R1-В-1, строка 153).
pub const TEXT_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".rst", ".csv", ".tsv", ".json", ".jsonl", ".xml", ".yaml", ".yml", ".ini",
    ".cfg", ".log", ".py", ".ps1", ".js", ".ts", ".tsx", ".jsx", ".java", ".c", ".cpp", ".h",
    ".cs", ".sql", ".sh", ".bat", ".cmd",
];

/// Расширения содержимого архивов для обычного режима (R1-В-1, строки 176–179).
pub const ARCHIVE_CONTENT_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".rst", ".csv", ".tsv", ".json", ".jsonl", ".xml", ".yaml", ".yml", ".ini",
    ".cfg", ".log", ".py", ".ps1", ".js", ".ts", ".tsx", ".jsx", ".java", ".c", ".cpp", ".h",
    ".cs", ".sql", ".sh", ".bat", ".cmd", ".fb2", ".epub", ".docx", ".xlsx", ".pptx", ".odt",
    ".ods", ".odp", ".rtf", ".html", ".htm", ".pdf",
];

/// Расширения содержимого архивов для режима «только документы» (R1-В-1, строки 180–182).
pub const DOCUMENT_ONLY_ARCHIVE_MEMBER_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".rst", ".pdf", ".docx", ".odt", ".rtf", ".epub", ".fb2", ".xls", ".xlsx",
    ".ods", ".ppt", ".pptx", ".odp", ".html", ".htm",
];

/// Расширения с обновлённым извлечением текста (R1-В-1, строки 170–175).
pub const LEGACY_CHANGED_TEXT_EXTENSIONS: &[&str] = &[
    ".pdf", ".fb2", ".epub", ".docx", ".odt", ".rtf", ".html", ".htm", ".txt", ".md", ".sql",
    ".jsonl", ".json", ".xml", ".log", ".csv", ".tsv", ".py", ".ps1", ".js", ".ts", ".tsx", ".jsx",
    ".java", ".c", ".cpp", ".h", ".cs", ".sh", ".bat", ".cmd",
];

/// Разделитель пути файла внутри архива (R1-В-2, строка 184).
pub const ARCHIVE_SEPARATOR: &str = " → ";

/// Текущая версия извлечения текста (R1-В-2, строка 169).
pub const TEXT_EXTRACTION_VERSION: u32 = 2;

/// Порог размера для больших txt/md файлов по умолчанию (5 МБ).
pub const LARGE_PLAIN_TEXT_BYTES: u64 = 5 * 1024 * 1024;

/// Максимальный размер файла внутри ZIP по умолчанию (20 МБ).
pub const DEFAULT_MAX_ARCHIVE_MEMBER_SIZE: u64 = 20 * 1024 * 1024;

/// Сопоставляет шаблон со звездочками `*` и вопросительными знаками `?` со строкой без учёта регистра.
pub fn matches_glob(pattern: &str, text: &str) -> bool {
    let p_chars: Vec<char> = pattern.chars().flat_map(|c| c.to_lowercase()).collect();
    let t_chars: Vec<char> = text.chars().flat_map(|c| c.to_lowercase()).collect();

    let (mut p_idx, mut t_idx) = (0, 0);
    let (mut star_idx, mut match_idx) = (None, 0);

    while t_idx < t_chars.len() {
        if p_idx < p_chars.len() && (p_chars[p_idx] == '?' || p_chars[p_idx] == t_chars[t_idx]) {
            p_idx += 1;
            t_idx += 1;
        } else if p_idx < p_chars.len() && p_chars[p_idx] == '*' {
            star_idx = Some(p_idx);
            p_idx += 1;
            match_idx = t_idx;
        } else if let Some(star) = star_idx {
            p_idx = star + 1;
            match_idx += 1;
            t_idx = match_idx;
        } else {
            return false;
        }
    }

    while p_idx < p_chars.len() && p_chars[p_idx] == '*' {
        p_idx += 1;
    }

    p_idx == p_chars.len()
}

/// Проверяет, исключена ли папка по имени.
pub fn is_folder_name_excluded(folder_name: &str, excluded_names: &[String]) -> bool {
    let lower_name = folder_name.to_lowercase();
    for pattern in excluded_names {
        let p_lower = pattern.to_lowercase();
        if p_lower.contains('*') || p_lower.contains('?') {
            if matches_glob(&p_lower, &lower_name) {
                return true;
            }
        } else if lower_name == p_lower {
            return true;
        }
    }
    false
}

/// Проверяет, переводит ли имя папки ветвь в режим «только документы».
pub fn is_document_only_folder_name(folder_name: &str, doc_only_names: &[String]) -> bool {
    let lower_name = folder_name.to_lowercase();
    for pattern in doc_only_names {
        let p_lower = pattern.to_lowercase();
        if p_lower.contains('*') || p_lower.contains('?') {
            if matches_glob(&p_lower, &lower_name) {
                return true;
            }
        } else if lower_name == p_lower {
            return true;
        }
    }
    false
}

/// Нормализует путь в виде строки: удаляет префикс `\\?\`, завершающие слэши,
/// переводит в нижний регистр и заменяет прямые слэши на обратные.
pub fn normalize_path_str(s: &str) -> String {
    let mut clean = s.trim();
    if let Some(stripped) = clean.strip_prefix(r"\\?\") {
        clean = stripped;
    }
    clean
        .trim_end_matches(['\\', '/'])
        .to_lowercase()
        .replace('/', "\\")
}

/// Проверяет, совпадает ли шаблон исключения с корнем источника или является ли его предком.
pub fn is_exclude_path_ancestor_or_self(raw_pattern: &str, source_root: &Path) -> bool {
    let expanded = expand_path_variables(raw_pattern);
    let exp_clean = normalize_path_str(&expanded);
    if exp_clean.is_empty() {
        return false;
    }

    let root_str = normalize_path_str(&source_root.to_string_lossy());
    if root_str.is_empty() {
        return false;
    }

    if !exp_clean.contains(':') && !exp_clean.starts_with('\\') {
        // Шаблон без диска (например, "Program Files")
        let pattern_with_slash = format!("\\{exp_clean}\\");
        let pattern_at_end = format!("\\{exp_clean}");
        root_str.contains(&pattern_with_slash) || root_str.ends_with(&pattern_at_end)
    } else {
        // Шаблон с полным путем
        root_str == exp_clean || root_str.starts_with(&format!("{exp_clean}\\"))
    }
}

/// Фильтрует список исключённых путей для заданного источника:
/// исключения, совпадающие с корнем источника или являющиеся его предком, не действуют на этот источник.
pub fn effective_exclude_paths(source_root: &Path, exclude_paths: &[String]) -> Vec<String> {
    exclude_paths
        .iter()
        .filter(|pattern| !is_exclude_path_ancestor_or_self(pattern, source_root))
        .cloned()
        .collect()
}

/// Проверяет, исключён ли путь по списку исключённых путей и папке данных.
pub fn is_path_excluded(path: &Path, exclude_paths: &[String], data_dir: Option<&Path>) -> bool {
    let path_clean = normalize_path_str(&path.to_string_lossy());
    if path_clean.is_empty() {
        return false;
    }

    if let Some(dd) = data_dir {
        let dd_clean = normalize_path_str(&dd.to_string_lossy());
        if !dd_clean.is_empty()
            && (path_clean == dd_clean || path_clean.starts_with(&format!("{dd_clean}\\")))
        {
            return true;
        }
    }

    for raw_pattern in exclude_paths {
        let expanded = expand_path_variables(raw_pattern);
        let exp_clean = normalize_path_str(&expanded);

        if exp_clean.is_empty() {
            continue;
        }

        // Если шаблон без диска (например, "Program Files", "$Recycle.Bin", "System Volume Information")
        if !exp_clean.contains(':') && !exp_clean.starts_with('\\') {
            // Проверяем совпадение любого компонента пути или совпадение в корне любого диска: X:\<exp_clean>
            let pattern_with_slash = format!("\\{exp_clean}\\");
            let pattern_at_end = format!("\\{exp_clean}");
            if path_clean.contains(&pattern_with_slash) || path_clean.ends_with(&pattern_at_end) {
                return true;
            }
        } else {
            // Шаблон с полным путем: точное совпадение или совпадение префикса слэшем
            if path_clean == exp_clean || path_clean.starts_with(&format!("{exp_clean}\\")) {
                return true;
            }
        }
    }

    false
}

#[cfg(windows)]
fn known_folder_path(folder_id: &windows::core::GUID) -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    unsafe {
        let path_ptr = SHGetKnownFolderPath(folder_id, KF_FLAG_DEFAULT, None).ok()?;
        let path_str = path_ptr.to_string().ok()?;
        CoTaskMemFree(Some(path_ptr.as_ptr() as *const _));
        Some(path_str)
    }
}

/// Подставляет переменные окружения %WINDIR%, %LOCALAPPDATA%, %APPDATA% в шаблон пути.
pub fn expand_path_variables(pattern: &str) -> String {
    let mut result = pattern.to_string();

    if result.contains("%WINDIR%") {
        let windir = std::env::var("WINDIR")
            .or_else(|_| std::env::var("SystemRoot"))
            .unwrap_or_else(|_| r"C:\Windows".to_string());
        result = result.replace("%WINDIR%", &windir);
    }
    if result.contains("%LOCALAPPDATA%") {
        let local = dirs::data_local_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| std::env::var("LOCALAPPDATA").ok())
            .or_else(|| {
                #[cfg(windows)]
                {
                    known_folder_path(&windows::Win32::UI::Shell::FOLDERID_LocalAppData)
                }
                #[cfg(not(windows))]
                {
                    None
                }
            })
            .unwrap_or_default();
        result = result.replace("%LOCALAPPDATA%", &local);
    }
    if result.contains("%APPDATA%") {
        let roaming = dirs::config_dir()
            .map(|p| p.to_string_lossy().into_owned())
            .or_else(|| std::env::var("APPDATA").ok())
            .or_else(|| {
                #[cfg(windows)]
                {
                    known_folder_path(&windows::Win32::UI::Shell::FOLDERID_RoamingAppData)
                }
                #[cfg(not(windows))]
                {
                    None
                }
            })
            .unwrap_or_default();
        result = result.replace("%APPDATA%", &roaming);
    }

    result
}

/// Проверяет, исключён ли файл по имени (шаблоны с `*` и `?`, без учёта регистра).
pub fn is_file_excluded(file_name: &str, exclude_files: &[String]) -> bool {
    let lower_name = file_name.to_lowercase();
    if lower_name.ends_with(".sqlite3")
        || lower_name.ends_with(".sqlite3-wal")
        || lower_name.ends_with(".sqlite3-shm")
        || lower_name.ends_with(".sqlite3-journal")
    {
        return true;
    }
    for pattern in exclude_files {
        let p_lower = pattern.to_lowercase();
        if p_lower.contains('*') || p_lower.contains('?') {
            if matches_glob(&p_lower, &lower_name) {
                return true;
            }
        } else if lower_name == p_lower {
            return true;
        }
    }
    false
}

/// Проверяет, является ли расширение допустимым в режиме «только документы».
pub fn is_document_only_extension(ext: &str) -> bool {
    let lower = ext.to_lowercase();
    DOCUMENT_ONLY_EXTENSIONS.contains(&lower.as_str())
}

/// Проверяет, допустим ли внутренний файл архива.
pub fn is_archive_content_extension(ext: &str, document_only: bool) -> bool {
    let lower = ext.to_lowercase();
    if document_only {
        DOCUMENT_ONLY_ARCHIVE_MEMBER_EXTENSIONS.contains(&lower.as_str())
    } else {
        ARCHIVE_CONTENT_EXTENSIONS.contains(&lower.as_str())
    }
}

/// Определяет категорию файла по пути и настройкам книжных слов (R1-А-39, catalogizer.py 283–312).
pub fn category_for(path: &str, book_folder_words: &[String]) -> &'static str {
    let normalized = path.replace('/', "\\");
    let parts: Vec<&str> = normalized.split('\\').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return "прочее";
    }

    let components_lower: Vec<String> = parts.iter().map(|p| p.to_lowercase()).collect();
    let dir_components_lower = if components_lower.len() > 1 {
        &components_lower[..components_lower.len() - 1]
    } else {
        &components_lower[..0]
    };

    // Проверка на резервную копию
    for comp in &components_lower {
        if comp.starts_with("backup")
            || comp.starts_with("резервн")
            || comp == "bak"
            || comp == "бэкап"
            || comp == "бэкапы"
        {
            return "резервная копия";
        }
    }

    let file_name = parts[parts.len() - 1];
    let ext = match file_name.rfind('.') {
        Some(idx) => file_name[idx..].to_lowercase(),
        None => String::new(),
    };

    if ext == ".bak" {
        return "резервная копия";
    }

    if EBOOK_EXTENSIONS.contains(&ext.as_str()) {
        return "книга";
    }

    // Проверка книжного контекста по словам папок из настроек
    let mut book_context = false;
    for dir in dir_components_lower {
        for word in book_folder_words {
            let w = word.to_lowercase();
            if w.ends_with('*') {
                let prefix = &w[..w.len() - 1];
                if dir.contains(prefix) {
                    book_context = true;
                    break;
                }
            } else if dir == &w {
                book_context = true;
                break;
            }
        }
        if book_context {
            break;
        }
    }

    if DOCUMENT_EXTENSIONS.contains(&ext.as_str()) && book_context {
        return "книга";
    }
    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return "изображение";
    }
    if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        return "аудио";
    }
    if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        return "видео";
    }
    if SOURCE_EXTENSIONS.contains(&ext.as_str()) {
        return "исходный код";
    }
    if DOCUMENT_EXTENSIONS.contains(&ext.as_str()) {
        return "документ";
    }
    if ARCHIVE_EXTENSIONS.contains(&ext.as_str()) {
        return "архив или образ";
    }
    if INSTALLER_EXTENSIONS.contains(&ext.as_str()) {
        return "установщик или программа";
    }
    "прочее"
}

/// Уточняет категории файлов для группы: если в папке есть электронная книга,
/// файлы PDF и DJVU в той же папке получают категорию «книга» (R1-А-39, R1-Б-5).
pub fn refine_book_categories_in_list<T, FGetPath, FGetExt, FSetCategory>(
    items: &mut [T],
    get_path: FGetPath,
    get_ext: FGetExt,
    set_category: FSetCategory,
) where
    FGetPath: Fn(&T) -> &str,
    FGetExt: Fn(&T) -> &str,
    FSetCategory: Fn(&mut T, &'static str),
{
    use std::collections::HashSet;

    let mut ebook_dirs = HashSet::new();
    for item in items.iter() {
        let ext = get_ext(item).to_lowercase();
        if EBOOK_EXTENSIONS.contains(&ext.as_str()) {
            let path = get_path(item).replace('/', "\\");
            if let Some(parent) = path.rfind('\\') {
                ebook_dirs.insert(path[..parent].to_lowercase());
            }
        }
    }

    if ebook_dirs.is_empty() {
        return;
    }

    for item in items.iter_mut() {
        let ext = get_ext(item).to_lowercase();
        if ext == ".pdf" || ext == ".djvu" {
            let path = get_path(item).replace('/', "\\");
            if let Some(parent) = path.rfind('\\')
                && ebook_dirs.contains(&path[..parent].to_lowercase())
            {
                set_category(item, "книга");
            }
        }
    }
}

/// Проверяет, является ли путь закрытыми данными (секреты, токены, пароли не в коде)
/// (R1-А-114, 115, catalogizer.py 1927–1937).
pub fn is_sensitive_path(path: &str, extra_patterns: &[String]) -> bool {
    let lowered = path.to_lowercase().replace('/', "\\");
    let parts: Vec<&str> = lowered.split('\\').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return false;
    }

    for &part in &parts {
        if part == ".ssh" || part == ".gnupg" || part == "учетки" || part == "учётки" {
            return true;
        }
    }

    let file_name = parts[parts.len() - 1];
    let ext = match file_name.rfind('.') {
        Some(idx) => &file_name[idx..],
        None => "",
    };

    let env_file = file_name == ".env" || file_name.starts_with(".env.") || ext == ".env";
    let named_password_file = (file_name.contains("password") || file_name.contains("парол"))
        && !SOURCE_EXTENSIONS.contains(&ext);

    if env_file || named_password_file {
        return true;
    }

    let sensitive_tokens = [
        "\\.env",
        "auth.json",
        "credentials",
        "secrets",
        "logins.json",
        "private_key",
    ];
    for token in sensitive_tokens {
        if lowered.contains(token) {
            return true;
        }
    }

    for pat in extra_patterns {
        let p_lower = pat.to_lowercase();
        if matches_glob(&p_lower, file_name)
            || matches_glob(&p_lower, &lowered)
            || lowered.contains(&p_lower)
        {
            return true;
        }
    }

    false
}

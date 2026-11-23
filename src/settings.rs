use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Режим индексирования источника.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SourceMode {
    /// Полное индексирование (имена, свойства, текст всех поддерживаемых файлов).
    #[serde(alias = "full", alias = "полностью")]
    #[default]
    Full,
    /// Только документы (без служебных файлов и кода).
    #[serde(alias = "documents_only", alias = "только документы")]
    DocumentsOnly,
    /// Только имена и свойства файлов (без извлечения текста).
    #[serde(alias = "names_only", alias = "только имена")]
    NamesOnly,
}

/// Источник данных для индексирования (диск, папка или файл).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Путь к диску или папке источника.
    pub path: PathBuf,
    /// Режим индексирования источника.
    #[serde(default)]
    pub mode: SourceMode,
    /// Предельная глубина вложенности (0 — без предела).
    #[serde(default)]
    pub max_depth: u32,
    /// Включён ли источник в поиск.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Серийный номер тома для отслеживания смены букв дисков.
    #[serde(default)]
    pub volume_serial: Option<u32>,
    /// Дополнительные шаблоны исключений для этого источника.
    #[serde(default)]
    pub extra_excludes: Vec<String>,
}

/// Способ извлечения текста из PDF-документов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PdfExtractorMode {
    /// Встроенный извлекатель на чистом Rust.
    #[serde(alias = "builtin", alias = "встроенный")]
    #[default]
    Builtin,
    /// Внешняя утилита pdftotext.
    #[serde(alias = "external", alias = "внешний")]
    External,
}

/// Режим поиска нескольких слов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MultiWordMode {
    /// Строгая фраза: слова подряд в порядке запроса, словоформы на каждой позиции.
    #[serde(alias = "strict_phrase", alias = "строгая фраза")]
    #[default]
    StrictPhrase,
    /// Все слова в любом месте документа, точная фраза ранжируется выше.
    #[serde(alias = "all_words", alias = "все слова")]
    AllWords,
}

/// Режим словоформ в запросе (не влияет на индексацию).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WordFormsMode {
    /// Все формы слова (при наличии словаря и включённых словоформ), иначе слова с таким началом.
    #[serde(alias = "all_forms", alias = "все формы")]
    #[default]
    AllForms,
    /// Точно как набрано: целое слово в этой форме, без продолжений и других форм.
    #[serde(alias = "exact", alias = "точно как набрано")]
    Exact,
}

/// Порядок сортировки результатов поиска по умолчанию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SortMode {
    /// По соответствию (точная фраза, затем релевантность BM25).
    #[serde(alias = "relevance", alias = "по соответствию")]
    #[default]
    Relevance,
    /// Сначала крупные файлы.
    #[serde(alias = "size_desc", alias = "сначала крупные")]
    SizeDesc,
    /// Сначала новые файлы (по дате изменения).
    #[serde(alias = "date_desc", alias = "сначала новые")]
    DateDesc,
    /// По имени файла по алфавиту.
    #[serde(alias = "name_asc", alias = "по имени")]
    NameAsc,
}

fn default_true() -> bool {
    true
}

/// Полная конфигурация программы ПоисКовичеК (раздел 5 ТЗ).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // === Вкладка «Источники» ===
    /// Список источников поиска (диски, папки) с индивидуальными настройками.
    pub sources: Vec<Source>,

    // === Вкладка «Исключения» ===
    /// Имена папок, исключаемые из обхода (шаблоны).
    pub exclude_folder_names: Vec<String>,
    /// Имена папок, для которых принудительно включается режим «только документы».
    pub documents_only_folder_names: Vec<String>,
    /// Системные и служебные пути, исключаемые из обхода.
    pub exclude_paths: Vec<String>,
    /// Шаблоны файлов, исключаемых из обхода.
    pub exclude_files: Vec<String>,

    // === Вкладка «Форматы и пределы» ===
    /// Извлекать текст из текстовых форматов (txt, md, csv, json, xml, yaml, ini, log, исходный код).
    pub extract_text_formats: bool,
    /// Извлекать текст из офисных документов (docx, xlsx, pptx, odt, ods, odp).
    pub extract_office_formats: bool,
    /// Извлекать текст из книг и форматированного текста (html, htm, rtf, fb2, epub).
    pub extract_book_formats: bool,
    /// Извлекать текст из файлов PDF.
    pub extract_pdf_format: bool,
    /// Предел извлекаемых символов для книг и документов.
    pub limit_chars_books_documents: usize,
    /// Предел извлекаемых символов для исходного кода и служебных файлов.
    pub limit_chars_code_service: usize,
    /// Предел извлекаемых символов для прочих форматов.
    pub limit_chars_other: usize,
    /// Предел символов для больших текстовых файлов (txt/md).
    pub limit_chars_large_txt: usize,
    /// Порог размера в мегабайтах, начиная с которого txt/md считается большим.
    pub large_txt_threshold_mb: u64,
    /// Заходить внутрь ZIP-архивов для индексирования содержимого.
    pub zip_scan: bool,
    /// Предельный размер файла внутри ZIP-архива для чтения, МБ.
    pub zip_max_file_size_mb: u64,
    /// Способ извлечения текста из PDF (встроенный или внешний pdftotext).
    pub pdf_extractor: PdfExtractorMode,
    /// Путь к внешней утилите pdftotext (если выбран внешний режим).
    pub pdf_external_path: PathBuf,
    /// Предельное время извлечения текста из одного файла в секундах.
    pub extract_timeout_seconds: u64,

    // === Вкладка «Закрытые данные» ===
    /// Дополнительные пользовательские шаблоны закрытых данных, текст которых не читается.
    pub secret_extra_patterns: Vec<String>,

    // === Вкладка «Поиск» ===
    /// Использовать морфологический словарь русских словоформ.
    pub enable_morphology: bool,
    /// Режим сопоставления нескольких слов в запросе.
    pub multi_word_mode: MultiWordMode,
    /// Режим словоформ в запросе: все формы или точно как набрано.
    pub word_forms_mode: WordFormsMode,
    /// Максимальное число результатов в выдаче (от 1 до 500).
    pub max_results: usize,
    /// Длина фрагмента текста до совпадения (символов).
    pub fragment_before: usize,
    /// Длина фрагмента текста после совпадения (символов).
    pub fragment_after: usize,
    /// Порядок сортировки результатов по умолчанию.
    pub default_sort: SortMode,
    /// Слова в имени папки, указывающие на книжную категорию.
    pub book_folder_words: Vec<String>,

    // === Вкладка «Обновление» ===
    /// Автоматически искать изменения и обновлять поиск при запуске программы.
    pub update_on_startup: bool,
    /// Интервал периодического фонового обновления в минутах (0 — выключено).
    pub update_interval_minutes: u32,
    /// Автозапуск программы при входе пользователя в Windows.
    pub autostart: bool,

    // === Вкладка «Данные и производительность» ===
    /// Пользовательский путь к папке данных (пустая строка — по умолчанию %LOCALAPPDATA%\ПоисКовичеК).
    pub data_dir: PathBuf,
    /// Число рабочих процессов извлечения текста (0 — автоматически по числу ядер процессора).
    pub worker_threads: usize,
    /// Щадящий режим для жестких дисков (HDD): не более одного потока чтения на физический том.
    pub hdd_gentle_mode: bool,
    /// Срок хранения файлов журнала в днях.
    pub log_keep_days: u32,

    // === Окно и состояние интерфейса ===
    /// Запомненная ширина окна
    #[serde(default)]
    pub window_width: Option<u32>,
    /// Запомненная высота окна
    #[serde(default)]
    pub window_height: Option<u32>,
    /// Запомненное положение окна X
    #[serde(default)]
    pub window_x: Option<i32>,
    /// Запомненное положение окна Y
    #[serde(default)]
    pub window_y: Option<i32>,
    /// История последних поисковых запросов (до 50 записей)
    #[serde(default)]
    pub query_history: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            exclude_folder_names: vec![
                ".git",
                ".svn",
                ".hg",
                "node_modules",
                "__pycache__",
                ".pytest_cache",
                ".mypy_cache",
                ".ruff_cache",
                ".cache",
                ".tmp",
                ".venv",
                "venv",
                "site-packages",
                "target",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            documents_only_folder_names: vec!["tmp".to_string(), "cache".to_string()],
            exclude_paths: vec![
                "%WINDIR%",
                "Program Files",
                "Program Files (x86)",
                "ProgramData",
                "$Recycle.Bin",
                "System Volume Information",
                "Recovery",
                "PerfLogs",
                "%LOCALAPPDATA%",
                "%APPDATA%",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            exclude_files: vec![
                "hiberfil.sys",
                "pagefile.sys",
                "swapfile.sys",
                "dumpstack.log",
                "dumpstack.log.tmp",
                "ntuser.dat*",
                "~$*",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            extract_text_formats: true,
            extract_office_formats: true,
            extract_book_formats: true,
            extract_pdf_format: true,
            limit_chars_books_documents: 20_000_000,
            limit_chars_code_service: 200_000,
            limit_chars_other: 100_000,
            limit_chars_large_txt: 200_000,
            large_txt_threshold_mb: 5,
            zip_scan: true,
            zip_max_file_size_mb: 20,
            pdf_extractor: PdfExtractorMode::Builtin,
            pdf_external_path: PathBuf::new(),
            extract_timeout_seconds: 120,
            secret_extra_patterns: Vec::new(),
            enable_morphology: true,
            multi_word_mode: MultiWordMode::StrictPhrase,
            word_forms_mode: WordFormsMode::AllForms,
            max_results: 200,
            fragment_before: 140,
            fragment_after: 300,
            default_sort: SortMode::Relevance,
            book_folder_words: vec![
                "books",
                "ebooks",
                "литература",
                "library",
                "библиотека",
                "книг*",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            update_on_startup: true,
            update_interval_minutes: 60,
            autostart: false,
            data_dir: PathBuf::new(),
            worker_threads: 0,
            hdd_gentle_mode: false,
            log_keep_days: 14,
            window_width: None,
            window_height: None,
            window_x: None,
            window_y: None,
            query_history: Vec::new(),
        }
    }
}

/// Ошибка при загрузке или сохранении настроек.
#[derive(Debug)]
pub enum SettingsError {
    /// Ошибка ввода-вывода.
    Io(std::io::Error),
    /// Ошибка синтаксиса или разбора файла с указанием строки.
    Parse {
        line: Option<usize>,
        message: String,
    },
    /// Ошибка сериализации настроек.
    Serialize(String),
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "Ошибка чтения файла настроек: {err}"),
            Self::Parse {
                line: Some(l),
                message,
            } => {
                write!(f, "Ошибка в строке {l} файла настроек: {message}")
            }
            Self::Parse {
                line: None,
                message,
            } => {
                write!(f, "Ошибка разбора файла настроек: {message}")
            }
            Self::Serialize(msg) => write!(f, "Ошибка сохранения настроек: {msg}"),
        }
    }
}

impl std::error::Error for SettingsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for SettingsError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl Settings {
    /// Загружает настройки из указанного файла.
    ///
    /// - Если файл не существует, возвращаются настройки по умолчанию.
    /// - Неизвестные поля в файле игнорируются.
    /// - Отсутствующие поля заполняются значениями по умолчанию.
    /// - Если файл поврежден, возвращается ошибка с текстом и номером строки.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = fs::read_to_string(path)?;
        Self::from_toml_str(&content)
    }

    /// Возвращает копию настроек для самопроверки (`--self-test <папка>`).
    ///
    /// Чистая функция: ничего не читает и не пишет на диск. В копию добавляются источник
    /// `docs_dir` и исключения `self-test.txt` / `self-test.log`; если задана `temp_data_dir`,
    /// она становится папкой данных. Файл настроек пользователя самопроверка не сохраняет.
    pub fn for_self_test(&self, docs_dir: &Path, temp_data_dir: Option<&Path>) -> Self {
        let mut settings = self.clone();
        if !settings.sources.iter().any(|s| s.path == docs_dir) {
            settings.sources.push(Source {
                path: docs_dir.to_path_buf(),
                mode: SourceMode::Full,
                max_depth: 0,
                enabled: true,
                volume_serial: None,
                extra_excludes: Vec::new(),
            });
        }
        for name in ["self-test.txt", "self-test.log"] {
            if !settings.exclude_files.iter().any(|f| f == name) {
                settings.exclude_files.push(name.to_string());
            }
        }
        if let Some(dir) = temp_data_dir {
            settings.data_dir = dir.to_path_buf();
        }
        settings
    }

    /// Разбирает настройки из строки TOML.
    pub fn from_toml_str(content: &str) -> Result<Self, SettingsError> {
        match toml::from_str::<Self>(content) {
            Ok(settings) => Ok(settings),
            Err(err) => {
                let line = err.span().map(|span| {
                    let end = span.start.min(content.len());
                    content[..end].chars().filter(|&c| c == '\n').count() + 1
                });
                let message = err.message().to_string();
                Err(SettingsError::Parse { line, message })
            }
        }
    }

    /// Атомарно сохраняет настройки в файл: запись во временный файл рядом + переименование.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;

        let toml_str =
            toml::to_string_pretty(self).map_err(|e| SettingsError::Serialize(e.to_string()))?;

        // Временный файл в той же папке для гарантии атомарного перемещения
        let temp_filename = format!(
            ".settings.{}.{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        let temp_path = parent.join(temp_filename);

        fs::write(&temp_path, toml_str)?;

        if let Err(err) = fs::rename(&temp_path, path) {
            let _ = fs::remove_file(&temp_path);
            return Err(SettingsError::Io(err));
        }

        Ok(())
    }

    /// Проверяет корректность настроек.
    ///
    /// Возвращает список понятных сообщений об ошибках на русском языке.
    /// Для корректных настроек возвращается пустой вектор.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.max_results < 1 || self.max_results > 500 {
            errors.push(format!(
                "Число результатов должно быть от 1 до 500 (задано: {})",
                self.max_results
            ));
        }

        if self.limit_chars_books_documents == 0 {
            errors.push("Предел символов для книг и документов должен быть больше 0".into());
        }
        if self.limit_chars_code_service == 0 {
            errors.push(
                "Предел символов для исходного кода и служебных файлов должен быть больше 0".into(),
            );
        }
        if self.limit_chars_other == 0 {
            errors.push("Предел символов для прочих форматов должен быть больше 0".into());
        }
        if self.limit_chars_large_txt == 0 {
            errors.push("Предел символов для больших txt/md файлов должен быть больше 0".into());
        }
        if self.large_txt_threshold_mb == 0 {
            errors.push("Порог «большой txt/md» должен быть больше 0 МБ".into());
        }
        if self.zip_max_file_size_mb == 0 {
            errors.push("Предел размера файла внутри ZIP должен быть больше 0 МБ".into());
        }
        if self.extract_timeout_seconds == 0 {
            errors
                .push("Предел времени извлечения одного файла должен быть больше 0 секунд".into());
        }
        if self.log_keep_days == 0 {
            errors.push("Срок хранения журнала должен быть больше 0 дней".into());
        }
        if self.update_interval_minutes > 1440 {
            errors.push(format!(
                "Интервал автообновления не может превышать 1440 минут (24 ч), задано: {}",
                self.update_interval_minutes
            ));
        }
        if self.worker_threads > 64 {
            errors.push(format!(
                "Число рабочих процессов не может превышать 64, задано: {}",
                self.worker_threads
            ));
        }

        if !self.data_dir.as_os_str().is_empty() && !self.data_dir.is_absolute() {
            errors.push(format!(
                "Папка данных должна быть абсолютным путём (задано: '{}')",
                self.data_dir.display()
            ));
        }

        if self.pdf_extractor == PdfExtractorMode::External {
            if self.pdf_external_path.as_os_str().is_empty() {
                errors.push(
                    "Для внешнего извлекателя PDF необходимо указать путь к pdftotext".into(),
                );
            } else if !self.pdf_external_path.is_absolute() {
                errors.push(format!(
                    "Путь к внешнему pdftotext должен быть абсолютным (задано: '{}')",
                    self.pdf_external_path.display()
                ));
            }
        }

        for source in &self.sources {
            if !source.path.is_absolute() {
                errors.push(format!(
                    "Путь источника '{}' должен быть абсолютным",
                    source.path.display()
                ));
            }
        }

        errors
    }
}

//! Модуль конвейера индексации ПоисКовичеКа (раздел 2.6 ТЗ).
//!
//! Потоковая схема:
//! [потоки обхода] --Entry/Skip--> очередь A (cap)
//! [поток сверки] читает A, сверяет со снимком в памяти (база НЕ читается),
//!     New/Changed/Stale -> очередь B (cap) задания на извлечение,
//!     Unchanged -> ничего, всё что нужно записать без извлечения -> очередь C (cap)
//! [читатели по дискам] (если `po_diskam`): поток-распределитель берёт B и кладёт задание в
//!     очередь своего физического диска; у каждого диска свои потоки чтения (по классу диска),
//!     читатель кладёт в память файл (≤ 16 МБ, не из архива) и передаёт в общую очередь
//!     извлечения (cap = 2 × число потоков извлечения)
//! [пул извлечения: N рабочих процессов] читает B (или очередь извлечения) -> очередь C (cap)
//! [ОДИН писатель] читает C: пишет в базу порциями (транзакция ≤ 500 записей или ≤ 1 с),
//!     сохраняет текст (`catalog::store_text`), добавляет/удаляет документы tantivy
//!     по таблице `index_outbox`, коммит tantivy не чаще 1 раза в 2 с и в конце.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{
    Receiver, RecvTimeoutError, SendTimeoutError, Sender, TrySendError, bounded,
};

use crate::catalog::{Catalog, CatalogError, FileRecord};
use crate::extract::{EXTRACTION_VERSION, ExtractConfig, Extracted};
use crate::index::{IndexError, IndexWriterHandle};
use crate::morph::Morph;
use crate::planirovshchik::{self, KlyuchDiska, OpredelitelDiskov, PREDEL_CHTENIYA};
use crate::rules::{ARCHIVE_SEPARATOR, category_for, is_sensitive_path};
use crate::schetchiki::{
    Izmeritel, OtchetEtapov, SchetchikiEtapov, dobavit, otpravit_s_uchetom, poluchit_s_tajmautom,
    poluchit_s_uchetom, pribavit,
};
use crate::settings::{Settings, Source, SourceMode};
use crate::sync::{SyncDecision, classify_entry};
use crate::walk::{WalkEntry, WalkSkip, walk_source_counting};
use crate::worker::{WorkerPool, WorkerTask};

pub const DEFAULT_QUEUE_CAPACITY: usize = 64;

/// Ошибки конвейера индексации.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("Ошибка ввода-вывода конвейера: {0}")]
    Io(#[from] io::Error),
    #[error("Ошибка базы данных: {0}")]
    Catalog(#[from] CatalogError),
    #[error("Ошибка SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Ошибка поискового индекса: {0}")]
    Index(#[from] IndexError),
    #[error("Ошибка синхронизации: {0}")]
    Sync(#[from] crate::sync::SyncError),
    #[error("База уже обслуживается другим окном ПоисКовичеКа: {0}")]
    Locked(String),
    #[error("Конвейер был отменён")]
    Cancelled,
    #[error("Ошибка конвейера: {0}")]
    Other(String),
}

/// Токен отмены конвейера.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    cancelled: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Итоговая сводка выполненной синхронизации/индексации.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FinishedSummary {
    pub added: usize,
    pub changed: usize,
    pub deleted: usize,
}

/// Живые счётчики хода индексирования одного источника.
/// Пишутся из потоков обхода, сверки и из писателя.
#[derive(Debug, Default)]
pub struct SourceProgress {
    /// Сколько файлов найдено при обходе (список для обработки).
    pub found: AtomicUsize,
    /// Список файлов источника составлен полностью.
    pub list_done: AtomicBool,
    /// Сколько файлов уже решено: записан, удалён, пропущен или не изменился.
    pub done: AtomicUsize,
    /// Сколько байтов прочитано при извлечении текста.
    pub bytes_read: AtomicU64,
}

/// Снимок счётчиков одного источника для окна программы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceProgressView {
    /// Короткая подпись: «C:» для диска, имя папки для папки.
    pub label: String,
    /// Полный путь источника.
    pub path: String,
    pub found: usize,
    pub list_done: bool,
    pub done: usize,
    pub bytes_read: u64,
    /// Список составлен и все файлы решены.
    pub finished: bool,
}

/// Подпись источника для окна программы.
#[derive(Debug, Clone)]
struct SourceLabel {
    path: String,
    label: String,
}

/// Короткая подпись источника: буква диска или имя папки.
fn source_label(path: &Path) -> String {
    let full = path.to_string_lossy().to_string();
    let trimmed = full.trim_end_matches('\\');
    if trimmed.len() == 2 && trimmed.ends_with(':') {
        return trimmed.to_string();
    }
    if let Some(name) = path.file_name() {
        let name = name.to_string_lossy().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    trimmed.to_string()
}

/// Индекс источника, которому принадлежит путь: самое длинное совпадение префикса.
fn source_index_for_path(path: &str, source_roots: &[String]) -> Option<usize> {
    let mut best: Option<(usize, usize)> = None;
    for (idx, root) in source_roots.iter().enumerate() {
        let matches = path == root
            || path
                .strip_prefix(root.as_str())
                .is_some_and(|rest| rest.starts_with('\\') || rest.starts_with(ARCHIVE_SEPARATOR));
        if !matches {
            continue;
        }
        if best.is_none_or(|(_, len)| root.len() > len) {
            best = Some((idx, root.len()));
        }
    }
    best.map(|(idx, _)| idx)
}

/// Папка файла; для члена архива — папка самого архива.
fn folder_of_path(path: &str) -> String {
    let file_part = path.split(ARCHIVE_SEPARATOR).next().unwrap_or(path);
    Path::new(file_part)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Событие прогресса для окна программы.
#[derive(Debug, Clone)]
pub struct Progress {
    pub phase: String,
    pub walked: usize,
    pub extracted: usize,
    pub written: usize,
    pub current_dir: String,
    pub bytes_per_sec: u64,
    pub files_per_sec: f64,
    pub sources: Vec<SourceProgressView>,
    pub total_found: usize,
    pub total_done: usize,
    pub finished_summary: Option<FinishedSummary>,
}

/// Межпроцессная блокировка файла `<папка данных>\index.lock`.
pub struct IndexLock {
    _file: fs::File,
    path: PathBuf,
}

impl IndexLock {
    /// Захватывает эксклюзивную блокировку базы и индекса.
    pub fn acquire(data_dir: &Path) -> Result<Self, PipelineError> {
        fs::create_dir_all(data_dir)?;
        let lock_path = data_dir.join("index.lock");

        use std::os::windows::fs::OpenOptionsExt;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0) // share_mode(0) = FILE_SHARE_NONE (эксклюзивное открытие)
            .open(&lock_path)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(32) {
                    PipelineError::Locked(
                        "База уже обслуживается другим окном ПоисКовичеКа".to_string(),
                    )
                } else {
                    PipelineError::Io(e)
                }
            })?;

        Ok(Self {
            _file: file,
            path: lock_path,
        })
    }

    /// Путь к файлу блокировки.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Настройки запуска конвейера.
#[derive(Debug, Clone)]
pub struct PipelineOptions {
    pub queue_capacity: usize,
    pub in_process_workers: bool,
    pub num_workers: usize,
    pub cancel_token: Option<CancelToken>,
    pub progress_tx: Option<Sender<Progress>>,
    pub test_hook: Option<String>,
    pub morph: Option<Morph>,
    /// Читать файлы отдельными потоками для каждого физического диска (по умолчанию `true`).
    /// `false` — прежний путь: файлы читает сам пул извлечения.
    pub po_diskam: bool,
}

impl Default for PipelineOptions {
    fn default() -> Self {
        Self {
            queue_capacity: DEFAULT_QUEUE_CAPACITY,
            in_process_workers: false,
            num_workers: 0,
            cancel_token: None,
            progress_tx: None,
            test_hook: None,
            morph: None,
            po_diskam: true,
        }
    }
}

/// Внутренний диспетчер сообщений прогресса с ограничением частоты (≤ 10 раз/с).
struct ProgressReporter {
    tx: Option<Sender<Progress>>,
    last_sent: Instant,
    start_time: Instant,
    /// Последние замеры (время, байты, решённые файлы) для скользящего среднего.
    samples: VecDeque<(Instant, u64, u64)>,
    sources: Arc<Vec<SourceProgress>>,
    labels: Vec<SourceLabel>,
    walked: usize,
    extracted: usize,
    written: usize,
    current_dir: String,
}

impl ProgressReporter {
    fn new(
        tx: Option<Sender<Progress>>,
        sources: Arc<Vec<SourceProgress>>,
        labels: Vec<SourceLabel>,
    ) -> Self {
        let now = Instant::now();
        Self {
            tx,
            last_sent: now,
            start_time: now,
            samples: VecDeque::new(),
            sources,
            labels,
            walked: 0,
            extracted: 0,
            written: 0,
            current_dir: String::new(),
        }
    }

    fn update(&mut self, walked: usize, extracted: usize, written: usize) {
        self.walked = walked;
        self.extracted = extracted;
        self.written = written;
    }

    fn set_current_dir(&mut self, path: &str) {
        let dir = folder_of_path(path);
        if !dir.is_empty() {
            self.current_dir = dir;
        }
    }

    fn snapshot(&self) -> Vec<SourceProgressView> {
        self.sources
            .iter()
            .zip(self.labels.iter())
            .map(|(p, l)| {
                let found = p.found.load(Ordering::Relaxed);
                let done = p.done.load(Ordering::Relaxed);
                let list_done = p.list_done.load(Ordering::Relaxed);
                SourceProgressView {
                    label: l.label.clone(),
                    path: l.path.clone(),
                    found,
                    list_done,
                    done,
                    bytes_read: p.bytes_read.load(Ordering::Relaxed),
                    finished: list_done && done >= found,
                }
            })
            .collect()
    }

    /// Скорость за последние 4 секунды: байты в секунду и файлы в секунду.
    fn rates(&mut self, now: Instant) -> (u64, f64) {
        let total_bytes: u64 = self
            .sources
            .iter()
            .map(|p| p.bytes_read.load(Ordering::Relaxed))
            .sum();
        let total_done: u64 = self
            .sources
            .iter()
            .map(|p| p.done.load(Ordering::Relaxed) as u64)
            .sum();

        self.samples.push_back((now, total_bytes, total_done));
        while self.samples.len() > 2
            && now.duration_since(self.samples[0].0) > Duration::from_secs(4)
        {
            self.samples.pop_front();
        }

        let (start_at, start_bytes, start_done) = self.samples[0];
        let window_secs = now.duration_since(start_at).as_secs_f64();
        if self.samples.len() >= 2 && window_secs >= 0.05 {
            let bytes_per_sec =
                (total_bytes.saturating_sub(start_bytes) as f64 / window_secs) as u64;
            let files_per_sec = total_done.saturating_sub(start_done) as f64 / window_secs;
            return (bytes_per_sec, files_per_sec);
        }

        let elapsed = self.start_time.elapsed().as_secs_f64();
        if elapsed > 0.05 {
            (
                (total_bytes as f64 / elapsed) as u64,
                total_done as f64 / elapsed,
            )
        } else {
            (0, 0.0)
        }
    }

    fn build(&mut self, phase: &str, finished_summary: Option<FinishedSummary>) -> Progress {
        let (bytes_per_sec, files_per_sec) = self.rates(Instant::now());
        let sources = self.snapshot();
        let total_found = sources.iter().map(|s| s.found).sum();
        let total_done = sources.iter().map(|s| s.done).sum();
        Progress {
            phase: phase.to_string(),
            walked: self.walked,
            extracted: self.extracted,
            written: self.written,
            current_dir: self.current_dir.clone(),
            bytes_per_sec,
            files_per_sec,
            sources,
            total_found,
            total_done,
            finished_summary,
        }
    }

    fn maybe_send(&mut self, phase: &str, force: bool) {
        if self.tx.is_none() {
            return;
        }
        let now = Instant::now();
        if !force && now.duration_since(self.last_sent) < Duration::from_millis(100) {
            return;
        }
        self.last_sent = now;
        let progress = self.build(phase, None);
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(progress);
        }
    }

    fn send_finished(&mut self, summary: FinishedSummary) {
        let Some(tx) = self.tx.clone() else {
            return;
        };
        let progress = self.build("готово", Some(summary));
        // Итоговое сообщение важнее промежуточных: ждём место в очереди не дольше 1 с
        let _ = tx.send_timeout(progress, Duration::from_secs(1));
    }
}

/// Сообщения очереди A (от потоков обхода к потоку сверки).
enum QueueAMsg {
    Entry(WalkEntry),
    Skip(WalkSkip),
    Finished {
        source_path: String,
        is_available: bool,
        protected_prefixes: Vec<String>,
    },
}

/// Задание на извлечение текста в очереди B (от потока сверки к пулу извлечения).
struct ExtractTask {
    entry: WalkEntry,
    is_new: bool,
    existing_id: Option<i64>,
    previous_text_sha: Option<String>,
    is_sensitive: bool,
    source_db_id: Option<i64>,
    volume_id: Option<i64>,
    /// Прочитанное читателем диска содержимое; извлечение забирает его (`take`).
    data: Option<Vec<u8>>,
}

/// Сообщения очереди C (от сверки и рабочих процессов к ОДНОМУ писателю).
enum QueueCMsg {
    /// Запись без извлечения текста (например, ZIP-контейнер или файлы в режиме "только свойства")
    DirectRecord { record: FileRecord, is_new: bool },
    /// Результат извлечения текста из пула рабочих процессов
    ExtractedResult {
        task: ExtractTask,
        extracted: Extracted,
    },
    /// Список ID файлов на удаление из базы и индекса вместе с путями для счётчика
    DeleteIds(Vec<(i64, String)>),
    /// Запись о пропуске при обходе
    SkipRecord(WalkSkip),
    /// Обновление статуса источника
    SourceStatus {
        source_path: String,
        status: String,
        details: Option<String>,
    },
}

/// Читает содержимое файла внутри ZIP-архива в виде байтов.
fn read_archive_member_bytes(archive_path: &str, member_name: &str) -> io::Result<Vec<u8>> {
    let file = fs::File::open(archive_path)?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| io::Error::other(e.to_string()))?;
    // Член ищется по тому же правилу имени, что и при обходе (walk::normalize_zip_member_name)
    let index = crate::walk::find_zip_member_index(&mut zip, member_name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("файл не найден внутри архива: {member_name}"),
        )
    })?;
    let mut entry = zip
        .by_index(index)
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    io::Read::read_to_end(&mut entry, &mut buf)?;
    Ok(buf)
}

/// Создаёт заготовку FileRecord из WalkEntry.
fn build_file_record(
    entry: &WalkEntry,
    run_id: i64,
    source_id: Option<i64>,
    volume_id: Option<i64>,
    book_folder_words: &[String],
    existing_id: Option<i64>,
) -> FileRecord {
    let cat = category_for(&entry.path, book_folder_words);
    let ext = Path::new(&entry.path)
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{}", s.to_lowercase()))
        .unwrap_or_default();

    let name = if entry.inside_archive {
        entry
            .path
            .rsplit(ARCHIVE_SEPARATOR)
            .next()
            .unwrap_or(&entry.path)
            .rsplit('\\')
            .next()
            .unwrap_or(&entry.path)
            .to_string()
    } else {
        Path::new(&entry.path)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    };

    FileRecord {
        id: existing_id,
        path: entry.path.clone(),
        source_id,
        volume_id,
        rel_path: entry.rel_path.clone(),
        name,
        extension: ext,
        size: entry.size,
        modified_ns: entry.modified_ns,
        created_ns: entry.created_ns,
        category: cat.to_string(),
        inside_archive: entry.inside_archive,
        container_id: None,
        content_status: Some("pending".to_string()),
        extraction_version: 0,
        text_sha: None,
        quick_hash: None,
        sha256: None,
        error: None,
        scan_run: Some(run_id),
        available: true,
    }
}

/// Шаг ожидания при блокирующих операциях с очередями: так отмена замечается быстро.
const SHAG_OZHIDANIYA: Duration = Duration::from_millis(100);

/// Ёмкость очереди одного физического диска (в ней только описания заданий, без содержимого).
const OCHERED_DISKA: usize = 4096;

/// Кладёт значение в очередь, не зависая при отмене и закрытии очереди.
/// `false` — значение не доставлено (отмена или все получатели ушли).
/// Время ожидания места (очередь была полна) прибавляется к `zhdal`.
fn polozhit_v_ochered<T>(
    tx: &Sender<T>,
    mut value: T,
    cancel: &CancelToken,
    zhdal: &AtomicU64,
) -> bool {
    value = match tx.try_send(value) {
        Ok(()) => return true,
        Err(TrySendError::Disconnected(_)) => return false,
        Err(TrySendError::Full(back)) => back,
    };
    let _izmeritel = Izmeritel::new(zhdal);
    loop {
        match tx.send_timeout(value, SHAG_OZHIDANIYA) {
            Ok(()) => return true,
            Err(SendTimeoutError::Timeout(back)) => {
                if cancel.is_cancelled() {
                    return false;
                }
                value = back;
            }
            Err(SendTimeoutError::Disconnected(_)) => return false,
        }
    }
}

/// Читает содержимое файла в память, если оно понадобится извлекателю.
/// Файл из архива, больше 16 МБ, закрытый, ненужный формату и любая ошибка чтения —
/// задание остаётся без данных, и извлечение работает как раньше (само прочитает и
/// само сообщит ошибку).
fn prochitat_dlya_zadaniya(task: &mut ExtractTask, config: &ExtractConfig) {
    if task.entry.inside_archive || task.entry.size > PREDEL_CHTENIYA {
        return;
    }
    let ext = Path::new(&task.entry.path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    let need = crate::extract::bytes_needed(ext, task.entry.size, task.is_sensitive, config);
    if need == 0 {
        return;
    }
    task.data = planirovshchik::prochitat_nachalo(Path::new(&task.entry.path), need).ok();
}

/// Поток-читатель одного физического диска: берёт задания его очереди, читает файл
/// в память и передаёт в общую очередь извлечения.
fn chitatel_diska(
    rx_disk: Receiver<ExtractTask>,
    tx_izvlecheniya: Sender<ExtractTask>,
    cancel: CancelToken,
    config: Arc<ExtractConfig>,
    etapy: Arc<SchetchikiEtapov>,
) {
    loop {
        if cancel.is_cancelled() {
            break;
        }
        match rx_disk.recv_timeout(SHAG_OZHIDANIYA) {
            Ok(mut task) => {
                {
                    let _izmeritel = Izmeritel::new(&etapy.chtenie_ns);
                    prochitat_dlya_zadaniya(&mut task, &config);
                }
                if let Some(data) = &task.data {
                    pribavit(&etapy.chtenie_faylov, 1);
                    pribavit(&etapy.chtenie_bayt, data.len() as u64);
                }
                if !polozhit_v_ochered(&tx_izvlecheniya, task, &cancel, &etapy.chtenie_zhdali_ns) {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

/// Поток-распределитель: берёт задания из очереди B и кладёт в очередь физического диска файла.
/// Очередь диска и её читатели создаются при первом файле этого диска.
/// По окончании B ждёт читателей и только потом закрывает очередь извлечения.
fn raspredelitel_po_diskam(
    rx_b: Receiver<ExtractTask>,
    tx_izvlecheniya: Sender<ExtractTask>,
    cancel: CancelToken,
    config: Arc<ExtractConfig>,
    etapy: Arc<SchetchikiEtapov>,
    shchadyashchiy: bool,
) {
    let opredelitel = OpredelitelDiskov::new(shchadyashchiy);
    let mut ocheredi: HashMap<KlyuchDiska, Sender<ExtractTask>> = HashMap::new();
    let mut chitateli = Vec::new();

    loop {
        if cancel.is_cancelled() {
            break;
        }
        let task = match rx_b.recv_timeout(SHAG_OZHIDANIYA) {
            Ok(task) => task,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let svedeniya = opredelitel.dlya_puti(Path::new(&task.entry.path));
        let tx_disk = ocheredi.entry(svedeniya.klyuch).or_insert_with(|| {
            let (tx, rx) = bounded::<ExtractTask>(OCHERED_DISKA);
            for _ in 0..svedeniya.potokov.max(1) {
                let rx = rx.clone();
                let tx_izv = tx_izvlecheniya.clone();
                let cancel = cancel.clone();
                let config = Arc::clone(&config);
                let etapy = Arc::clone(&etapy);
                pribavit(&etapy.chtenie_potokov, 1);
                chitateli.push(thread::spawn(move || {
                    chitatel_diska(rx, tx_izv, cancel, config, etapy)
                }));
            }
            tx
        });
        if !polozhit_v_ochered(tx_disk, task, &cancel, &etapy.raspredelitel_zhdal_ns) {
            break;
        }
    }

    // Закрываем очереди дисков, читатели доделывают оставшееся и выходят
    drop(ocheredi);
    for handle in chitateli {
        let _ = handle.join();
    }
    drop(tx_izvlecheniya);
}

/// Выполняет полный конвейер индексации для переданного списка источников.
pub fn index_sources(
    settings: &Settings,
    sources: &[Source],
) -> Result<FinishedSummary, PipelineError> {
    index_sources_with_options(settings, sources, PipelineOptions::default())
}

/// Выполняет конвейер с расширенными параметрами (ёмкость очередей, потоки, отмена).
pub fn index_sources_with_options(
    settings: &Settings,
    sources: &[Source],
    options: PipelineOptions,
) -> Result<FinishedSummary, PipelineError> {
    index_sources_with_report(settings, sources, options).map(|(summary, _)| summary)
}

/// То же, что `index_sources_with_options`, и дополнительно возвращает счётчики времени
/// этапов конвейера за прогон (где он тратит время и где стоит в ожидании).
pub fn index_sources_with_report(
    settings: &Settings,
    sources: &[Source],
    options: PipelineOptions,
) -> Result<(FinishedSummary, OtchetEtapov), PipelineError> {
    let nachalo_progona = Instant::now();
    let etapy = Arc::new(SchetchikiEtapov::default());
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let app_paths = crate::paths::resolve(&exe_dir, settings);
    app_paths.ensure_dirs()?;

    // 1. Межпроцессная блокировка базы
    let _lock = IndexLock::acquire(&app_paths.data_dir)?;

    // 2. Открытие каталога SQLite (только писатель будет владеть им)
    let mut catalog = Catalog::open_or_create(&app_paths.catalog_db)?;

    // 3. Открытие поискового индекса tantivy
    let morph = if options.morph.is_some() {
        options.morph.clone()
    } else if settings.enable_morphology {
        let morph_path = crate::paths::morph_path(&exe_dir);
        Morph::open(&morph_path).ok()
    } else {
        None
    };
    let mut index_writer = crate::index::open_or_create(&app_paths.index_dir, morph)?;

    // 4. Снимок базы данных в памяти (база НЕ читается во время сверки)
    let snapshot = catalog.snapshot()?;
    let snapshot_arc = Arc::new(snapshot);

    // Подготовка карты членов архивов: container_id -> Vec<WalkEntry>
    let mut archive_members_map: HashMap<i64, Vec<WalkEntry>> = HashMap::new();
    for (path, snap_entry) in snapshot_arc.iter() {
        if let Some(cid) = snap_entry.container_id {
            let name = path.rsplit('\\').next().unwrap_or(path).to_string();
            archive_members_map.entry(cid).or_default().push(WalkEntry {
                path: path.clone(),
                rel_path: name,
                size: snap_entry.size,
                modified_ns: snap_entry.modified_ns,
                created_ns: 0,
                is_dir: false,
                inside_archive: true,
                container_path: None,
                document_only: false,
            });
        }
    }
    let archive_members_arc = Arc::new(archive_members_map);

    // Счётчики хода по источникам: общие для потоков обхода, сверки и писателя
    let source_progress: Arc<Vec<SourceProgress>> =
        Arc::new(sources.iter().map(|_| SourceProgress::default()).collect());
    let source_roots: Vec<String> = sources
        .iter()
        .map(|s| {
            s.path
                .to_string_lossy()
                .to_string()
                .trim_end_matches('\\')
                .to_string()
        })
        .collect();
    let source_labels: Vec<SourceLabel> = sources
        .iter()
        .zip(source_roots.iter())
        .map(|(s, root)| SourceLabel {
            path: root.clone(),
            label: source_label(&s.path),
        })
        .collect();

    let cancel_token = options.cancel_token.unwrap_or_default();
    let mut progress_reporter = ProgressReporter::new(
        options.progress_tx,
        Arc::clone(&source_progress),
        source_labels,
    );

    let run_id = catalog.record_run("pipeline", "running", None)?;

    // Источники и тома регистрируются до обхода: каждая запись файла получает свой source_id
    // и volume_id, а статус источника (`QueueCMsg::SourceStatus`) находит запись по пути (№221)
    let mut registered_sources: Vec<(i64, Option<i64>)> = Vec::with_capacity(sources.len());
    for src in sources {
        let (serial, label) = crate::walk::get_volume_info(&src.path);
        registered_sources.push(crate::sync::register_source(
            &mut catalog,
            src,
            serial,
            label.as_deref(),
            "indexing",
        )?);
    }
    let registered_sources = Arc::new(registered_sources);

    // 5. Создание ограниченных каналов (crossbeam-channel)
    let cap = options.queue_capacity.max(1);
    let (tx_a, rx_a) = bounded::<QueueAMsg>(cap);
    let (tx_b, rx_b) = bounded::<ExtractTask>(cap);
    let (tx_c, rx_c) = bounded::<QueueCMsg>(cap);

    // 6. Запуск потоков обхода (по одному на том/источник)
    etapy
        .obhod_potokov
        .store(sources.len() as u64, Ordering::Relaxed);
    let mut walk_threads = Vec::new();
    for (source_idx, src) in sources.iter().enumerate() {
        let src_clone = src.clone();
        let settings_clone = settings.clone();
        let tx_a_clone = tx_a.clone();
        let cancel_clone = cancel_token.clone();
        let snapshot_ref = Arc::clone(&snapshot_arc);
        let archive_members_ref = Arc::clone(&archive_members_arc);
        let counters = Arc::clone(&source_progress);
        let etapy_obhod = Arc::clone(&etapy);

        let handle = thread::spawn(move || {
            let source_path_str = src_clone.path.to_string_lossy().to_string();

            // Замыкание для проверки неизменности архивов
            let checker =
                |archive_path: &str, size: u64, modified_ns: i64| -> Option<Vec<WalkEntry>> {
                    if let Some(existing) = snapshot_ref.get(archive_path)
                        && !existing.has_error
                        && existing.size == size
                        && existing.modified_ns == modified_ns
                    {
                        return Some(
                            archive_members_ref
                                .get(&existing.id)
                                .cloned()
                                .unwrap_or_default(),
                        );
                    }
                    None
                };

            let live_found = &counters[source_idx].found;
            let nachalo_obhoda = Instant::now();
            let walk_res = walk_source_counting(
                &src_clone,
                &settings_clone,
                Some(&checker),
                Some(live_found),
            );
            dobavit(&etapy_obhod.obhod_ns, nachalo_obhoda.elapsed());
            pribavit(&etapy_obhod.obhod_faylov, walk_res.entries().len() as u64);
            // Точное число найденных файлов: список для обработки составлен
            counters[source_idx]
                .found
                .store(walk_res.entries().len(), Ordering::Relaxed);
            counters[source_idx]
                .list_done
                .store(true, Ordering::Relaxed);

            // Отправляем пропуски
            for skip in walk_res.skips() {
                if otpravit_s_uchetom(
                    &tx_a_clone,
                    QueueAMsg::Skip(skip.clone()),
                    &etapy_obhod.obhod_zhdal_a_ns,
                )
                .is_err()
                {
                    return;
                }
            }

            // Отправляем элементы обхода
            for entry in walk_res.entries() {
                if cancel_clone.is_cancelled() {
                    break;
                }
                if otpravit_s_uchetom(
                    &tx_a_clone,
                    QueueAMsg::Entry(entry.clone()),
                    &etapy_obhod.obhod_zhdal_a_ns,
                )
                .is_err()
                {
                    return;
                }
            }

            // Сигнализируем о завершении обхода источника
            let _ = tx_a_clone.send(QueueAMsg::Finished {
                source_path: source_path_str,
                is_available: walk_res.is_available,
                protected_prefixes: walk_res.protected_prefixes,
            });
        });
        walk_threads.push(handle);
    }
    // Дропаем исходный tx_a, чтобы rx_a закрылся после завершения всех walk_threads
    drop(tx_a);

    // 7. Запуск потока сверки (читает A, сверяет со снимком, пишет в B и C)
    let sync_thread = {
        let snapshot_ref = Arc::clone(&snapshot_arc);
        let archive_members_ref = Arc::clone(&archive_members_arc);
        let cancel_clone = cancel_token.clone();
        let sources_list = sources.to_vec();
        let tx_c_sync = tx_c.clone();
        let settings_sync = settings.clone();
        let counters_sync = Arc::clone(&source_progress);
        let roots_sync = source_roots.clone();
        let registered_sync = Arc::clone(&registered_sources);
        let etapy_sverka = Arc::clone(&etapy);

        thread::spawn(move || {
            let send_c = |msg: QueueCMsg| {
                let _ = otpravit_s_uchetom(&tx_c_sync, msg, &etapy_sverka.sverka_zhdala_c_ns);
            };
            let mut seen_paths: HashSet<String> = HashSet::new();
            let mut protected_prefixes: Vec<String> = Vec::new();
            let mut available_sources: Vec<String> = Vec::new();
            let mut changed_archive_container_ids: HashSet<i64> = HashSet::new();
            // Источник и том файла: источник с самым длинным совпадающим путём (№221)
            let ids_of = |path: &str| -> (Option<i64>, Option<i64>) {
                match source_index_for_path(path, &roots_sync) {
                    Some(idx) => (Some(registered_sync[idx].0), registered_sync[idx].1),
                    None => (None, None),
                }
            };

            while let Ok(msg) = poluchit_s_uchetom(&rx_a, &etapy_sverka.sverka_zhdala_a_ns) {
                let _izmeritel = Izmeritel::new(&etapy_sverka.sverka_ns);
                if cancel_clone.is_cancelled() {
                    break;
                }
                match msg {
                    QueueAMsg::Skip(skip) => {
                        send_c(QueueCMsg::SkipRecord(skip));
                    }
                    QueueAMsg::Finished {
                        source_path,
                        is_available,
                        protected_prefixes: p_prefixes,
                    } => {
                        if is_available {
                            available_sources.push(source_path.clone());
                            protected_prefixes.extend(p_prefixes);
                            send_c(QueueCMsg::SourceStatus {
                                source_path,
                                status: "ready".to_string(),
                                details: None,
                            });
                        } else {
                            send_c(QueueCMsg::SourceStatus {
                                source_path,
                                status: "недоступен".to_string(),
                                details: Some("Корень источника недоступен".to_string()),
                            });
                        }
                    }
                    QueueAMsg::Entry(entry) => {
                        pribavit(&etapy_sverka.sverka_faylov, 1);
                        seen_paths.insert(entry.path.clone());
                        let decision = classify_entry(&entry, &snapshot_ref);

                        match decision {
                            SyncDecision::Unchanged { .. } => {
                                // Файл не изменился — в B не идёт, но в полосе хода считается решённым
                                if let Some(idx) = source_index_for_path(&entry.path, &roots_sync) {
                                    counters_sync[idx].done.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            SyncDecision::New
                            | SyncDecision::Changed { .. }
                            | SyncDecision::StaleExtraction { .. } => {
                                let (is_new, existing_id) = match decision {
                                    SyncDecision::New => (true, None),
                                    SyncDecision::Changed { id }
                                    | SyncDecision::StaleExtraction { id } => (false, Some(id)),
                                    _ => (false, None),
                                };

                                // Если изменился архив-контейнер
                                if entry.path.ends_with(".zip") && !entry.inside_archive {
                                    if let Some(cid) = existing_id {
                                        changed_archive_container_ids.insert(cid);
                                    }
                                    let (source_db_id, volume_id) = ids_of(&entry.path);
                                    let rec = build_file_record(
                                        &entry,
                                        run_id,
                                        source_db_id,
                                        volume_id,
                                        &settings_sync.book_folder_words,
                                        existing_id,
                                    );
                                    send_c(QueueCMsg::DirectRecord {
                                        record: rec,
                                        is_new,
                                    });
                                    continue;
                                }

                                // Если источник в режиме «только имена» — не извлекаем текст (R1-А-50)
                                let is_names_only = sources_list.iter().any(|s| {
                                    s.mode == SourceMode::NamesOnly
                                        && entry
                                            .path
                                            .starts_with(&s.path.to_string_lossy().to_string())
                                });
                                if is_names_only {
                                    let (source_db_id, volume_id) = ids_of(&entry.path);
                                    let mut rec = build_file_record(
                                        &entry,
                                        run_id,
                                        source_db_id,
                                        volume_id,
                                        &settings_sync.book_folder_words,
                                        existing_id,
                                    );
                                    rec.content_status = Some("metadata-only".to_string());
                                    send_c(QueueCMsg::DirectRecord {
                                        record: rec,
                                        is_new,
                                    });
                                    continue;
                                }

                                // Облачные файлы с отложенной загрузкой — содержимое не читается (Т9б)
                                #[cfg(windows)]
                                let is_cloud_file = {
                                    use std::os::windows::fs::MetadataExt;
                                    std::fs::metadata(&entry.path)
                                        .map(|m| {
                                            crate::walk::is_cloud_file_attributes(
                                                m.file_attributes(),
                                            )
                                        })
                                        .unwrap_or(false)
                                };
                                #[cfg(not(windows))]
                                let is_cloud_file = false;

                                if is_cloud_file {
                                    let (source_db_id, volume_id) = ids_of(&entry.path);
                                    let mut rec = build_file_record(
                                        &entry,
                                        run_id,
                                        source_db_id,
                                        volume_id,
                                        &settings_sync.book_folder_words,
                                        existing_id,
                                    );
                                    rec.content_status = Some("cloud-not-downloaded".to_string());
                                    send_c(QueueCMsg::DirectRecord {
                                        record: rec,
                                        is_new,
                                    });
                                    continue;
                                }

                                // Чувствительные данные (R1-А-114, 115, 127)
                                let sensitive = is_sensitive_path(
                                    &entry.path,
                                    &settings_sync.secret_extra_patterns,
                                );

                                // Прежний текст файла: при сбое разбора он остаётся (№222)
                                let previous_text_sha = existing_id.and_then(|_| {
                                    snapshot_ref
                                        .get(&entry.path)
                                        .and_then(|snap| snap.text_sha.clone())
                                });
                                let (source_db_id, volume_id) = ids_of(&entry.path);
                                let task = ExtractTask {
                                    entry,
                                    is_new,
                                    existing_id,
                                    previous_text_sha,
                                    is_sensitive: sensitive,
                                    source_db_id,
                                    volume_id,
                                    data: None,
                                };
                                let _ = otpravit_s_uchetom(
                                    &tx_b,
                                    task,
                                    &etapy_sverka.sverka_zhdala_b_ns,
                                );
                            }
                        }
                    }
                }
            }

            // После завершения обхода формируем удаление отсутствующих файлов (R1-А-82)
            if !cancel_clone.is_cancelled() {
                let mut ids_to_delete = Vec::new();

                // 1. Члены изменённых архивов подлежат замене (R1-А-78)
                for cid in &changed_archive_container_ids {
                    if let Some(members) = archive_members_ref.get(cid) {
                        for m in members {
                            // Член, который в архиве остался, записывается заново на месте
                            // (по тому же пути); удалять его после записи нельзя
                            if seen_paths.contains(&m.path) {
                                continue;
                            }
                            if let Some(snap_entry) = snapshot_ref.get(&m.path) {
                                ids_to_delete.push((snap_entry.id, m.path.clone()));
                            }
                        }
                    }
                }

                // 2. Отсутствующие файлы доступных источников
                for src in &sources_list {
                    let src_path_str = src.path.to_string_lossy().to_string();
                    if !available_sources.contains(&src_path_str) {
                        continue; // Недоступный источник защищён от удаления (R1-А-75, 76; Е-3)
                    }

                    let is_dir = src.path.is_dir();
                    let prefix_slash = format!("{}\\", src_path_str.trim_end_matches('\\'));

                    for (snap_path, snap_entry) in snapshot_ref.iter() {
                        let belongs = if is_dir {
                            snap_path == &src_path_str || snap_path.starts_with(&prefix_slash)
                        } else {
                            snap_path == &src_path_str
                        };

                        if !belongs {
                            continue;
                        }

                        if !seen_paths.contains(snap_path) {
                            let is_protected =
                                protected_prefixes.iter().any(|p| snap_path.starts_with(p));
                            if !is_protected {
                                ids_to_delete.push((snap_entry.id, snap_path.clone()));
                            }
                        }
                    }
                }

                if !ids_to_delete.is_empty() {
                    send_c(QueueCMsg::DeleteIds(ids_to_delete));
                }
            }

            // Дропаем каналы tx_b и tx_c_sync в конце потока сверки
            drop(tx_b);
            drop(tx_c_sync);
        })
    };

    // 8. Пул извлечения текста (читает B -> отправляет результат в C)
    let num_workers = if options.num_workers > 0 {
        options.num_workers
    } else if settings.worker_threads > 0 {
        settings.worker_threads
    } else {
        std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .min(4)
    };

    let worker_pool = Arc::new(WorkerPool::new(
        None,
        num_workers,
        Duration::from_secs(settings.extract_timeout_seconds),
        options.in_process_workers,
        ExtractConfig::from(settings),
    ));

    // 8а. Читатели по физическим дискам: между очередью B и пулом извлечения.
    // Без них (`po_diskam == false`) пул берёт задания прямо из B и читает файлы сам.
    let mut raspredelitel_thread = None;
    let rx_izvlecheniya = if options.po_diskam {
        let (tx_izv, rx_izv) = bounded::<ExtractTask>(2 * num_workers);
        let rx_b_dist = rx_b.clone();
        let cancel_dist = cancel_token.clone();
        let config_dist = Arc::new(worker_pool.config().clone());
        let etapy_dist = Arc::clone(&etapy);
        let shchadyashchiy = settings.hdd_gentle_mode;
        raspredelitel_thread = Some(thread::spawn(move || {
            raspredelitel_po_diskam(
                rx_b_dist,
                tx_izv,
                cancel_dist,
                config_dist,
                etapy_dist,
                shchadyashchiy,
            )
        }));
        rx_izv
    } else {
        rx_b.clone()
    };
    // B читают только распределитель или пул. Наш экземпляр не нужен, а пока он жив,
    // сверка при отмене не заметила бы, что читателей больше нет, и могла бы зависнуть.
    drop(rx_b);

    etapy
        .razbor_potokov
        .store(num_workers as u64, Ordering::Relaxed);
    let mut worker_threads = Vec::with_capacity(num_workers);
    for _ in 0..num_workers {
        let etapy_razbor = Arc::clone(&etapy);
        let rx_work = rx_izvlecheniya.clone();
        let tx_c_clone = tx_c.clone();
        let cancel_clone = cancel_token.clone();
        let pool = Arc::clone(&worker_pool);
        let test_hook_clone = options.test_hook.clone();

        let handle = thread::spawn(move || {
            loop {
                if cancel_clone.is_cancelled() {
                    break;
                }
                let mut task = match poluchit_s_tajmautom(
                    &rx_work,
                    SHAG_OZHIDANIYA,
                    &etapy_razbor.razbor_zhdal_zadaniy_ns,
                ) {
                    Ok(task) => task,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };

                let mut w_task = WorkerTask {
                    id: task.existing_id.unwrap_or(0) as u64,
                    path: if task.entry.inside_archive {
                        None
                    } else {
                        Some(PathBuf::from(&task.entry.path))
                    },
                    // Содержимое, уже прочитанное читателем диска: рабочий файл не перечитывает
                    data: task.data.take(),
                    extension: Path::new(&task.entry.path)
                        .extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    size: task.entry.size,
                    sensitive: task.is_sensitive,
                    config: pool.config().clone(),
                    test_hook: test_hook_clone.clone(),
                };

                let nachalo_arhiva = task.entry.inside_archive.then(Instant::now);
                // Ошибка чтения члена архива — результат разбора «не удалось» с настоящей причиной;
                // задание рабочему процессу в этом случае не отправляется.
                let mut oshibka_chteniya: Option<String> = None;
                if task.entry.inside_archive {
                    match task.entry.path.split_once(ARCHIVE_SEPARATOR) {
                        Some((archive_path, member_name)) => {
                            match read_archive_member_bytes(archive_path, member_name) {
                                Ok(bytes) => w_task.data = Some(bytes),
                                Err(err) => {
                                    oshibka_chteniya =
                                        Some(format!("Файл из архива не прочитан: {err}"));
                                }
                            }
                        }
                        None => {
                            oshibka_chteniya = Some(
                                "Файл из архива не прочитан: в пути нет имени члена архива"
                                    .to_string(),
                            );
                        }
                    }
                }
                if let Some(nachalo) = nachalo_arhiva {
                    dobavit(&etapy_razbor.razbor_arhiv_ns, nachalo.elapsed());
                }

                // Расширение (у члена архива — его собственное) и размер — до отправки задания.
                let rasshirenie_uchet = w_task.extension.clone();
                let razmer_uchet = w_task.size;
                let nachalo_razbora = Instant::now();
                let extracted = match oshibka_chteniya {
                    Some(prichina) => Extracted::failed(prichina),
                    None => {
                        let _izmeritel = Izmeritel::new(&etapy_razbor.razbor_ns);
                        pool.submit_task(w_task)
                    }
                };
                etapy_razbor.formaty.zapisat(
                    &rasshirenie_uchet,
                    razmer_uchet,
                    nachalo_razbora.elapsed().as_nanos() as u64,
                    &task.entry.path,
                    extracted.status == "failed" || extracted.error.is_some(),
                );
                pribavit(&etapy_razbor.razbor_faylov, 1);

                if otpravit_s_uchetom(
                    &tx_c_clone,
                    QueueCMsg::ExtractedResult { task, extracted },
                    &etapy_razbor.razbor_zhdal_c_ns,
                )
                .is_err()
                {
                    break;
                }
            }
        });
        worker_threads.push(handle);
    }
    drop(rx_izvlecheniya);
    // Дропаем исходный tx_c здесь: теперь только сверка и рабочие владеют своими клонами tx_c
    drop(tx_c);

    // 9. ОДИН писатель (читает C, пишет в базу и индекс)
    let mut walked_count = 0usize;
    let mut extracted_count = 0usize;
    let mut written_count = 0usize;
    let mut added_files = 0usize;
    let mut changed_files = 0usize;
    let mut deleted_files = 0usize;

    let mut records_buffer: Vec<(FileRecord, Option<String>)> = Vec::with_capacity(500);
    let mut files_to_delete_buffer: Vec<i64> = Vec::new();
    let mut last_tx_time = Instant::now();
    let mut last_index_commit = Instant::now();
    let mut container_id_map: HashMap<String, i64> = HashMap::new();

    let flush_batch = |catalog: &mut Catalog,
                       index_writer: &mut IndexWriterHandle,
                       records: &mut Vec<(FileRecord, Option<String>)>,
                       deletes: &mut Vec<i64>,
                       container_ids: &mut HashMap<String, i64>|
     -> Result<(), PipelineError> {
        // 1. Выполняем удаления
        if !deletes.is_empty() {
            let _izmeritel = Izmeritel::new(&etapy.pisatel_baza_ns);
            catalog.delete_files(deletes)?;
            deletes.clear();
        }

        // 2. Вставляем/обновляем файлы
        if !records.is_empty() {
            // Обычные файлы и архивы пишутся раньше членов архивов: у члена, попавшего в одну
            // порцию со своим архивом, container_id находится сразу, а не остаётся пустым
            // (иначе при следующей сверке неизменного архива его члены теряются).
            let (ordinary, members): (Vec<usize>, Vec<usize>) =
                (0..records.len()).partition(|&i| !records[i].0.inside_archive);

            for group in [ordinary, members] {
                if group.is_empty() {
                    continue;
                }
                let izmeritel_bazy = Izmeritel::new(&etapy.pisatel_baza_ns);
                // Разрешаем container_id для членов архива
                for &i in &group {
                    let rec = &mut records[i].0;
                    if rec.inside_archive
                        && rec.container_id.is_none()
                        && let Some((archive_path, _)) = rec.path.split_once(ARCHIVE_SEPARATOR)
                    {
                        if let Some(&cid) = container_ids.get(archive_path) {
                            rec.container_id = Some(cid);
                        } else if let Some(existing_id) = catalog.file_id_by_path(archive_path)? {
                            rec.container_id = Some(existing_id);
                            container_ids.insert(archive_path.to_string(), existing_id);
                        }
                    }
                }

                let file_records: Vec<FileRecord> =
                    group.iter().map(|&i| records[i].0.clone()).collect();
                // Файлы и их тексты пишутся в SQLite одной транзакцией; индекс tantivy — после неё
                let texts: Vec<Option<&str>> =
                    group.iter().map(|&i| records[i].1.as_deref()).collect();
                let (ids, shas) = catalog.write_batch(&file_records, &texts)?;

                // Запоминаем container_id для добавленных архивов
                for (k, &i) in group.iter().enumerate() {
                    let rec = &records[i].0;
                    if !rec.inside_archive && rec.path.ends_with(".zip") {
                        container_ids.insert(rec.path.clone(), ids[k]);
                    }
                }

                drop(izmeritel_bazy);

                // 3. Добавляем новые тексты в tantivy
                let text_batch: Vec<&str> = group
                    .iter()
                    .filter_map(|&i| records[i].1.as_deref())
                    .collect();

                if !text_batch.is_empty() {
                    let _izmeritel = Izmeritel::new(&etapy.pisatel_indeks_dobavlenie_ns);
                    for (idx, &text) in text_batch.iter().enumerate() {
                        let (sha, is_new) = &shas[idx];
                        if sha.is_empty() {
                            continue;
                        }
                        // Второй экземпляр одинакового текста (та же sha) не индексируется повторно (R1-Б-64).
                        // `is_new` истинно тогда и только тогда, когда запись добавлена в таблицу texts впервые.
                        if *is_new {
                            index_writer.add(sha, text)?;
                            pribavit(&etapy.pisatel_dokumentov, 1);
                        }
                    }
                }
            }

            records.clear();
        }

        // 4. Отрабатываем очередь удалений из индекса index_outbox
        let outbox = {
            let _izmeritel = Izmeritel::new(&etapy.pisatel_baza_ns);
            catalog.drain_index_outbox()?
        };
        for (sha, op) in outbox {
            if op == "delete" {
                let _izmeritel = Izmeritel::new(&etapy.pisatel_indeks_udalenie_ns);
                index_writer.delete(&sha)?;
            }
        }

        Ok(())
    };

    loop {
        match poluchit_s_tajmautom(&rx_c, Duration::from_millis(100), &etapy.pisatel_zhdal_ns) {
            Ok(msg) => match msg {
                QueueCMsg::SourceStatus {
                    source_path,
                    status,
                    details,
                } => {
                    let _izmeritel = Izmeritel::new(&etapy.pisatel_baza_ns);
                    if let Ok(Some(src_rec)) = catalog.get_source_by_path(&source_path)
                        && let Some(sid) = src_rec.id
                    {
                        let _ = catalog.update_source_status(sid, &status, details.as_deref());
                    }
                }
                QueueCMsg::SkipRecord(skip) => {
                    let _izmeritel = Izmeritel::new(&etapy.pisatel_baza_ns);
                    let _ = catalog.add_skipped(run_id, &skip.path, &skip.reason);
                }
                QueueCMsg::DeleteIds(ids) => {
                    let _izmeritel = Izmeritel::new(&etapy.pisatel_podgotovka_ns);
                    deleted_files += ids.len();
                    for (id, path) in ids {
                        if let Some(idx) = source_index_for_path(&path, &source_roots) {
                            source_progress[idx].done.fetch_add(1, Ordering::Relaxed);
                        }
                        files_to_delete_buffer.push(id);
                    }
                }
                QueueCMsg::DirectRecord { record, is_new } => {
                    let _izmeritel = Izmeritel::new(&etapy.pisatel_podgotovka_ns);
                    walked_count += 1;
                    written_count += 1;
                    pribavit(&etapy.pisatel_zapisey, 1);
                    if is_new {
                        added_files += 1;
                    } else {
                        changed_files += 1;
                    }
                    if let Some(idx) = source_index_for_path(&record.path, &source_roots) {
                        source_progress[idx].done.fetch_add(1, Ordering::Relaxed);
                    }
                    progress_reporter.set_current_dir(&record.path);
                    records_buffer.push((record, None));
                }
                QueueCMsg::ExtractedResult { task, extracted } => {
                    let _izmeritel = Izmeritel::new(&etapy.pisatel_podgotovka_ns);
                    walked_count += 1;
                    extracted_count += 1;
                    written_count += 1;
                    pribavit(&etapy.pisatel_zapisey, 1);
                    if task.is_new {
                        added_files += 1;
                    } else {
                        changed_files += 1;
                    }
                    if let Some(idx) = source_index_for_path(&task.entry.path, &source_roots) {
                        source_progress[idx].done.fetch_add(1, Ordering::Relaxed);
                        if !extracted.text.is_empty() {
                            source_progress[idx]
                                .bytes_read
                                .fetch_add(task.entry.size, Ordering::Relaxed);
                        }
                    }
                    progress_reporter.set_current_dir(&task.entry.path);

                    // Повторная проверка размера и времени файла перед записью результата:
                    // изменился во время извлечения -> результат не пишется, файл остаётся pending
                    let mut file_changed_during_extraction = false;
                    let mut file_disappeared = false;

                    if !task.entry.inside_archive {
                        match fs::metadata(&task.entry.path) {
                            Ok(m) => {
                                let current_mtime = m
                                    .modified()
                                    .ok()
                                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                    .map(|d| d.as_nanos() as i64)
                                    .unwrap_or(0);
                                if m.len() != task.entry.size
                                    || current_mtime != task.entry.modified_ns
                                {
                                    file_changed_during_extraction = true;
                                }
                            }
                            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                                file_disappeared = true;
                            }
                            Err(_) => {}
                        }
                    }

                    if file_disappeared {
                        // Файл исчез между обходом и извлечением -> удаляется без ошибки (R1-Б-44)
                        if let Some(id) = task.existing_id {
                            files_to_delete_buffer.push(id);
                        }
                        continue;
                    }

                    let mut rec = build_file_record(
                        &task.entry,
                        run_id,
                        task.source_db_id,
                        task.volume_id,
                        &settings.book_folder_words,
                        task.existing_id,
                    );

                    if file_changed_during_extraction {
                        rec.content_status = Some("pending".to_string());
                        rec.error = Some(
                            "Файл изменился во время извлечения; прежний текст сохранён"
                                .to_string(),
                        );
                        rec.text_sha = task.previous_text_sha;
                        records_buffer.push((rec, None));
                    } else if extracted.error.is_some() || extracted.status == "failed" {
                        // Ошибка извлечения: прежний текст файла сохраняется (R1-А-129, 130)
                        rec.content_status = Some("failed".to_string());
                        rec.error = extracted.error;
                        rec.text_sha = task.previous_text_sha;
                        records_buffer.push((rec, None));
                    } else {
                        rec.content_status = Some(extracted.status);
                        rec.extraction_version = EXTRACTION_VERSION;
                        rec.error = None;
                        let text_opt = if extracted.text.is_empty() {
                            None
                        } else {
                            Some(extracted.text)
                        };
                        records_buffer.push((rec, text_opt));
                    }
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Проверяем условия сброса порции в базу: ≤ 500 записей или ≤ 1 с
        if records_buffer.len() >= 500
            || files_to_delete_buffer.len() >= 500
            || (last_tx_time.elapsed() >= Duration::from_secs(1)
                && (!records_buffer.is_empty() || !files_to_delete_buffer.is_empty()))
        {
            flush_batch(
                &mut catalog,
                &mut index_writer,
                &mut records_buffer,
                &mut files_to_delete_buffer,
                &mut container_id_map,
            )?;
            last_tx_time = Instant::now();

            // Коммит индекса не чаще 1 раза в 2 с
            if last_index_commit.elapsed() >= Duration::from_secs(2) {
                fiksirovat_indeks(&mut index_writer, &etapy)?;
                last_index_commit = Instant::now();
            }
        }

        // Полоса хода: этап и скорость, не чаще 10 раз в секунду
        let all_lists_done = source_progress
            .iter()
            .all(|p| p.list_done.load(Ordering::Relaxed));
        let phase = if all_lists_done {
            "обработка"
        } else {
            "составляю список"
        };
        progress_reporter.update(walked_count, extracted_count, written_count);
        progress_reporter.maybe_send(phase, false);
    }

    // Сбрасываем остаток порций в конце
    flush_batch(
        &mut catalog,
        &mut index_writer,
        &mut records_buffer,
        &mut files_to_delete_buffer,
        &mut container_id_map,
    )?;

    // Обязательный финальный коммит индекса
    fiksirovat_indeks(&mut index_writer, &etapy)?;

    // Неизменённые файлы, записанные раньше без источника и тома, получают их одной транзакцией (№221)
    if !cancel_token.is_cancelled() {
        let privyazka: Vec<(i64, Option<i64>, String)> = sources
            .iter()
            .zip(registered_sources.iter())
            .map(|(src, &(source_id, volume_id))| {
                (source_id, volume_id, src.path.to_string_lossy().to_string())
            })
            .collect();
        let _izmeritel = Izmeritel::new(&etapy.pisatel_baza_ns);
        catalog.assign_sources_to_files(&privyazka)?;
    }

    // Ожидаем завершения всех потоков конвейера
    for handle in walk_threads {
        let _ = handle.join();
    }
    let _ = sync_thread.join();
    if let Some(handle) = raspredelitel_thread {
        let _ = handle.join();
    }
    for handle in worker_threads {
        let _ = handle.join();
    }

    let summary = FinishedSummary {
        added: added_files,
        changed: changed_files,
        deleted: deleted_files,
    };

    let status = if cancel_token.is_cancelled() {
        "cancelled"
    } else {
        "completed"
    };

    catalog.finish_run(
        run_id,
        status,
        Some(&format!(
            "added: {}, changed: {}, deleted: {}",
            summary.added, summary.changed, summary.deleted
        )),
    )?;

    progress_reporter.update(walked_count, extracted_count, written_count);
    progress_reporter.send_finished(summary.clone());

    // Счётчики обмена с внешними рабочими процессами (при разборе в процессе — нули)
    let obmen = worker_pool.stats();
    let zadach = obmen.tasks.load(Ordering::Relaxed);
    etapy.protsess_zadach.store(zadach, Ordering::Relaxed);
    etapy.protsess_serializ_ns.store(
        obmen.serialize_ns.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    etapy
        .protsess_zapis_ns
        .store(obmen.write_ns.load(Ordering::Relaxed), Ordering::Relaxed);
    etapy.protsess_otvet_ns.store(
        obmen.reply_wait_ns.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    etapy.protsess_razbor_otveta_ns.store(
        obmen.parse_reply_ns.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    etapy.protsess_rabochiy_izvlechenie_ns.store(
        obmen.child_extract_ns.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    etapy.protsess_rabochiy_dekodirovanie_ns.store(
        obmen.child_decode_ns.load(Ordering::Relaxed),
        Ordering::Relaxed,
    );
    etapy
        .protsess_bayt
        .store(obmen.json_bytes.load(Ordering::Relaxed), Ordering::Relaxed);

    let otchet = etapy.snimok(nachalo_progona.elapsed().as_nanos() as u64);
    Ok((summary, otchet))
}

/// Фиксирует индекс tantivy и учитывает время фиксации.
fn fiksirovat_indeks(
    index_writer: &mut IndexWriterHandle,
    etapy: &SchetchikiEtapov,
) -> Result<(), PipelineError> {
    let _izmeritel = Izmeritel::new(&etapy.pisatel_fiksaciya_ns);
    index_writer.commit()?;
    pribavit(&etapy.pisatel_fiksaciy, 1);
    Ok(())
}

/// Выполняет синхронизацию всех активных источников из настроек `Settings`.
pub fn sync_all(settings: &Settings) -> Result<FinishedSummary, PipelineError> {
    sync_all_with_options(settings, PipelineOptions::default())
}

/// Выполняет синхронизацию всех активных источников с параметрами.
pub fn sync_all_with_options(
    settings: &Settings,
    options: PipelineOptions,
) -> Result<FinishedSummary, PipelineError> {
    let enabled_sources: Vec<Source> = settings
        .sources
        .iter()
        .filter(|s| s.enabled)
        .cloned()
        .collect();

    index_sources_with_options(settings, &enabled_sources, options)
}

/// Добавляет выбранные пути в поиск (явный выбор — любые форматы, R1-А-66, R1-Б-11).
pub fn add_paths(paths: &[PathBuf]) -> Result<FinishedSummary, PipelineError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let settings_file = crate::paths::default_settings_path(&exe_dir);
    let settings = Settings::load(&settings_file).unwrap_or_default();
    add_paths_with_settings(&settings, paths, PipelineOptions::default())
}

/// Добавляет выбранные пути в поиск с явными настройками и параметрами.
pub fn add_paths_with_settings(
    settings: &Settings,
    paths: &[PathBuf],
    options: PipelineOptions,
) -> Result<FinishedSummary, PipelineError> {
    let sources: Vec<Source> = paths
        .iter()
        .map(|p| Source {
            path: p.clone(),
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        })
        .collect();

    index_sources_with_options(settings, &sources, options)
}

/// Удаляет выбранные пути из базы и индекса (файлы на диске не затрагиваются, R1-А-89..91).
pub fn remove_paths(paths: &[PathBuf]) -> Result<FinishedSummary, PipelineError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let settings_file = crate::paths::default_settings_path(&exe_dir);
    let settings = Settings::load(&settings_file).unwrap_or_default();
    remove_paths_with_settings(&settings, paths)
}

/// Удаляет выбранные пути с явными настройками.
pub fn remove_paths_with_settings(
    settings: &Settings,
    paths: &[PathBuf],
) -> Result<FinishedSummary, PipelineError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let app_paths = crate::paths::resolve(&exe_dir, settings);
    app_paths.ensure_dirs()?;

    let _lock = IndexLock::acquire(&app_paths.data_dir)?;
    let mut catalog = Catalog::open_or_create(&app_paths.catalog_db)?;

    let morph = if settings.enable_morphology {
        let morph_path = crate::paths::morph_path(&exe_dir);
        Morph::open(&morph_path).ok()
    } else {
        None
    };
    let mut index_writer = crate::index::open_or_create(&app_paths.index_dir, morph)?;

    let mut total_deleted = 0;
    for path in paths {
        let path_str = path.to_string_lossy();
        let deleted = crate::sync::remove_source(&mut catalog, &path_str)?;
        total_deleted += deleted;
    }

    // Очищаем index_outbox
    let outbox = catalog.drain_index_outbox()?;
    for (sha, op) in outbox {
        if op == "delete" {
            index_writer.delete(&sha)?;
        }
    }
    index_writer.commit()?;

    Ok(FinishedSummary {
        added: 0,
        changed: 0,
        deleted: total_deleted,
    })
}

/// Перестраивает поисковый индекс исключительно из сохранённых текстов в базе (R1-А-93, R1-Б-40).
/// Исходные файлы на диске НЕ читаются.
pub fn reindex_from_saved_texts() -> Result<FinishedSummary, PipelineError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let settings_file = crate::paths::default_settings_path(&exe_dir);
    let settings = Settings::load(&settings_file).unwrap_or_default();
    reindex_from_saved_texts_with_settings(&settings, PipelineOptions::default())
}

/// Перестраивает поисковый индекс из сохранённых текстов с заданными настройками и параметрами.
pub fn reindex_from_saved_texts_with_settings(
    settings: &Settings,
    options: PipelineOptions,
) -> Result<FinishedSummary, PipelineError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let app_paths = crate::paths::resolve(&exe_dir, settings);
    app_paths.ensure_dirs()?;

    let _lock = IndexLock::acquire(&app_paths.data_dir)?;
    let catalog = Catalog::open_or_create(&app_paths.catalog_db)?;

    let morph = if settings.enable_morphology {
        let morph_path = crate::paths::morph_path(&exe_dir);
        Morph::open(&morph_path).ok()
    } else {
        None
    };
    let mut index_writer = crate::index::open_or_create(&app_paths.index_dir, morph)?;

    // Очищаем индекс
    index_writer.delete_all()?;

    // Читаем все тексты из SQLite (исходные файлы не читаются)
    let texts = catalog.list_all_texts()?;
    let mut progress_reporter =
        ProgressReporter::new(options.progress_tx, Arc::new(Vec::new()), Vec::new());
    let mut count = 0;

    for (sha, text) in &texts {
        index_writer.add(sha, text)?;
        count += 1;
        if count % 250 == 0 {
            progress_reporter.update(0, count, count);
            progress_reporter.maybe_send("перестраиваю индекс", false);
        }
    }

    index_writer.commit()?;

    let summary = FinishedSummary {
        added: count,
        changed: 0,
        deleted: 0,
    };
    progress_reporter.update(0, count, count);
    progress_reporter.send_finished(summary.clone());

    Ok(summary)
}

/// Повторно извлекает тексты для файлов с устаревшей версией извлечения (R1-А-80, 132, 133).
pub fn reextract_stale() -> Result<FinishedSummary, PipelineError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let settings_file = crate::paths::default_settings_path(&exe_dir);
    let settings = Settings::load(&settings_file).unwrap_or_default();
    reextract_stale_with_settings(&settings, PipelineOptions::default())
}

/// Повторно извлекает тексты для файлов с устаревшей версией с заданными настройками.
pub fn reextract_stale_with_settings(
    settings: &Settings,
    options: PipelineOptions,
) -> Result<FinishedSummary, PipelineError> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."));
    let app_paths = crate::paths::resolve(&exe_dir, settings);
    app_paths.ensure_dirs()?;

    let _lock = IndexLock::acquire(&app_paths.data_dir)?;
    let mut catalog = Catalog::open_or_create(&app_paths.catalog_db)?;

    let morph = if settings.enable_morphology {
        let morph_path = crate::paths::morph_path(&exe_dir);
        Morph::open(&morph_path).ok()
    } else {
        None
    };
    let mut index_writer = crate::index::open_or_create(&app_paths.index_dir, morph)?;

    // Находим файлы с extraction_version < EXTRACTION_VERSION
    let mut stmt = catalog.connection().prepare(
        "SELECT id, path, extension, size, inside_archive, text_sha FROM files WHERE extraction_version < ?1 AND available = 1;",
    )?;

    struct StaleRow {
        id: i64,
        path: String,
        extension: String,
        size: u64,
        inside_archive: bool,
    }

    let mut rows = stmt.query(rusqlite::params![EXTRACTION_VERSION])?;
    let mut stale_files = Vec::new();
    while let Some(row) = rows.next()? {
        stale_files.push(StaleRow {
            id: row.get(0)?,
            path: row.get(1)?,
            extension: row.get(2)?,
            size: row.get::<_, i64>(3)? as u64,
            inside_archive: row.get::<_, i64>(4)? != 0,
        });
    }
    drop(rows);
    drop(stmt);

    let num_workers = if options.num_workers > 0 {
        options.num_workers
    } else if settings.worker_threads > 0 {
        settings.worker_threads
    } else {
        std::thread::available_parallelism()
            .map_or(2, |n| n.get())
            .min(4)
    };

    let pool = WorkerPool::new(
        None,
        num_workers,
        Duration::from_secs(settings.extract_timeout_seconds),
        options.in_process_workers,
        ExtractConfig::from(settings),
    );

    let mut updated_count = 0;
    let mut progress_reporter =
        ProgressReporter::new(options.progress_tx, Arc::new(Vec::new()), Vec::new());

    for stale in stale_files {
        // Член архива читается заранее; при ошибке чтения задание не отправляется (как в потоке разбора)
        let mut oshibka_chteniya: Option<String> = None;
        let member_data = if stale.inside_archive {
            match stale.path.split_once(ARCHIVE_SEPARATOR) {
                Some((archive_path, member_name)) => {
                    match read_archive_member_bytes(archive_path, member_name) {
                        Ok(bytes) => Some(bytes),
                        Err(err) => {
                            oshibka_chteniya = Some(format!("Файл из архива не прочитан: {err}"));
                            None
                        }
                    }
                }
                None => {
                    oshibka_chteniya = Some(
                        "Файл из архива не прочитан: в пути нет имени члена архива".to_string(),
                    );
                    None
                }
            }
        } else {
            None
        };
        let task = WorkerTask {
            id: stale.id as u64,
            path: if stale.inside_archive {
                None
            } else {
                Some(PathBuf::from(&stale.path))
            },
            data: member_data,
            extension: stale.extension.trim_start_matches('.').to_string(),
            size: stale.size,
            sensitive: false,
            config: pool.config().clone(),
            test_hook: None,
        };

        // Если исходный файл недоступен, сохраняем старый текст (R1-А-130, R1-Б-32)
        if !stale.inside_archive && !Path::new(&stale.path).exists() {
            catalog.connection().execute(
                "UPDATE files SET content_status = 'failed', error = 'Файл недоступен' WHERE id = ?1;",
                rusqlite::params![stale.id],
            )?;
            continue;
        }

        // Член архива не прочитан: задание не отправляется, причина пишется в базу, старый текст сохраняется
        if let Some(prichina) = oshibka_chteniya {
            let extracted = Extracted::failed(prichina);
            catalog.connection().execute(
                "UPDATE files SET content_status = ?1, error = ?2 WHERE id = ?3;",
                rusqlite::params![extracted.status, extracted.error, stale.id],
            )?;
            continue;
        }

        let extracted = pool.submit_task(task);
        if !extracted.text.is_empty() {
            let res = catalog.store_text_batch(&[(stale.id, extracted.text.as_str())])?;
            if let Some((sha, is_new)) = res.into_iter().next()
                && !sha.is_empty()
                && is_new
            {
                index_writer.add(&sha, &extracted.text)?;
            }
        }

        catalog.connection().execute(
            "UPDATE files SET extraction_version = ?1 WHERE id = ?2;",
            rusqlite::params![EXTRACTION_VERSION, stale.id],
        )?;

        updated_count += 1;
        progress_reporter.update(0, updated_count, updated_count);
        progress_reporter.maybe_send("обработка", false);
    }

    index_writer.commit()?;

    let summary = FinishedSummary {
        added: 0,
        changed: updated_count,
        deleted: 0,
    };
    progress_reporter.update(0, updated_count, updated_count);
    progress_reporter.send_finished(summary.clone());

    Ok(summary)
}

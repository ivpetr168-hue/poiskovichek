//! Пул рабочих процессов для изолированного извлечения текста (раздел 2.6 ТЗ).
//! Запуск того же exe с ключом `--worker`, общение через stdin/stdout,
//! Job Object с KILL_ON_JOB_CLOSE, отслеживание аварийных завершений и таймаутов.
//!
//! Задание передаётся двоично: строка JSON (заголовок, оканчивается `\n`; в нём `data_len` —
//! длина данных) и сразу за ней ровно `data_len` сырых байтов. Ответ — одна строка JSON.

use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::extract::{ExtractConfig, ExtractSource, Extracted, extract};

const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Задание на извлечение текста, передаваемое рабочему процессу.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerTask {
    pub id: u64,
    pub path: Option<PathBuf>,
    /// Готовые байты файла. Если заданы, извлекатель берёт их и файл с диска не перечитывает
    /// (даже когда `path` тоже указан). В JSON-заголовок не входят: по каналу к рабочему
    /// процессу идут сырыми байтами сразу после заголовка (см. [`write_task`], [`read_task`]).
    #[serde(skip)]
    pub data: Option<Vec<u8>>,
    pub extension: String,
    pub size: u64,
    pub sensitive: bool,
    pub config: ExtractConfig,
    /// Тестовый хук для проверки падений и зависаний (например, "hang_10s" или "abort").
    pub test_hook: Option<String>,
}

/// Предел длины данных одного задания, байт. Защита от мусора в заголовке: больше — ошибка
/// задания, а не попытка выделить память под выдуманную длину. 512 МБ — не меньше наибольшего
/// значения настройки `zip_max_file_size_mb` в окне (`range(1..=500)`, `ui\settings_dialog.rs`):
/// член архива такого размера передаётся рабочему процессу целиком.
pub const MAX_DATA_LEN: u64 = 512 * 1024 * 1024;

/// Данные до этой длины читаются в заранее выделенный буфер, длиннее — растущим буфером
/// (выдуманная длина в заголовке не должна сразу занимать сотни мегабайт).
const EXACT_ALLOC_LEN: u64 = 64 * 1024 * 1024;

/// Проверяет длину данных на предел [`MAX_DATA_LEN`].
fn check_data_len(data_len: u64) -> Result<(), String> {
    if data_len > MAX_DATA_LEN {
        return Err(format!(
            "длина данных задания {data_len} байт больше предела {MAX_DATA_LEN} байт"
        ));
    }
    Ok(())
}

/// Размер буфера записи в канал рабочего процесса. Данные не меньше буфера пишутся в канал
/// напрямую, без копирования.
const WRITE_BUF: usize = 64 * 1024;

/// Заголовок задания для записи (без копирования самого задания).
#[derive(Serialize)]
struct HeaderRef<'a> {
    task: &'a WorkerTask,
    /// Длина данных, которые идут следом за заголовком; 0 — данных нет.
    data_len: u64,
    /// Данные заданы, хотя их может быть 0 байт (пустой член архива): `Some(vec![])`.
    has_data: bool,
}

/// Заголовок задания при чтении.
#[derive(Deserialize)]
struct Header {
    task: WorkerTask,
    data_len: u64,
    #[serde(default)]
    has_data: bool,
}

/// Сериализует заголовок задания (одна строка JSON с `\n`) в `out`, прежнее содержимое
/// сбрасывается. Возвращает длину данных, которые надо записать следом.
fn serialize_header(task: &WorkerTask, out: &mut Vec<u8>) -> Result<u64, String> {
    let data_len = task.data.as_ref().map_or(0, |d| d.len() as u64);
    check_data_len(data_len)?;
    out.clear();
    let header = HeaderRef {
        task,
        data_len,
        has_data: task.data.is_some(),
    };
    serde_json::to_writer(&mut *out, &header).map_err(|e| e.to_string())?;
    out.push(b'\n');
    Ok(data_len)
}

/// Записывает заголовок и сразу за ним данные; один `flush` на задание.
fn write_frame<W: Write>(writer: &mut W, header: &[u8], data: &[u8]) -> io::Result<()> {
    writer.write_all(header)?;
    writer.write_all(data)?;
    writer.flush()
}

/// Записывает задание в двоичном формате обмена: строка JSON с длиной данных и следом
/// ровно столько сырых байтов. Данные пишутся из `task.data` как есть, без копии в общий буфер.
pub fn write_task<W: Write>(writer: &mut W, task: &WorkerTask) -> Result<(), String> {
    let mut header = Vec::new();
    serialize_header(task, &mut header)?;
    let data = task.data.as_deref().unwrap_or(&[]);
    write_frame(writer, &header, data).map_err(|e| e.to_string())
}

/// Читает одно задание из потока обмена. `Ok(None)` — поток закончился между заданиями.
/// Ошибка — мусор или оборванный поток (данных меньше, чем `data_len`) либо `data_len`
/// больше [`MAX_DATA_LEN`]. Вместе с заданием возвращает, сколько нс заняло чтение
/// (разбор заголовка и получение данных; ожидание самой строки заголовка не входит).
pub fn read_task<R: BufRead>(
    reader: &mut R,
    line: &mut String,
) -> Result<Option<(WorkerTask, u64)>, String> {
    loop {
        line.clear();
        let n = reader
            .read_line(line)
            .map_err(|e| format!("не удалось прочитать заголовок задания: {e}"))?;
        if n == 0 {
            return Ok(None);
        }
        if !line.trim().is_empty() {
            break;
        }
    }
    let start = Instant::now();
    let header: Header = serde_json::from_str(line.trim())
        .map_err(|e| format!("некорректный заголовок задания: {e}"))?;
    check_data_len(header.data_len)?;
    let mut task = header.task;
    if header.data_len > 0 || header.has_data {
        let truncated = |detail: String| {
            format!(
                "оборванный поток: ожидалось {} байт данных задания ({detail})",
                header.data_len
            )
        };
        let data = if header.data_len <= EXACT_ALLOC_LEN {
            let mut data = vec![0u8; header.data_len as usize];
            reader
                .read_exact(&mut data)
                .map_err(|e| truncated(e.to_string()))?;
            data
        } else {
            let mut data = Vec::new();
            let got = reader
                .take(header.data_len)
                .read_to_end(&mut data)
                .map_err(|e| truncated(e.to_string()))?;
            if got as u64 != header.data_len {
                return Err(truncated(format!("получено {got}")));
            }
            data
        };
        task.data = Some(data);
    }
    Ok(Some((task, start.elapsed().as_nanos() as u64)))
}

/// Ответ рабочего процесса с результатами извлечения.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerResponse {
    pub id: u64,
    pub extracted: Extracted,
    /// Сколько нс рабочий процесс извлекал текст (для замера этапов; в старых ответах нет).
    #[serde(default)]
    pub extract_ns: u64,
    /// Сколько нс рабочий процесс читал задание: разбор заголовка и получение данных.
    #[serde(default)]
    pub decode_ns: u64,
}

/// Счётчики времени обмена с внешними рабочими процессами (для замера этапов конвейера).
/// Времена в наносекундах, суммарно по всем потокам пула.
#[derive(Debug, Default)]
pub struct WorkerStats {
    /// Сколько заданий передано рабочим процессам.
    pub tasks: AtomicU64,
    /// Сериализация заголовка задания в JSON (данные идут сырыми байтами, без кодирования).
    pub serialize_ns: AtomicU64,
    /// Запись заголовка и данных задания в канал рабочего процесса.
    pub write_ns: AtomicU64,
    /// Ожидание ответа процесса (вместе с его работой).
    pub reply_wait_ns: AtomicU64,
    /// Разбор JSON-ответа в основном процессе.
    pub parse_reply_ns: AtomicU64,
    /// Работа рабочего процесса: извлечение текста.
    pub child_extract_ns: AtomicU64,
    /// Работа рабочего процесса: чтение задания (разбор заголовка и получение данных).
    pub child_decode_ns: AtomicU64,
    /// Сколько байтов передано рабочим процессам (заголовки JSON и данные).
    pub json_bytes: AtomicU64,
}

/// Обертка над Windows Job Object с флагом JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE.
pub struct JobObjectGuard {
    #[cfg(windows)]
    handle: windows::Win32::Foundation::HANDLE,
}

impl JobObjectGuard {
    pub fn new() -> Option<Self> {
        #[cfg(windows)]
        {
            use windows::Win32::System::JobObjects::{
                CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
                SetInformationJobObject,
            };
            unsafe {
                let handle = match CreateJobObjectW(None, None) {
                    Ok(h) => h,
                    Err(_) => return None,
                };
                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let set_res = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if set_res.is_err() {
                    let _ = windows::Win32::Foundation::CloseHandle(handle);
                    return None;
                }
                Some(Self { handle })
            }
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    pub fn assign_child(&self, child: &Child) {
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::JobObjects::AssignProcessToJobObject;
            let raw = child.as_raw_handle();
            let win32_handle = HANDLE(raw);
            unsafe {
                let _ = AssignProcessToJobObject(self.handle, win32_handle);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = child;
        }
    }
}

impl Drop for JobObjectGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(self.handle);
            }
        }
    }
}

// Позволяем передавать JobObjectGuard между потоками
unsafe impl Send for JobObjectGuard {}
unsafe impl Sync for JobObjectGuard {}

/// Находит путь к исполняемому файлу Poiskovichek.
pub fn find_worker_exe() -> PathBuf {
    if let Ok(exe) = std::env::var("CARGO_BIN_EXE_Poiskovichek") {
        let p = PathBuf::from(exe);
        if p.exists() {
            return p;
        }
    }
    if let Ok(current) = std::env::current_exe() {
        if current
            .file_stem()
            .is_some_and(|s| s.eq_ignore_ascii_case("poiskovichek"))
        {
            return current;
        }
        if let Some(parent) = current.parent() {
            if let Some(target_dir) = parent.parent() {
                let candidate = target_dir.join("Poiskovichek.exe");
                if candidate.exists() {
                    return candidate;
                }
            }
            let candidate = parent.join("Poiskovichek.exe");
            if candidate.exists() {
                return candidate;
            }
        }
        if current.exists() {
            return current;
        }
    }
    PathBuf::from("Poiskovichek.exe")
}

/// Цикл рабочего процесса: читает задачи из stdin, отправляет результаты в stdout.
/// Запускается при передаче ключа `--worker`.
pub fn run_worker_loop() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = stdin.lock();
    let mut writer = BufWriter::new(stdout.lock());
    serve(&mut reader, &mut writer);
}

/// Обслуживает поток заданий: читает задание, отвечает одной строкой JSON. Заканчивает на конце
/// потока. Мусор в заголовке или оборванный поток — ответ с ошибкой (id 0) и выход: после
/// такой ошибки границы заданий в потоке неизвестны, продолжать нельзя.
fn serve<R: BufRead, W: Write>(reader: &mut R, writer: &mut W) {
    let mut line = String::new();

    loop {
        let (task, decode_ns) = match read_task(reader, &mut line) {
            Ok(Some(read)) => read,
            Ok(None) => break, // EOF между заданиями
            Err(message) => {
                let resp = WorkerResponse {
                    id: 0,
                    extracted: Extracted::failed(message),
                    extract_ns: 0,
                    decode_ns: 0,
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = writeln!(writer, "{json}");
                    let _ = writer.flush();
                }
                break;
            }
        };

        // Тестовые хуки
        if let Some(hook) = &task.test_hook {
            if hook == "hang_10s" {
                thread::sleep(Duration::from_secs(10));
            } else if hook == "abort" {
                std::process::abort();
            }
        }

        // Байты, если переданы, важнее пути: файл с диска повторно не читается
        let extract_start = Instant::now();
        let extracted = if let Some(data) = &task.data {
            extract(
                ExtractSource::Bytes(data),
                &task.extension,
                task.size,
                task.sensitive,
                &task.config,
            )
        } else if let Some(path) = &task.path {
            extract(
                ExtractSource::Path(path),
                &task.extension,
                task.size,
                task.sensitive,
                &task.config,
            )
        } else {
            Extracted::failed("Отсутствует источник для извлечения")
        };

        let resp = WorkerResponse {
            id: task.id,
            extracted,
            extract_ns: extract_start.elapsed().as_nanos() as u64,
            decode_ns,
        };

        if let Ok(json) = serde_json::to_string(&resp) {
            let _ = writeln!(writer, "{json}");
            let _ = writer.flush();
        }
    }
}

/// Слот рабочего процесса внутри пула.
struct WorkerSlot {
    exe_path: PathBuf,
    job_object: Option<Arc<JobObjectGuard>>,
    child: Option<Child>,
    /// Буферизованная запись в stdin рабочего процесса.
    stdin: Option<BufWriter<ChildStdin>>,
    /// Повторно используемый буфер заголовка задания.
    header: Vec<u8>,
    rx: Option<Receiver<String>>,
    stats: Arc<WorkerStats>,
}

impl WorkerSlot {
    fn new(
        exe_path: PathBuf,
        job_object: Option<Arc<JobObjectGuard>>,
        stats: Arc<WorkerStats>,
    ) -> Self {
        let mut slot = Self {
            exe_path,
            job_object,
            child: None,
            stdin: None,
            header: Vec::new(),
            rx: None,
            stats,
        };
        let _ = slot.spawn();
        slot
    }

    fn spawn(&mut self) -> Result<(), String> {
        let mut cmd = Command::new(&self.exe_path);
        cmd.arg("--worker");
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::null());

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Не удалось запустить рабочий процесс: {e}"))?;

        if let Some(job) = &self.job_object {
            job.assign_child(&child);
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Не удалось захватить stdout процесса".to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Не удалось захватить stdin процесса".to_string())?;

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while let Ok(n) = reader.read_line(&mut line) {
                if n == 0 {
                    break;
                }
                if tx.send(line.clone()).is_err() {
                    break;
                }
                line.clear();
            }
        });

        self.child = Some(child);
        self.stdin = Some(BufWriter::with_capacity(WRITE_BUF, stdin));
        self.rx = Some(rx);
        Ok(())
    }

    fn restart(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.stdin = None;
        self.rx = None;
        let _ = self.spawn();
    }

    fn execute(&mut self, task: &WorkerTask, timeout: Duration) -> Extracted {
        let serialize_start = Instant::now();
        let data_len = match serialize_header(task, &mut self.header) {
            Ok(n) => n,
            Err(e) => return Extracted::failed(format!("Ошибка сериализации задачи: {e}")),
        };
        self.stats.tasks.fetch_add(1, Ordering::Relaxed);
        self.stats
            .json_bytes
            .fetch_add(self.header.len() as u64 + data_len, Ordering::Relaxed);
        self.stats.serialize_ns.fetch_add(
            serialize_start.elapsed().as_nanos() as u64,
            Ordering::Relaxed,
        );

        if self.child.is_none()
            && let Err(e) = self.spawn()
        {
            return Extracted::failed(e);
        }

        // Отправляем задачу в stdin процесса: заголовок и следом данные, один flush
        let write_start = Instant::now();
        let data = task.data.as_deref().unwrap_or(&[]);
        let write_ok = match self.stdin.as_mut() {
            Some(stdin) => write_frame(stdin, &self.header, data).is_ok(),
            None => false,
        };

        self.stats
            .write_ns
            .fetch_add(write_start.elapsed().as_nanos() as u64, Ordering::Relaxed);

        if !write_ok {
            self.restart();
            return Extracted::failed("извлекатель аварийно завершился");
        }

        // Ожидаем ответ с таймаутом
        let rx = match self.rx.as_ref() {
            Some(r) => r,
            None => {
                self.restart();
                return Extracted::failed("извлекатель аварийно завершился");
            }
        };

        let wait_start = Instant::now();
        let reply = rx.recv_timeout(timeout);
        self.stats
            .reply_wait_ns
            .fetch_add(wait_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        match reply {
            Ok(line) => {
                let parse_start = Instant::now();
                let trimmed = line.trim();
                let parsed = serde_json::from_str::<WorkerResponse>(trimmed);
                self.stats
                    .parse_reply_ns
                    .fetch_add(parse_start.elapsed().as_nanos() as u64, Ordering::Relaxed);
                match parsed {
                    Ok(resp) => {
                        self.stats
                            .child_extract_ns
                            .fetch_add(resp.extract_ns, Ordering::Relaxed);
                        self.stats
                            .child_decode_ns
                            .fetch_add(resp.decode_ns, Ordering::Relaxed);
                        resp.extracted
                    }
                    Err(e) => Extracted::failed(format!("Некорректный ответ процесса: {e}")),
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                self.restart();
                Extracted::failed(format!("превышено время {} с", timeout.as_secs()))
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.restart();
                Extracted::failed("извлекатель аварийно завершился")
            }
        }
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

type TaskMsg = (WorkerTask, Sender<Extracted>);

/// Пул рабочих процессов для параллельного извлечения текста.
pub struct WorkerPool {
    task_tx: Option<Sender<TaskMsg>>,
    workers: Vec<thread::JoinHandle<()>>,
    in_process: bool,
    timeout: Duration,
    default_config: ExtractConfig,
    stats: Arc<WorkerStats>,
}

impl WorkerPool {
    /// Создаёт новый пул рабочих процессов.
    ///
    /// Если `num_workers == 0`, число процессов выбирается по числу ядер процессора.
    /// Если `in_process == true`, извлечение выполняется в том же процессе без запуска подпроцессов (для тестов).
    pub fn new(
        exe_path: Option<PathBuf>,
        num_workers: usize,
        timeout: Duration,
        in_process: bool,
        config: ExtractConfig,
    ) -> Self {
        let workers_count = if num_workers == 0 {
            thread::available_parallelism().map_or(4, |n| n.get())
        } else {
            num_workers
        };

        if in_process {
            return Self {
                task_tx: None,
                workers: Vec::new(),
                in_process: true,
                timeout,
                default_config: config,
                stats: Arc::new(WorkerStats::default()),
            };
        }

        let exe = exe_path.unwrap_or_else(find_worker_exe);
        let job_object = JobObjectGuard::new().map(Arc::new);
        let stats = Arc::new(WorkerStats::default());

        let (task_tx, task_rx) = mpsc::channel::<TaskMsg>();
        let task_rx = Arc::new(Mutex::new(task_rx));

        let mut workers = Vec::with_capacity(workers_count);
        for _ in 0..workers_count {
            let rx = Arc::clone(&task_rx);
            let exe_clone = exe.clone();
            let job_clone = job_object.clone();
            let stats_clone = Arc::clone(&stats);

            let handle = thread::spawn(move || {
                let mut slot = WorkerSlot::new(exe_clone, job_clone, stats_clone);
                loop {
                    let task_opt = {
                        let lock = rx.lock().unwrap();
                        lock.recv().ok()
                    };
                    match task_opt {
                        Some((task, reply_tx)) => {
                            let result = slot.execute(&task, timeout);
                            let _ = reply_tx.send(result);
                        }
                        None => break, // канал закрыт
                    }
                }
            });
            workers.push(handle);
        }

        Self {
            task_tx: Some(task_tx),
            workers,
            in_process: false,
            timeout,
            default_config: config,
            stats,
        }
    }

    /// Счётчики времени обмена с рабочими процессами (при извлечении в процессе — нули).
    pub fn stats(&self) -> &WorkerStats {
        &self.stats
    }

    /// Возвращает настроенный таймаут на извлечение файла.
    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Возвращает конфигурацию извлечения по умолчанию.
    pub fn config(&self) -> &ExtractConfig {
        &self.default_config
    }

    /// Извлекает текст из одного файла через пул.
    pub fn extract_file(&self, path: &Path, sensitive: bool) -> Extracted {
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        let task = WorkerTask {
            id: 1,
            path: Some(path.to_path_buf()),
            data: None,
            extension: ext,
            size,
            sensitive,
            config: self.default_config.clone(),
            test_hook: None,
        };
        self.submit_task(task)
    }

    /// Извлекает текст из байтов через пул.
    pub fn extract_bytes(&self, bytes: &[u8], extension: &str, sensitive: bool) -> Extracted {
        let task = WorkerTask {
            id: 1,
            path: None,
            data: Some(bytes.to_vec()),
            extension: extension.to_string(),
            size: bytes.len() as u64,
            sensitive,
            config: self.default_config.clone(),
            test_hook: None,
        };
        self.submit_task(task)
    }

    /// Отправляет задачу в пул и ожидает результат.
    pub fn submit_task(&self, task: WorkerTask) -> Extracted {
        if self.in_process {
            if let Some(hook) = &task.test_hook {
                if hook == "hang_10s" {
                    thread::sleep(Duration::from_secs(10));
                } else if hook == "abort" {
                    return Extracted::failed("извлекатель аварийно завершился");
                }
            }
            return if let Some(data) = &task.data {
                extract(
                    ExtractSource::Bytes(data),
                    &task.extension,
                    task.size,
                    task.sensitive,
                    &task.config,
                )
            } else if let Some(path) = &task.path {
                extract(
                    ExtractSource::Path(path),
                    &task.extension,
                    task.size,
                    task.sensitive,
                    &task.config,
                )
            } else {
                Extracted::failed("Отсутствует источник для извлечения")
            };
        }

        let (reply_tx, reply_rx) = mpsc::channel();
        if let Some(tx) = &self.task_tx {
            if tx.send((task, reply_tx)).is_err() {
                return Extracted::failed("Пул процессов недоступен");
            }
            reply_rx
                .recv()
                .unwrap_or_else(|_| Extracted::failed("извлекатель аварийно завершился"))
        } else {
            Extracted::failed("Пул процессов не инициализирован")
        }
    }

    /// Обрабатывает пакет задач параллельно через пул процессов.
    pub fn extract_batch(&self, tasks: Vec<WorkerTask>) -> Vec<(u64, Extracted)> {
        if self.in_process {
            return tasks
                .into_iter()
                .map(|t| {
                    let id = t.id;
                    let ext = self.submit_task(t);
                    (id, ext)
                })
                .collect();
        }

        let mut receivers = Vec::with_capacity(tasks.len());
        if let Some(tx) = &self.task_tx {
            for task in tasks {
                let id = task.id;
                let (reply_tx, reply_rx) = mpsc::channel();
                if tx.send((task, reply_tx)).is_ok() {
                    receivers.push((id, reply_rx));
                } else {
                    receivers.push((id, {
                        let (dummy_tx, dummy_rx) = mpsc::channel();
                        let _ = dummy_tx.send(Extracted::failed("Пул процессов недоступен"));
                        dummy_rx
                    }));
                }
            }
        }

        receivers
            .into_iter()
            .map(|(id, rx)| {
                let res = rx
                    .recv()
                    .unwrap_or_else(|_| Extracted::failed("извлекатель аварийно завершился"));
                (id, res)
            })
            .collect()
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        // Закрываем канал, чтобы рабочие потоки завершили циклы
        drop(self.task_tx.take());
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn task(id: u64, path: Option<&str>, data: Option<Vec<u8>>) -> WorkerTask {
        WorkerTask {
            id,
            path: path.map(PathBuf::from),
            size: data.as_ref().map_or(0, |d| d.len() as u64),
            data,
            extension: "txt".to_string(),
            sensitive: false,
            config: ExtractConfig::default(),
            test_hook: None,
        }
    }

    fn encode(t: &WorkerTask) -> Vec<u8> {
        let mut out = Vec::new();
        write_task(&mut out, t).unwrap();
        out
    }

    fn decode(bytes: Vec<u8>) -> Result<Option<(WorkerTask, u64)>, String> {
        read_task(&mut Cursor::new(bytes), &mut String::new())
    }

    #[test]
    fn tricky_bytes_round_trip() {
        let mut data = vec![0x00, b'\n', b'\r', 0xFF, b'\n', b'\n', 0x00, 0xFE];
        data.extend("Сахалин ёжик 日本語 😀".as_bytes());
        data.extend(0..=255u8);
        let wire = encode(&task(7, None, Some(data.clone())));
        let (back, _) = decode(wire).unwrap().unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.data.as_deref(), Some(&data[..]));
    }

    #[test]
    fn data_is_raw_not_hex() {
        let data = vec![0xAB; 1000];
        let wire = encode(&task(1, None, Some(data)));
        let header_end = wire.iter().position(|&b| b == b'\n').unwrap() + 1;
        assert_eq!(
            wire.len() - header_end,
            1000,
            "после заголовка ровно data_len байт"
        );
        assert!(wire[header_end..].iter().all(|&b| b == 0xAB));
    }

    #[test]
    fn empty_data_and_no_data_are_distinct() {
        let (empty, _) = decode(encode(&task(1, None, Some(Vec::new()))))
            .unwrap()
            .unwrap();
        assert_eq!(empty.data, Some(Vec::new()));
        let (none, _) = decode(encode(&task(2, Some("x.txt"), None)))
            .unwrap()
            .unwrap();
        assert_eq!(none.data, None);
        assert_eq!(none.path, Some(PathBuf::from("x.txt")));
    }

    #[test]
    fn several_tasks_in_one_stream() {
        let mut wire = encode(&task(1, None, Some(b"a\nb".to_vec())));
        wire.extend(encode(&task(2, Some("x.txt"), None)));
        wire.extend(encode(&task(3, None, Some(vec![0, 0, 0]))));
        let mut cur = Cursor::new(wire);
        let mut line = String::new();
        let ids_data: Vec<_> = std::iter::from_fn(|| read_task(&mut cur, &mut line).unwrap())
            .map(|(t, _)| (t.id, t.data))
            .collect();
        assert_eq!(
            ids_data,
            vec![
                (1, Some(b"a\nb".to_vec())),
                (2, None),
                (3, Some(vec![0, 0, 0]))
            ]
        );
    }

    #[test]
    fn truncated_stream_is_error() {
        let mut wire = encode(&task(1, None, Some(vec![5; 100])));
        wire.truncate(wire.len() - 10);
        let err = decode(wire).unwrap_err();
        assert!(err.contains("оборванный поток"), "{err}");
    }

    #[test]
    fn oversize_data_len_is_error_without_allocation() {
        let mut header = Vec::new();
        serialize_header(&task(1, None, None), &mut header).unwrap();
        let text = String::from_utf8(header).unwrap().replace(
            "\"data_len\":0",
            &format!("\"data_len\":{}", MAX_DATA_LEN + 1),
        );
        let err = decode(text.into_bytes()).unwrap_err();
        assert!(err.contains("больше предела"), "{err}");
    }

    #[test]
    fn garbage_header_is_error_and_eof_is_none() {
        assert!(decode(b"not json\n".to_vec()).is_err());
        assert!(decode(b"\xFF\xFE\n".to_vec()).is_err());
        assert!(decode(Vec::new()).unwrap().is_none());
        assert!(decode(b"\n\r\n".to_vec()).unwrap().is_none());
    }

    #[test]
    fn data_len_limit_is_checked_without_allocation() {
        assert!(check_data_len(MAX_DATA_LEN).is_ok());
        assert!(check_data_len(MAX_DATA_LEN + 1).is_err());
        const { assert!(MAX_DATA_LEN >= 500 * 1024 * 1024) }; // предел настройки zip, 500 МБ
    }

    #[test]
    fn serve_answers_each_task_and_fails_on_truncated_tail() {
        let mut wire = encode(&task(10, None, Some("привет\r\n\0мир".as_bytes().to_vec())));
        wire.extend(encode(&task(11, None, Some(Vec::new()))));
        let mut tail = encode(&task(12, None, Some(vec![b'x'; 50])));
        tail.truncate(tail.len() - 7);
        wire.extend(tail);

        let mut out = Vec::new();
        serve(&mut Cursor::new(wire), &mut out);
        let replies: Vec<WorkerResponse> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            replies.len(),
            3,
            "два ответа и ошибка на оборванное задание"
        );
        assert_eq!(replies[0].id, 10);
        assert_eq!(replies[0].extracted.status, "text");
        assert!(replies[0].extracted.text.contains("привет"));
        assert_eq!(replies[1].id, 11);
        assert_eq!(replies[2].extracted.status, "failed");
        assert!(
            replies[2]
                .extracted
                .error
                .as_deref()
                .unwrap_or("")
                .contains("оборванный поток")
        );
    }
}

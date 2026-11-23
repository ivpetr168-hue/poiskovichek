use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use rusqlite::{Connection, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::pipeline::CancelToken;

pub const QUICK_HASH_THRESHOLD: u64 = 262_144; // 256 КБ
pub const QUICK_HASH_CHUNK: u64 = 131_072; // 128 КБ
pub const HASH_BUFFER_SIZE: usize = 64 * 1024; // 64 КБ буфер чтения

/// Как часто главный поток просыпается, если новых результатов нет.
const TICK: Duration = Duration::from_millis(100);
/// Если число не менялось, ход работы всё равно сообщается не реже этого срока.
const HEARTBEAT: Duration = Duration::from_secs(1);

static FILE_READ_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Возвращает общее количество выполненных обращений к файлам на диске для чтения содержимого.
pub fn get_file_read_count() -> usize {
    FILE_READ_COUNT.load(Ordering::SeqCst)
}

/// Сбрасывает глобальный счетчик прочитанных файлов.
pub fn reset_file_read_count() {
    FILE_READ_COUNT.store(0, Ordering::SeqCst);
}

/// Статистика выполнения поиска дубликатов за один вызов.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DuplicateStats {
    pub files_read: usize,
}

/// Группа точных дубликатов по полному хэшу SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DuplicateGroup {
    pub sha256: String,
    pub file_count: usize,
    pub total_bytes: u64,
    pub reclaimable_bytes: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DuplicateError {
    #[error("Ошибка базы данных SQLite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("Ошибка ввода-вывода: {0}")]
    Io(#[from] std::io::Error),
    #[error("Операция поиска дубликатов отменена")]
    Cancelled,
    #[error("Ошибка пула рабочих потоков: {0}")]
    ThreadPool(String),
}

/// Этап поиска дубликатов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateStage {
    /// Быстрые отпечатки всех файлов-кандидатов.
    QuickHash,
    /// Полные отпечатки файлов с совпавшим быстрым отпечатком.
    FullHash,
    /// Сборка групп дубликатов из сохранённых отпечатков.
    Finishing,
}

/// Ход поиска дубликатов в единицах файлов (и байтов на этапе полных отпечатков).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DuplicateProgress {
    pub stage: DuplicateStage,
    pub files_done: usize,
    pub files_total: usize,
    /// Прочитано байтов (только на этапе полных отпечатков, иначе 0).
    pub bytes_done: u64,
    /// Нужно прочитать байтов (только на этапе полных отпечатков, иначе 0).
    pub bytes_total: u64,
}

/// Настройки частоты сохранения и сообщений о ходе работы.
#[derive(Debug, Clone)]
pub struct DuplicateOptions {
    /// Записывать посчитанные отпечатки в базу, когда накопилось столько файлов.
    pub flush_files: usize,
    /// …или когда самой старой несохранённой записи столько времени.
    pub flush_interval: Duration,
    /// Не сообщать о ходе работы чаще этого интервала.
    pub progress_interval: Duration,
    /// Размер очереди результатов от рабочих потоков к главному (ограничивает «забег вперёд»).
    pub queue_len: usize,
}

impl Default for DuplicateOptions {
    fn default() -> Self {
        Self {
            flush_files: 2000,
            flush_interval: Duration::from_secs(5),
            progress_interval: Duration::from_millis(250),
            queue_len: 64,
        }
    }
}

/// Вычисляет быстрый хэш файла:
/// - если размер > 256 КБ: первые 128 КБ + последние 128 КБ;
/// - иначе: файл целиком (совпадает с полным SHA-256).
pub fn compute_quick_hash(path: &Path, size: u64) -> Result<String, std::io::Error> {
    FILE_READ_COUNT.fetch_add(1, Ordering::SeqCst);
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();

    if size > QUICK_HASH_THRESHOLD {
        let mut head = vec![0u8; QUICK_HASH_CHUNK as usize];
        file.read_exact(&mut head)?;
        hasher.update(&head);

        file.seek(SeekFrom::Start(size - QUICK_HASH_CHUNK))?;
        let mut tail = vec![0u8; QUICK_HASH_CHUNK as usize];
        file.read_exact(&mut tail)?;
        hasher.update(&tail);
    } else {
        let mut buf = [0u8; HASH_BUFFER_SIZE];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Вычисляет полный SHA-256 хэш файла на диске.
pub fn compute_full_hash(path: &Path) -> Result<String, std::io::Error> {
    FILE_READ_COUNT.fetch_add(1, Ordering::SeqCst);
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; HASH_BUFFER_SIZE];

    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }

    Ok(hex::encode(hasher.finalize()))
}

/// Полный SHA-256 с проверкой отмены на каждом буфере и учётом прочитанных байтов.
/// `Ok(None)` — отменено посреди файла.
fn compute_full_hash_tracked(
    path: &Path,
    cancel: &CancelToken,
    bytes_done: &AtomicU64,
) -> Result<Option<String>, std::io::Error> {
    FILE_READ_COUNT.fetch_add(1, Ordering::SeqCst);
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; HASH_BUFFER_SIZE];

    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes_done.fetch_add(n as u64, Ordering::Relaxed);
    }

    Ok(Some(hex::encode(hasher.finalize())))
}

#[derive(Debug, Clone)]
struct CandidateFile {
    /// Порядковый номер в общем списке кандидатов.
    idx: usize,
    /// Номер группы одинакового размера.
    group: usize,
    id: i64,
    path: String,
    size: u64,
    modified_ns: i64,
    cached_quick_hash: Option<String>,
    cached_sha256: Option<String>,
}

/// Результат этапа быстрых отпечатков для одного файла.
struct QuickResult {
    idx: usize,
    /// `None` — файл пропущен (нет на диске, ошибка чтения).
    qh: Option<String>,
    /// Размер файла на диске в момент проверки.
    meta_size: u64,
    /// Размер и время на диске совпадают с записью в базе.
    unchanged: bool,
    /// Отпечаток посчитан заново (а не взят из базы).
    recomputed: bool,
}

struct FullResult {
    idx: usize,
    sha: Option<String>,
}

/// Что записать в `files`.
enum Update {
    /// Новый быстрый отпечаток. Полный сбрасывается: он относился к прежнему содержимому.
    Quick {
        id: i64,
        qh: String,
    },
    Sha {
        id: i64,
        sha: String,
    },
}

/// Копит посчитанное и записывает его в базу короткими транзакциями.
struct Saver {
    pending: Vec<Update>,
    since: Option<Instant>,
    flush_files: usize,
    flush_interval: Duration,
}

impl Saver {
    fn new(options: &DuplicateOptions) -> Self {
        Self {
            pending: Vec::new(),
            since: None,
            flush_files: options.flush_files.max(1),
            flush_interval: options.flush_interval,
        }
    }

    fn push(&mut self, update: Update) {
        if self.pending.is_empty() {
            self.since = Some(Instant::now());
        }
        self.pending.push(update);
    }

    fn maybe_flush(&mut self, conn: &mut Connection) -> Result<(), DuplicateError> {
        let due = self.pending.len() >= self.flush_files
            || self
                .since
                .is_some_and(|t| !self.pending.is_empty() && t.elapsed() >= self.flush_interval);
        if due { self.flush(conn) } else { Ok(()) }
    }

    /// Одна короткая транзакция. При ошибке записи накопленное остаётся для повторной попытки.
    fn flush(&mut self, conn: &mut Connection) -> Result<(), DuplicateError> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut quick_stmt = tx
                .prepare_cached("UPDATE files SET quick_hash = ?1, sha256 = NULL WHERE id = ?2;")?;
            let mut sha_stmt = tx.prepare_cached("UPDATE files SET sha256 = ?1 WHERE id = ?2;")?;
            for update in &self.pending {
                match update {
                    Update::Quick { id, qh } => quick_stmt.execute(params![qh, id])?,
                    Update::Sha { id, sha } => sha_stmt.execute(params![sha, id])?,
                };
            }
        }
        tx.commit()?;
        self.pending.clear();
        self.since = None;
        Ok(())
    }
}

/// Ограничивает частоту сообщений о ходе работы.
struct Reporter {
    interval: Duration,
    last: Instant,
    last_key: (usize, u64),
}

impl Reporter {
    fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: Instant::now(),
            last_key: (usize::MAX, u64::MAX),
        }
    }

    /// Сообщает, если прошёл интервал и число изменилось (или прошла секунда). `force` — всегда.
    fn report(
        &mut self,
        p: DuplicateProgress,
        force: bool,
        out: &mut impl FnMut(DuplicateProgress),
    ) {
        let key = (p.files_done, p.bytes_done);
        let elapsed = self.last.elapsed();
        if force || (elapsed >= self.interval && (key != self.last_key || elapsed >= HEARTBEAT)) {
            out(p);
            self.last = Instant::now();
            self.last_key = key;
        }
    }
}

/// Размер и время изменения файла на диске (наносекунды от эпохи).
fn file_meta(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Some((meta.len(), mtime_ns))
}

/// Обрабатывает `items` в пуле потоков. Результаты приходят главному потоку по ограниченной
/// очереди; `on_event(Some(r))` — на каждый результат, `on_event(None)` — по таймеру.
/// После отмены рабочие потоки не начинают новых файлов; уже посчитанное доходит до
/// `on_event`. Ошибка `on_event` останавливает работу и возвращается.
fn run_pool<T: Sync, R: Send>(
    pool: &rayon::ThreadPool,
    items: &[T],
    cancel: &CancelToken,
    queue_len: usize,
    work: impl Fn(&T) -> Option<R> + Sync,
    mut on_event: impl FnMut(Option<R>) -> Result<(), DuplicateError>,
) -> Result<(), DuplicateError> {
    let stop = AtomicBool::new(false);
    let (tx, rx) = sync_channel::<R>(queue_len.max(1));
    let work = &work;
    let stop_ref = &stop;
    let mut outcome = Ok(());

    std::thread::scope(|scope| {
        scope.spawn(move || {
            pool.install(|| {
                use rayon::prelude::*;
                items.par_iter().for_each_with(tx, |tx, item| {
                    if cancel.is_cancelled() || stop_ref.load(Ordering::Relaxed) {
                        return;
                    }
                    if let Some(r) = work(item) {
                        let _ = tx.send(r);
                    }
                });
            });
        });

        loop {
            let event = match rx.recv_timeout(TICK) {
                Ok(r) => Some(r),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            // После ошибки только вычитываем очередь, чтобы рабочие потоки не зависли на отправке.
            if outcome.is_ok()
                && let Err(e) = on_event(event)
            {
                outcome = Err(e);
                stop.store(true, Ordering::Relaxed);
            }
        }
    });

    outcome
}

/// Выполняет поиск точных дубликатов в каталоге с возвратом статистики за вызов.
pub fn find_duplicates_with_stats(
    catalog_path: &Path,
    cancel: &CancelToken,
    progress: impl FnMut(DuplicateProgress),
) -> Result<(Vec<DuplicateGroup>, DuplicateStats), DuplicateError> {
    find_duplicates_with_options(catalog_path, cancel, &DuplicateOptions::default(), progress)
}

/// То же, что `find_duplicates_with_stats`, с настройкой частоты сохранения и сообщений.
pub fn find_duplicates_with_options(
    catalog_path: &Path,
    cancel: &CancelToken,
    options: &DuplicateOptions,
    mut progress: impl FnMut(DuplicateProgress),
) -> Result<(Vec<DuplicateGroup>, DuplicateStats), DuplicateError> {
    if cancel.is_cancelled() {
        return Err(DuplicateError::Cancelled);
    }

    let local_reads = AtomicUsize::new(0);

    let mut conn = Connection::open(catalog_path)?;
    conn.busy_timeout(Duration::from_secs(30))?;

    // Кандидаты: файлы тех размеров, которых в каталоге больше одного (правило отбора прежнее).
    let mut cands: Vec<CandidateFile> = Vec::new();
    {
        let mut stmt = conn.prepare(
            r#"
            SELECT id, path, size, modified_ns, quick_hash, sha256
            FROM files
            WHERE error IS NULL
              AND inside_archive = 0
              AND available != 0
              AND size > 1
              AND size IN (
                  SELECT size
                  FROM files
                  WHERE error IS NULL
                    AND inside_archive = 0
                    AND available != 0
                    AND size > 1
                  GROUP BY size
                  HAVING COUNT(*) > 1
              )
            ORDER BY size, id;
            "#,
        )?;
        let mut rows = stmt.query([])?;
        let mut group = 0usize;
        let mut prev_size: Option<u64> = None;
        while let Some(row) = rows.next()? {
            let size = row.get::<_, i64>(2)? as u64;
            if prev_size.is_some_and(|p| p != size) {
                group += 1;
            }
            prev_size = Some(size);
            let idx = cands.len();
            cands.push(CandidateFile {
                idx,
                group,
                id: row.get(0)?,
                path: row.get(1)?,
                size,
                modified_ns: row.get(3)?,
                cached_quick_hash: row.get(4)?,
                cached_sha256: row.get(5)?,
            });
        }
    }

    let total_files = cands.len();
    let mut reporter = Reporter::new(options.progress_interval);
    let stage1 = |files_done: usize| DuplicateProgress {
        stage: DuplicateStage::QuickHash,
        files_done,
        files_total: total_files,
        bytes_done: 0,
        bytes_total: 0,
    };
    reporter.report(stage1(0), true, &mut progress);

    if total_files == 0 {
        // Очищаем таблицу duplicate_groups при отсутствии дубликатов
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM duplicate_groups;", [])?;
        tx.commit()?;
        return Ok((Vec::new(), DuplicateStats::default()));
    }

    let thread_pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .map_err(|e| DuplicateError::ThreadPool(e.to_string()))?;

    let mut saver = Saver::new(options);

    // ---- Этап 1: быстрые отпечатки ----
    let mut quick: Vec<Option<QuickResult>> = Vec::new();
    quick.resize_with(total_files, || None);
    let mut files_done = 0usize;

    let quick_outcome = run_pool(
        &thread_pool,
        &cands,
        cancel,
        options.queue_len,
        |cand: &CandidateFile| {
            let skipped = QuickResult {
                idx: cand.idx,
                qh: None,
                meta_size: cand.size,
                unchanged: false,
                recomputed: false,
            };
            let p = Path::new(&cand.path);
            let Some((meta_size, meta_mtime_ns)) = file_meta(p) else {
                return Some(skipped); // Файла нет или он недоступен — пропуск
            };

            // Отпечаток из базы годится, пока размер и время файла на диске равны записанным.
            let unchanged = meta_size == cand.size && meta_mtime_ns == cand.modified_ns;
            if unchanged && let Some(qh) = &cand.cached_quick_hash {
                return Some(QuickResult {
                    idx: cand.idx,
                    qh: Some(qh.clone()),
                    meta_size,
                    unchanged,
                    recomputed: false,
                });
            }

            if cancel.is_cancelled() {
                return None;
            }
            local_reads.fetch_add(1, Ordering::SeqCst);
            match compute_quick_hash(p, meta_size) {
                Ok(hash) => Some(QuickResult {
                    idx: cand.idx,
                    qh: Some(hash),
                    meta_size,
                    unchanged,
                    recomputed: true,
                }),
                Err(_) => Some(skipped), // Ошибка чтения одного файла — пропуск
            }
        },
        |event| {
            if let Some(r) = event {
                files_done += 1;
                if r.recomputed
                    && let Some(qh) = &r.qh
                {
                    saver.push(Update::Quick {
                        id: cands[r.idx].id,
                        qh: qh.clone(),
                    });
                }
                let idx = r.idx;
                quick[idx] = Some(r);
            }
            saver.maybe_flush(&mut conn)?;
            reporter.report(stage1(files_done), false, &mut progress);
            Ok(())
        },
    );

    // Посчитанное сохраняем при любом исходе (в том числе при отмене).
    let flush_result = saver.flush(&mut conn);
    quick_outcome?;
    flush_result?;
    if cancel.is_cancelled() {
        return Err(DuplicateError::Cancelled);
    }
    reporter.report(stage1(files_done), true, &mut progress);

    // ---- Этап 2: полные отпечатки у файлов с совпавшим быстрым ----
    let mut by_quick: HashMap<(usize, &str), Vec<usize>> = HashMap::new();
    for (i, q) in quick.iter().enumerate() {
        if let Some(q) = q
            && let Some(qh) = &q.qh
        {
            by_quick
                .entry((cands[i].group, qh.as_str()))
                .or_default()
                .push(i);
        }
    }
    let mut matching: Vec<usize> = by_quick
        .into_values()
        .filter(|v| v.len() >= 2)
        .flatten()
        .collect();
    matching.sort_unstable();

    let full_total = matching.len();
    let mut full_done = 0usize;
    let mut bytes_total = 0u64;
    let mut to_read: Vec<usize> = Vec::new();
    for &i in &matching {
        let c = &cands[i];
        let Some(q) = &quick[i] else { continue };
        let Some(qh) = &q.qh else { continue };
        if q.meta_size <= QUICK_HASH_THRESHOLD {
            // Файл прочитан целиком: быстрый отпечаток уже равен полному SHA-256
            full_done += 1;
            if q.recomputed || c.cached_sha256.as_deref() != Some(qh.as_str()) {
                saver.push(Update::Sha {
                    id: c.id,
                    sha: qh.clone(),
                });
            }
        } else if q.unchanged && !q.recomputed && c.cached_sha256.is_some() {
            // Полный отпечаток сохранён и файл не менялся — читать не нужно
            full_done += 1;
        } else {
            bytes_total += q.meta_size;
            to_read.push(i);
        }
    }

    let bytes_done = AtomicU64::new(0);
    let stage2 = |files_done: usize| DuplicateProgress {
        stage: DuplicateStage::FullHash,
        files_done,
        files_total: full_total,
        bytes_done: bytes_done.load(Ordering::Relaxed),
        bytes_total,
    };
    reporter.report(stage2(full_done), true, &mut progress);

    let full_outcome = run_pool(
        &thread_pool,
        &to_read,
        cancel,
        options.queue_len,
        |&i: &usize| {
            if cancel.is_cancelled() {
                return None;
            }
            local_reads.fetch_add(1, Ordering::SeqCst);
            match compute_full_hash_tracked(Path::new(&cands[i].path), cancel, &bytes_done) {
                Ok(Some(sha)) => Some(FullResult {
                    idx: i,
                    sha: Some(sha),
                }),
                Ok(None) => None, // отменено посреди файла
                Err(_) => Some(FullResult { idx: i, sha: None }),
            }
        },
        |event| {
            if let Some(r) = event {
                full_done += 1;
                if let Some(sha) = r.sha {
                    saver.push(Update::Sha {
                        id: cands[r.idx].id,
                        sha,
                    });
                }
            }
            saver.maybe_flush(&mut conn)?;
            reporter.report(stage2(full_done), false, &mut progress);
            Ok(())
        },
    );

    let flush_result = saver.flush(&mut conn);
    full_outcome?;
    flush_result?;
    if cancel.is_cancelled() {
        return Err(DuplicateError::Cancelled);
    }
    reporter.report(stage2(full_done), true, &mut progress);

    // ---- Этап 3: группы дубликатов собираются из сохранённых отпечатков ----
    reporter.report(
        DuplicateProgress {
            stage: DuplicateStage::Finishing,
            files_done: full_done,
            files_total: full_total,
            bytes_done: bytes_done.load(Ordering::Relaxed),
            bytes_total,
        },
        true,
        &mut progress,
    );

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute("DELETE FROM duplicate_groups;", [])?;
    tx.execute(
        r#"
        INSERT INTO duplicate_groups(sha256, file_count, total_bytes, reclaimable_bytes)
        SELECT sha256, COUNT(*), SUM(size), SUM(size) - MAX(size)
        FROM files
        WHERE sha256 IS NOT NULL
          AND inside_archive = 0
          AND available != 0
          AND error IS NULL
        GROUP BY sha256
        HAVING COUNT(*) > 1;
        "#,
        [],
    )?;

    let mut groups = Vec::new();
    {
        let mut group_stmt = tx.prepare(
            r#"
            SELECT sha256, file_count, total_bytes, reclaimable_bytes
            FROM duplicate_groups
            ORDER BY reclaimable_bytes DESC, sha256 ASC;
            "#,
        )?;

        let mut rows = group_stmt.query([])?;
        while let Some(row) = rows.next()? {
            groups.push(DuplicateGroup {
                sha256: row.get(0)?,
                file_count: row.get::<_, i64>(1)? as usize,
                total_bytes: row.get::<_, i64>(2)? as u64,
                reclaimable_bytes: row.get::<_, i64>(3)? as u64,
            });
        }
    }

    tx.commit()?;

    let stats = DuplicateStats {
        files_read: local_reads.load(Ordering::SeqCst),
    };

    Ok((groups, stats))
}

/// Выполняет поиск точных дубликатов в каталоге.
///
/// 1. Группирует файлы по размеру (> 1 байта, без ошибок, не внутри архивов, доступные).
/// 2. Этап «быстрые отпечатки»: файл > 256 КБ — первые и последние 128 КБ, иначе целиком.
/// 3. Этап «полные отпечатки»: полный SHA-256 только у файлов с совпавшим быстрым отпечатком.
/// 4. Чтение файлов — параллельно через `rayon` (4 потока).
/// 5. Посчитанные `files.quick_hash/sha256` записываются порциями (по умолчанию каждые
///    2000 файлов или 5 секунд) короткими транзакциями; при отмене или сбое сохраняется всё
///    уже посчитанное. Таблица `duplicate_groups` пересобирается в конце из `files.sha256`.
/// 6. Ход работы (файлы, на этапе полных отпечатков ещё и байты) сообщается по времени:
///    не чаще 4 раз в секунду и не реже раза в секунду.
/// 7. Отпечаток из базы не пересчитывается, пока размер (`size`) и время изменения
///    (`modified_ns`) файла на диске равны записанным в `files`. Поэтому поиск, прерванный
///    отменой, при повторном запуске читает только непосчитанные файлы.
/// 8. Ошибка чтения одного файла — пропуск, не провал.
pub fn find_duplicates(
    catalog_path: &Path,
    cancel: &CancelToken,
    progress: impl FnMut(DuplicateProgress),
) -> Result<Vec<DuplicateGroup>, DuplicateError> {
    let (groups, _) = find_duplicates_with_stats(catalog_path, cancel, progress)?;
    Ok(groups)
}

/// Упрощенный поиск дубликатов без токена отмены и колбэка прогресса.
pub fn find_duplicates_simple(catalog_path: &Path) -> Result<Vec<DuplicateGroup>, DuplicateError> {
    find_duplicates(catalog_path, &CancelToken::new(), |_| {})
}

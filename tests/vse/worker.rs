use std::time::{Duration, Instant};

use poiskovichek::extract::ExtractConfig;
use poiskovichek::worker::{WorkerPool, WorkerTask, find_worker_exe};

#[test]
fn test_find_worker_exe() {
    let exe = find_worker_exe();
    println!("Found worker exe: {:?}", exe);
    assert!(
        exe.exists(),
        "Исполняемый файл Poiskovichek.exe должен существовать: {:?}",
        exe
    );
}

#[test]
fn test_worker_in_process_mode() {
    let config = ExtractConfig::default();
    let pool = WorkerPool::new(None, 2, Duration::from_secs(5), true, config.clone());

    let bytes = b"Test text in process".to_vec();
    let task = WorkerTask {
        id: 42,
        path: None,
        data: Some(bytes.clone()),
        extension: "txt".to_string(),
        size: bytes.len() as u64,
        sensitive: false,
        config,
        test_hook: None,
    };

    let res = pool.submit_task(task);
    assert_eq!(res.status, "text");
    assert_eq!(res.text, "Test text in process");
}

#[test]
fn test_worker_subprocess_normal() {
    let config = ExtractConfig::default();
    let pool = WorkerPool::new(None, 2, Duration::from_secs(10), false, config.clone());

    let bytes = "Сахалинский архив через рабочий процесс"
        .as_bytes()
        .to_vec();
    let task = WorkerTask {
        id: 101,
        path: None,
        data: Some(bytes.clone()),
        extension: "txt".to_string(),
        size: bytes.len() as u64,
        sensitive: false,
        config,
        test_hook: None,
    };

    let res = pool.submit_task(task);
    assert_eq!(res.status, "text");
    assert!(res.text.contains("Сахалинский архив"));
}

/// Рабочий процесс, который «зависает» (ждёт 10 с при пределе 1 с) -> failed «превышено время»,
/// пул продолжает работу со следующими файлами.
#[test]
fn test_worker_timeout_hang_and_recovery() {
    let config = ExtractConfig::default();
    // Предел времени 1 с
    let pool = WorkerPool::new(None, 1, Duration::from_secs(1), false, config.clone());

    // 1. Задача, которая зависает на 10 с
    let hang_task = WorkerTask {
        id: 201,
        path: None,
        data: Some(b"hang".to_vec()),
        extension: "txt".to_string(),
        size: 4,
        sensitive: false,
        config: config.clone(),
        test_hook: Some("hang_10s".to_string()),
    };

    let start = Instant::now();
    let res_hang = pool.submit_task(hang_task);
    let elapsed = start.elapsed();

    assert_eq!(res_hang.status, "failed");
    assert!(
        res_hang
            .error
            .as_deref()
            .unwrap_or("")
            .contains("превышено время"),
        "Ожидалась ошибка 'превышено время', получено: {:?}",
        res_hang.error
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "Убийство процесса заняло слишком много времени: {:?}",
        elapsed
    );

    // 2. Пул продолжает работу со следующим файлом
    let next_task = WorkerTask {
        id: 202,
        path: None,
        data: Some(b"recovered text".to_vec()),
        extension: "txt".to_string(),
        size: 14,
        sensitive: false,
        config,
        test_hook: None,
    };

    let res_next = pool.submit_task(next_task);
    assert_eq!(res_next.status, "text");
    assert_eq!(res_next.text, "recovered text");
}

/// Рабочий процесс, который падает (std::process::abort в заглушке) -> failed «аварийно завершился»,
/// следующий файл обрабатывается.
#[test]
fn test_worker_crash_abort_and_recovery() {
    let config = ExtractConfig::default();
    let pool = WorkerPool::new(None, 1, Duration::from_secs(15), false, config.clone());

    // 1. Задача, вызывающая abort
    let crash_task = WorkerTask {
        id: 301,
        path: None,
        data: Some(b"crash".to_vec()),
        extension: "txt".to_string(),
        size: 5,
        sensitive: false,
        config: config.clone(),
        test_hook: Some("abort".to_string()),
    };

    let res_crash = pool.submit_task(crash_task);
    assert_eq!(res_crash.status, "failed");
    assert!(
        res_crash
            .error
            .as_deref()
            .unwrap_or("")
            .contains("аварийно завершился"),
        "Ожидалась ошибка 'аварийно завершился', получено: {:?}",
        res_crash.error
    );

    // 2. Следующий файл обрабатывается успешно
    let next_task = WorkerTask {
        id: 302,
        path: None,
        data: Some(b"after crash text".to_vec()),
        extension: "txt".to_string(),
        size: 16,
        sensitive: false,
        config,
        test_hook: None,
    };

    let res_next = pool.submit_task(next_task);
    assert_eq!(res_next.status, "text");
    assert_eq!(res_next.text, "after crash text");
}

/// 500 файлов через пул из 4 процессов — все обработаны, ни одного зависания (предел 60 с).
#[test]
fn test_worker_pool_500_files() {
    let config = ExtractConfig::default();
    let pool = WorkerPool::new(None, 4, Duration::from_secs(10), false, config.clone());

    let mut tasks = Vec::with_capacity(500);
    for i in 0..500 {
        let bytes = format!("Содержимое документа номер {i}").into_bytes();
        let size = bytes.len() as u64;
        tasks.push(WorkerTask {
            id: i,
            path: None,
            data: Some(bytes),
            extension: "txt".to_string(),
            size,
            sensitive: false,
            config: config.clone(),
            test_hook: None,
        });
    }

    let start = Instant::now();
    let results = pool.extract_batch(tasks);
    let elapsed = start.elapsed();

    assert_eq!(results.len(), 500);
    for (id, res) in results {
        assert_eq!(res.status, "text");
        assert!(
            res.text.contains(&format!("номер {id}")),
            "Результат для {id} должен содержать 'номер {id}', текст: {:?}",
            res.text
        );
    }

    assert!(
        elapsed < Duration::from_secs(60),
        "500 файлов должны обрабатываться быстрее 60 с, заняло: {:?}",
        elapsed
    );
    println!("500 файлов успешно обработаны за {:?}", elapsed);
}

// ---- Двоичный обмен заданиями (данные идут сырыми байтами после JSON-заголовка) ----

fn byte_task(id: u64, path: Option<std::path::PathBuf>, data: Option<Vec<u8>>) -> WorkerTask {
    WorkerTask {
        id,
        path,
        size: data.as_ref().map_or(0, |d| d.len() as u64),
        data,
        extension: "txt".to_string(),
        sensitive: false,
        config: ExtractConfig::default(),
        test_hook: None,
    }
}

fn sub_pool(timeout_s: u64) -> WorkerPool {
    WorkerPool::new(
        None,
        1,
        Duration::from_secs(timeout_s),
        false,
        ExtractConfig::default(),
    )
}

/// Байты, которые ломают текстовый обмен: 0x00, `\n`, `\r`, 0xFF и UTF-8.
fn tricky_bytes() -> Vec<u8> {
    let mut data = b"line1\nline2\r\nline3\r".to_vec();
    data.extend([0x00, 0x0A, 0x0D, 0xFF, 0xFE, 0x00]);
    data.extend("Сахалинский архив, ёжик, 日本語, 😀\n".as_bytes());
    data.extend(0..=255u8);
    data
}

fn assert_same_result(a: &poiskovichek::extract::Extracted, b: &poiskovichek::extract::Extracted) {
    assert_eq!(a.status, b.status);
    assert_eq!(a.text, b.text);
    assert_eq!(a.error, b.error);
}

/// Данные с 0x00, `\n`, `\r`, 0xFF и UTF-8 доходят до рабочего процесса без искажений:
/// результат такой же, как при разборе в процессе.
#[test]
fn test_worker_binary_bytes_match_in_process() {
    let sub = sub_pool(20);
    let local = WorkerPool::new(
        None,
        1,
        Duration::from_secs(20),
        true,
        ExtractConfig::default(),
    );

    let cases: Vec<Vec<u8>> = vec![
        tricky_bytes(),
        "Строка 1\nСтрока 2\r\nСтрока 3 🙂".as_bytes().to_vec(),
        vec![0x00; 4096],
        vec![0xFF, 0xFE, 0x00, 0x0A],
        (0..=255u8).cycle().take(200_000).collect(),
    ];
    for (i, data) in cases.into_iter().enumerate() {
        let a = sub.submit_task(byte_task(i as u64, None, Some(data.clone())));
        let b = local.submit_task(byte_task(i as u64, None, Some(data)));
        assert_same_result(&a, &b);
        assert!(
            !a.error.as_deref().unwrap_or("").contains("аварийно"),
            "случай {i}: {:?}",
            a.error
        );
    }
}

/// Текст с переводами строк и UTF-8 приходит ровно таким, каким ушёл.
#[test]
fn test_worker_binary_text_is_exact() {
    let pool = sub_pool(20);
    let text = "Первая\nВторая\r\nТретья 日本語 😀\n\nПоследняя";
    let res = pool.submit_task(byte_task(1, None, Some(text.as_bytes().to_vec())));
    assert_eq!(res.status, "text", "{:?}", res.error);
    assert_eq!(res.text, text);
}

/// Пустые данные (0 байт) — не ошибка протокола; результат как при разборе в процессе.
#[test]
fn test_worker_empty_data() {
    let sub = sub_pool(20);
    let local = WorkerPool::new(
        None,
        1,
        Duration::from_secs(20),
        true,
        ExtractConfig::default(),
    );
    let a = sub.submit_task(byte_task(1, None, Some(Vec::new())));
    let b = local.submit_task(byte_task(1, None, Some(Vec::new())));
    assert_same_result(&a, &b);
    // Следующее задание идёт в тот же процесс и читается с верной границы
    let next = sub.submit_task(byte_task(2, None, Some(b"after empty".to_vec())));
    assert_eq!(next.text, "after empty");
}

/// Задание без данных (только путь): рабочий читает файл сам.
#[test]
fn test_worker_path_only_task() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("only_path.txt");
    std::fs::write(&file, "Текст из файла на диске\n").unwrap();
    let pool = sub_pool(20);
    let mut task = byte_task(5, Some(file.clone()), None);
    task.size = std::fs::metadata(&file).unwrap().len();
    let res = pool.submit_task(task);
    assert_eq!(res.status, "text", "{:?}", res.error);
    assert!(res.text.contains("Текст из файла на диске"));
}

/// Крупные данные (5 МБ) и серия заданий разного размера подряд через один процесс.
#[test]
fn test_worker_large_and_mixed_sizes() {
    let pool = sub_pool(30);
    for (i, len) in [10usize, 70_000, 5 * 1024 * 1024, 3, 65_536, 65_535]
        .into_iter()
        .enumerate()
    {
        let data = vec![b'a'; len];
        let res = pool.submit_task(byte_task(i as u64, None, Some(data)));
        assert_eq!(res.status, "text", "len={len}: {:?}", res.error);
        assert!(res.text.starts_with("aaa"), "len={len}");
    }
}

/// Падение рабочего процесса при крупных данных: ошибка, затем процесс перезапущен и
/// следующие задания обрабатываются.
#[test]
fn test_worker_restart_after_crash_with_large_data() {
    let pool = sub_pool(20);
    let mut crash = byte_task(1, None, Some(vec![b'z'; 3 * 1024 * 1024]));
    crash.test_hook = Some("abort".to_string());
    let res = pool.submit_task(crash);
    assert_eq!(res.status, "failed");
    assert!(res.error.as_deref().unwrap_or("").contains("аварийно"));

    for i in 0..3u64 {
        let mut data = tricky_bytes();
        data.extend(b"tail");
        let ok = pool.submit_task(byte_task(10 + i, None, Some(data)));
        assert!(
            !ok.error.as_deref().unwrap_or("").contains("аварийно"),
            "{:?}",
            ok.error
        );
    }
    let text = pool.submit_task(byte_task(20, None, Some(b"restarted ok".to_vec())));
    assert_eq!(text.text, "restarted ok");
}

/// Запускает `--worker`, пишет в его stdin `input`, закрывает канал и ждёт весь stdout
/// не дольше 15 с. Зависание — паника.
fn run_raw_worker(input: &[u8]) -> (String, std::process::ExitStatus) {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let mut child = Command::new(find_worker_exe())
        .arg("--worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(input).unwrap();
    drop(stdin); // конец потока
    let mut out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut s = String::new();
        let _ = out.read_to_string(&mut s);
        let _ = tx.send(s);
    });
    match rx.recv_timeout(Duration::from_secs(15)) {
        Ok(reply) => {
            let status = child.wait().unwrap();
            (reply, status)
        }
        Err(_) => {
            let _ = child.kill();
            panic!("рабочий процесс завис");
        }
    }
}

/// Оборванный поток: заголовок обещает больше байтов, чем пришло, и канал закрыт.
/// Рабочий процесс не зависает: отвечает ошибкой задания и завершается.
#[test]
fn test_worker_truncated_stream_is_error_not_hang() {
    let mut wire = Vec::new();
    poiskovichek::worker::write_task(&mut wire, &byte_task(77, None, Some(vec![b'q'; 1000])))
        .unwrap();
    wire.truncate(wire.len() - 400);

    let (reply, status) = run_raw_worker(&wire);
    assert!(status.success(), "выход: {status:?}");
    let lines: Vec<&str> = reply.lines().collect();
    assert_eq!(lines.len(), 1, "{reply:?}");
    let resp: poiskovichek::worker::WorkerResponse = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(resp.extracted.status, "failed");
    assert!(
        resp.extracted
            .error
            .as_deref()
            .unwrap_or("")
            .contains("оборванный поток"),
        "{:?}",
        resp.extracted.error
    );
}

/// Целое задание, затем оборванное: на первое есть нормальный ответ, на второе — ошибка.
#[test]
fn test_worker_good_task_then_truncated_one() {
    let mut wire = Vec::new();
    poiskovichek::worker::write_task(&mut wire, &byte_task(1, None, Some(b"good one".to_vec())))
        .unwrap();
    let mut second = Vec::new();
    poiskovichek::worker::write_task(&mut second, &byte_task(2, None, Some(vec![b'q'; 500])))
        .unwrap();
    second.truncate(second.len() - 1);
    wire.extend(second);

    let (reply, _) = run_raw_worker(&wire);
    let lines: Vec<&str> = reply.lines().collect();
    assert_eq!(lines.len(), 2, "{reply:?}");
    let first: poiskovichek::worker::WorkerResponse = serde_json::from_str(lines[0]).unwrap();
    assert_eq!((first.id, first.extracted.text.as_str()), (1, "good one"));
    let second: poiskovichek::worker::WorkerResponse = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second.extracted.status, "failed");
}

/// data_len выше предела в заголовке: ошибка без выделения памяти и без зависания.
#[test]
fn test_worker_huge_data_len_in_header_is_error() {
    let mut wire = Vec::new();
    poiskovichek::worker::write_task(&mut wire, &byte_task(1, None, None)).unwrap();
    let text = String::from_utf8(wire)
        .unwrap()
        .replace("\"data_len\":0", "\"data_len\":99999999999");
    let (reply, status) = run_raw_worker(text.as_bytes());
    assert!(status.success());
    assert!(
        reply.contains("failed") && reply.contains("предела"),
        "{reply}"
    );
}

/// Мусор вместо заголовка: ошибка и выход, без зависания.
#[test]
fn test_worker_garbage_header_is_error_not_hang() {
    let (reply, _) = run_raw_worker(b"\xFF\x00 not a header\n12345");
    assert!(reply.contains("failed"), "{reply}");
}

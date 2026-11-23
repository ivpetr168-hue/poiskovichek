use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, bounded};
use tempfile::tempdir;

use poiskovichek::pipeline::{PipelineOptions, Progress, sync_all_with_options};
use poiskovichek::settings::{Settings, Source, SourceMode};

fn settings_with_sources(data_dir: &Path, dirs: &[&Path]) -> Settings {
    Settings {
        data_dir: data_dir.to_path_buf(),
        enable_morphology: false,
        sources: dirs
            .iter()
            .map(|d| Source {
                path: d.to_path_buf(),
                mode: SourceMode::Full,
                max_depth: 0,
                enabled: true,
                volume_serial: None,
                extra_excludes: Vec::new(),
            })
            .collect(),
        ..Default::default()
    }
}

fn make_files(dir: &Path, prefix: &str, count: usize) {
    fs::create_dir_all(dir).expect("create dir");
    for i in 0..count {
        fs::write(
            dir.join(format!("{prefix}_{i:04}.txt")),
            format!("Документ {i:04} источника {prefix}: текст для полосы хода индексирования."),
        )
        .expect("write file");
    }
}

fn start_collector(rx: Receiver<Progress>) -> Arc<Mutex<Vec<Progress>>> {
    let collected = Arc::new(Mutex::new(Vec::new()));
    let target = Arc::clone(&collected);
    std::thread::spawn(move || {
        while let Ok(p) = rx.recv() {
            target.lock().unwrap().push(p);
        }
    });
    collected
}

fn wait_last(collected: &Arc<Mutex<Vec<Progress>>>) -> Progress {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(p) = collected
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|p| p.finished_summary.is_some())
            .cloned()
        {
            return p;
        }
        assert!(
            Instant::now() < deadline,
            "не дождались итогового сообщения прогресса"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn assert_russian_phase(phase: &str) {
    assert!(
        !phase.chars().any(|c| c.is_ascii_alphabetic()),
        "название этапа должно быть русским, без латиницы: {phase}"
    );
}

/// Два источника по 300 и 500 файлов: у каждого в итоге обработано столько же, сколько найдено.
#[test]
fn test_progress_two_sources_reach_full_count() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let first = dir.path().join("источник-а");
    let second = dir.path().join("источник-б");
    make_files(&first, "a", 300);
    make_files(&second, "b", 500);

    let settings = settings_with_sources(&data_dir, &[&first, &second]);
    let (tx, rx) = bounded::<Progress>(4);
    let collected = start_collector(rx);

    let options = PipelineOptions {
        queue_capacity: 4,
        in_process_workers: true,
        progress_tx: Some(tx),
        ..Default::default()
    };

    let start = Instant::now();
    let summary = sync_all_with_options(&settings, options).expect("sync");
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "конвейер с очередями ёмкостью 4 не должен зависать: {:?}",
        start.elapsed()
    );
    assert_eq!(summary.added, 800);

    let last = wait_last(&collected);
    assert_eq!(last.phase, "готово");
    assert_eq!(last.sources.len(), 2);
    assert_eq!(last.sources[0].found, 300);
    assert_eq!(last.sources[1].found, 500);
    assert_eq!(last.sources[0].label, "источник-а");
    assert_eq!(last.sources[1].label, "источник-б");
    for src in &last.sources {
        assert!(
            src.list_done,
            "список источника {} должен быть составлен",
            src.label
        );
        assert_eq!(
            src.done, src.found,
            "обработано должно быть столько же, сколько найдено"
        );
        assert!(src.finished);
    }
    assert_eq!(last.total_found, 800);
    assert_eq!(last.total_done, 800);
    assert!(last.finished_summary.is_some());

    for p in collected.lock().unwrap().iter() {
        assert_russian_phase(&p.phase);
    }
}

/// Повторная сверка без изменений: неизменённые файлы тоже считаются обработанными.
#[test]
fn test_progress_resync_counts_unchanged_files() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let first = dir.path().join("один");
    let second = dir.path().join("два");
    make_files(&first, "one", 40);
    make_files(&second, "two", 60);

    let settings = settings_with_sources(&data_dir, &[&first, &second]);

    sync_all_with_options(
        &settings,
        PipelineOptions {
            in_process_workers: true,
            ..Default::default()
        },
    )
    .expect("первая сверка");

    let (tx, rx) = bounded::<Progress>(4);
    let collected = start_collector(rx);
    sync_all_with_options(
        &settings,
        PipelineOptions {
            in_process_workers: true,
            progress_tx: Some(tx),
            ..Default::default()
        },
    )
    .expect("повторная сверка");

    let last = wait_last(&collected);
    assert_eq!(last.phase, "готово");
    for src in &last.sources {
        assert!(src.list_done);
        assert_eq!(
            src.done, src.found,
            "неизменённые файлы должны считаться решёнными"
        );
    }
    assert_eq!(last.total_done, 100);
}

/// Скорость: при индексировании текстовых файлов байты в секунду больше нуля.
#[test]
fn test_progress_speed_positive_for_text_files() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path().join("data");
    let source = dir.path().join("тексты");
    make_files(&source, "doc", 60);

    let settings = settings_with_sources(&data_dir, &[&source]);
    let (tx, rx) = bounded::<Progress>(8);
    let collected = start_collector(rx);

    sync_all_with_options(
        &settings,
        PipelineOptions {
            in_process_workers: true,
            progress_tx: Some(tx),
            ..Default::default()
        },
    )
    .expect("sync");

    let last = wait_last(&collected);
    assert!(
        last.bytes_per_sec > 0,
        "скорость чтения должна быть больше нуля"
    );
    assert!(
        last.files_per_sec > 0.0,
        "скорость файлов должна быть больше нуля"
    );
    assert_eq!(last.total_found, 60);
    assert_eq!(last.total_done, 60);
}

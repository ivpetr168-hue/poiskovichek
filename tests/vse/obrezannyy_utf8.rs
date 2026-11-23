//! №115: большой .txt читается не целиком (предел × 4 байт); граница чтения может попасть
//! в середину русской буквы UTF-8 — текст всё равно должен быть UTF-8, а не CP1251.

use std::fs;
use std::path::Path;

use tempfile::tempdir;

use poiskovichek::extract::{ExtractConfig, extract_file};
use poiskovichek::index::reader;
use poiskovichek::pipeline::{PipelineOptions, sync_all_with_options};
use poiskovichek::settings::{Settings, Source, SourceMode};
use tantivy::query::QueryParser;

const PREDEL_SIMVOLOV: usize = 1001;

/// Файл > 1 МБ: слово в начале, дальше только «ж» (2 байта); граница чтения
/// `PREDEL_SIMVOLOV * 4` попадает точно в середину буквы.
fn bolshoy_tekst() -> Vec<u8> {
    let mut bayty = "мурмелиада ".as_bytes().to_vec();
    if bayty.len().is_multiple_of(2) {
        bayty.push(b' ');
    }
    while bayty.len() < 1_100_000 {
        bayty.extend_from_slice("ж".as_bytes());
    }
    let granica = PREDEL_SIMVOLOV * 4;
    let oshibka = std::str::from_utf8(&bayty[..granica]).unwrap_err();
    assert!(
        oshibka.error_len().is_none(),
        "граница должна резать букву пополам"
    );
    bayty
}

#[test]
fn izvlechenie_bolshogo_txt_s_razrezannoy_bukvoy() {
    let dir = tempdir().unwrap();
    let put = dir.path().join("bolshoy.txt");
    fs::write(&put, bolshoy_tekst()).unwrap();

    let config = ExtractConfig {
        large_txt_threshold_mb: 1,
        limit_chars_large_txt: PREDEL_SIMVOLOV,
        ..ExtractConfig::default()
    };
    let res = extract_file(&put, false, &config);
    assert_eq!(res.status, "text");
    assert!(res.text.starts_with("мурмелиада "), "{:.40}", res.text);
    assert!(!res.text.contains('\u{fffd}'));
    assert!(res.text.chars().skip(11).all(|c| c == 'ж' || c == ' '));
}

fn nastroyki(data_dir: &Path, source_dir: &Path) -> Settings {
    Settings {
        data_dir: data_dir.to_path_buf(),
        enable_morphology: false,
        large_txt_threshold_mb: 1,
        limit_chars_large_txt: PREDEL_SIMVOLOV,
        sources: vec![Source {
            path: source_dir.to_path_buf(),
            mode: SourceMode::Full,
            max_depth: 0,
            enabled: true,
            volume_serial: None,
            extra_excludes: Vec::new(),
        }],
        ..Default::default()
    }
}

#[test]
fn bolshoy_txt_indeksiruetsya_i_nakhoditsya_poiskom() {
    let dir = tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).unwrap();
    fs::write(source_dir.join("bolshoy.txt"), bolshoy_tekst()).unwrap();

    let settings = nastroyki(&data_dir, &source_dir);
    let options = PipelineOptions {
        in_process_workers: true,
        ..Default::default()
    };
    sync_all_with_options(&settings, options).expect("sync");

    let rdr = reader(&data_dir.join("index"), None).expect("reader");
    let searcher = rdr.searcher();
    let body = searcher.schema().get_field("body").expect("body");
    let parser = QueryParser::for_index(searcher.index(), vec![body]);
    let q = parser.parse_query("мурмелиада").expect("parse");
    let count = searcher
        .search(&q, &tantivy::collector::Count)
        .expect("search");
    assert_eq!(count, 1, "слово из начала большого .txt должно находиться");
}

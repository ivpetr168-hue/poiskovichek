//! Единая программа интеграционных тестов (R1-№121): все прежние файлы tests\*.rs
//! собираются и линкуются один раз, а не отдельной программой на каждый файл.
//!
//! Отдельными программами остались:
//! - tests\settings.rs: тест меняет текущую папку процесса (env::set_current_dir),
//!   а тест app::test_no_external_process_invocation_in_src читает `src` по относительному
//!   пути, поэтому вместе в одном процессе они гонялись бы друг с другом;
//! - tests\zamer_diskov.rs: использует env!("CARGO_BIN_EXE_zamer_diskov") и запускает
//!   собранную программу как внешний процесс.
//!
//! Имена тестов не менялись, добавился только префикс модуля: `catalog::perf_snapshot`.

mod app;
mod autostart;
mod catalog;
mod diskinfo;
mod drives;
mod duplicates;
mod export;
mod extract;
mod index;
mod morph;
mod names;
mod obnovlenie;
mod obrezannyy_utf8;
mod phrase;
mod pipeline;
mod po_diskam;
mod povtor_zapisi;
mod progress;
mod query;
mod relocate;
mod schetchiki;
mod search;
mod settings_effects;
mod single;
mod snippet;
mod sync;
mod walk;
mod worker;

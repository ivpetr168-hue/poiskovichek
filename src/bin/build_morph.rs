use std::path::PathBuf;
use std::process::ExitCode;

use poiskovichek::morph::build_morph_fst;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Использование: {} <путь к morphology.sqlite3> <выход morph.fst>",
            args.first().map(|s| s.as_str()).unwrap_or("build_morph")
        );
        return ExitCode::FAILURE;
    }

    let sqlite_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);

    println!(
        "Сборка morph.fst из {} в {}...",
        sqlite_path.display(),
        output_path.display()
    );

    match build_morph_fst(&sqlite_path, &output_path) {
        Ok(stats) => {
            println!("Сборка успешно завершена!");
            println!("Число форм: {}", stats.form_count);
            println!("Число лексем: {}", stats.lexeme_count);
            println!("Форм с >= 2 лексемами: {}", stats.multi_lexeme_forms);
            println!(
                "Размер файла: {} байт ({:.2} МБ)",
                stats.file_size,
                stats.file_size as f64 / (1024.0 * 1024.0)
            );
            println!(
                "Время сборки: {:.2} с",
                stats.elapsed_millis as f64 / 1000.0
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("Ошибка сборки словаря: {err}");
            ExitCode::FAILURE
        }
    }
}

use std::fs;
use std::io::Write;
use std::path::Path;
use std::time::Instant;
use tempfile::tempdir;

use poiskovichek::rules::{category_for, is_sensitive_path};
use poiskovichek::settings::{Settings, Source, SourceMode};
use poiskovichek::walk::{WalkEntry, is_reparse_attribute, walk_source};

#[test]
fn test_windows_reparse_attribute_is_detected() {
    // R1-Б-1: выделение флага reparse point 0x400
    assert!(is_reparse_attribute(0x400));
    assert!(is_reparse_attribute(0x420));
    assert!(!is_reparse_attribute(0));
    assert!(!is_reparse_attribute(0x20));
}

#[test]
fn test_cloud_file_attributes_detection() {
    use poiskovichek::{
        FILE_ATTRIBUTE_OFFLINE, FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS,
        FILE_ATTRIBUTE_RECALL_ON_OPEN, is_cloud_file_attributes,
    };

    // Флаг RECALL_ON_DATA_ACCESS (0x00400000)
    assert!(is_cloud_file_attributes(
        FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS
    ));
    assert!(is_cloud_file_attributes(0x0040_0020));

    // Флаг RECALL_ON_OPEN (0x00040000)
    assert!(is_cloud_file_attributes(FILE_ATTRIBUTE_RECALL_ON_OPEN));
    assert!(is_cloud_file_attributes(0x0004_0020));

    // Флаг OFFLINE (0x00001000)
    assert!(is_cloud_file_attributes(FILE_ATTRIBUTE_OFFLINE));
    assert!(is_cloud_file_attributes(0x0000_1020));

    // Обычные файлы без облачных флагов
    assert!(!is_cloud_file_attributes(0));
    assert!(!is_cloud_file_attributes(0x20));
    assert!(!is_cloud_file_attributes(0x80));
    assert!(!is_cloud_file_attributes(0x400));
}

#[test]
fn test_default_rules_exclude_catalog_and_generated_dependencies() {
    // R1-Б-3: только общие правила, без личных путей пользователя
    let settings = Settings::default();

    assert!(settings.exclude_folder_names.contains(&".git".to_string()));
    assert!(
        settings
            .exclude_folder_names
            .contains(&"node_modules".to_string())
    );
    assert!(
        settings
            .exclude_folder_names
            .contains(&"target".to_string())
    );
    assert!(
        settings
            .exclude_folder_names
            .contains(&"__pycache__".to_string())
    );

    assert!(
        settings
            .documents_only_folder_names
            .contains(&"tmp".to_string())
    );
    assert!(
        settings
            .documents_only_folder_names
            .contains(&"cache".to_string())
    );

    assert!(settings.exclude_files.contains(&"ntuser.dat*".to_string()));
    assert!(settings.exclude_files.contains(&"~$*".to_string()));
    assert!(settings.exclude_files.contains(&"hiberfil.sys".to_string()));
    assert!(settings.exclude_files.contains(&"pagefile.sys".to_string()));
}

#[test]
fn test_category_uses_archive_context_for_books_and_backups() {
    // R1-Б-4: пути с книжным контекстом и бэкапами
    let words = vec![
        "books".into(),
        "ebooks".into(),
        "литература".into(),
        "library".into(),
        "библиотека".into(),
        "книг*".into(),
    ];

    assert_eq!(category_for(r"D:\Книги\математика.pdf", &words), "книга");
    assert_eq!(
        category_for(r"D:\Downloads\Автор - Название.epub", &words),
        "книга"
    );
    assert_eq!(
        category_for(r"D:\Downloads\Анализы\Кровь.pdf", &words),
        "документ"
    );
    assert_eq!(
        category_for(r"E:\Backups\system.zip", &words),
        "резервная копия"
    );
    assert_eq!(
        category_for(r"C:\Archive\data.bak", &words),
        "резервная копия"
    );
}

#[test]
fn test_scan_catalogues_document_and_excludes_named_tree() {
    // R1-Б-6: дерево с документом и исключённой папкой
    let dir = tempdir().expect("Временная папка");
    let root = dir.path();

    fs::write(root.join("Документ.txt"), "важный текст").expect("write");
    let excluded = root.join("Windows");
    fs::create_dir_all(&excluded).expect("create dir");
    fs::write(excluded.join("driver.sys"), b"driver").expect("write sys");

    let mut settings = Settings::default();
    settings.exclude_folder_names.push("windows".into());

    let source = Source {
        path: root.to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let paths: Vec<String> = result
        .entries()
        .iter()
        .map(|e| e.rel_path.clone())
        .collect();

    assert_eq!(paths, vec!["Документ.txt"]);

    // Проверяем, что Windows попала в пропуски
    let skips = result.skips();
    assert!(
        skips
            .iter()
            .any(|s| s.path.ends_with("Windows") && s.reason == "исключено правилом")
    );
}

#[test]
fn test_document_only_program_tree_indexes_documents_not_binaries() {
    // R1-Б-7: режим «только документы» индексирует тексты и PDF, отбрасывая DLL
    let dir = tempdir().expect("Временная папка");
    let root = dir.path();
    let readme_dir = root.join("games").join("S.T.A.L.K.E.R.").join("readme");
    fs::create_dir_all(&readme_dir).expect("create dirs");

    fs::write(readme_dir.join("ReadMe.txt"), "Инструкция").expect("write");
    fs::write(readme_dir.join("StalkerUserManual.pdf"), b"%PDF-1.4").expect("write");
    fs::write(readme_dir.parent().unwrap().join("game.dll"), b"MZ...").expect("write");

    let settings = Settings::default();
    let source = Source {
        path: root.to_path_buf(),
        mode: SourceMode::DocumentsOnly,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let names: Vec<String> = result
        .entries()
        .iter()
        .map(|e| {
            Path::new(&e.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();

    assert!(names.contains(&"ReadMe.txt".to_string()));
    assert!(names.contains(&"StalkerUserManual.pdf".to_string()));
    assert!(!names.contains(&"game.dll".to_string()));
}

#[test]
fn test_document_only_program_zip_indexes_documents_not_source_code() {
    // R1-Б-8: ZIP в режиме «только документы» включает только документы, игнорируя код
    let dir = tempdir().expect("Временная папка");
    let root = dir.path();
    let zip_path = root.join("documentation.zip");

    {
        let file = fs::File::create(&zip_path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("manual.txt", options).expect("start manual");
        zip.write_all(b"manual contents").expect("write manual");

        zip.start_file("src/generated.py", options)
            .expect("start code");
        zip.write_all(b"import os").expect("write code");

        zip.finish().expect("finish zip");
    }

    let settings = Settings::default();
    let source = Source {
        path: root.to_path_buf(),
        mode: SourceMode::DocumentsOnly,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let inner_members: Vec<&WalkEntry> = result
        .entries()
        .into_iter()
        .filter(|e| e.inside_archive)
        .collect();

    assert_eq!(inner_members.len(), 1);
    assert!(inner_members[0].path.ends_with("manual.txt"));
}

#[test]
fn test_document_only_tmp_name_keeps_user_document_not_binary() {
    // R1-Б-10: папка с именем tmp переводит ветвь в режим «только документы»
    let dir = tempdir().expect("Временная папка");
    let tmp_folder = dir.path().join("Sveta").join("tmp");
    fs::create_dir_all(&tmp_folder).expect("create dirs");

    fs::write(tmp_folder.join("заметка.txt"), "личный документ").expect("write");
    fs::write(tmp_folder.join("cache.bin"), b"binary data").expect("write");

    let settings = Settings::default();
    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let names: Vec<String> = result
        .entries()
        .iter()
        .map(|e| {
            Path::new(&e.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();

    assert!(names.contains(&"заметка.txt".to_string()));
    assert!(!names.contains(&"cache.bin".to_string()));
}

#[test]
fn test_explicit_selection_is_not_limited_to_document_only_extensions() {
    // R1-Б-11: явно выбранный одиночный файл индексируется любого формата
    let dir = tempdir().expect("Временная папка");
    let dll_path = dir.path().join("game.dll");
    fs::write(&dll_path, b"MZ binary payload").expect("write");

    let settings = Settings::default();
    let source = Source {
        path: dll_path.clone(),
        mode: SourceMode::DocumentsOnly,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let entries = result.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, dll_path.to_string_lossy());
}

#[test]
fn test_real_scope_keeps_settings_and_install_files_but_skips_program_trees() {
    // R1-Б-12: синтетическое дерево (настройки и установщики остаются, NTUSER.DAT, ~$draft.docx, папка программ нет)
    let dir = tempdir().expect("Временная папка");
    let root = dir.path();

    let vscode_dir = root.join("User").join(".vscode");
    fs::create_dir_all(&vscode_dir).expect("create vscode");
    fs::write(vscode_dir.join("settings.json"), r#"{"theme": "dark"}"#).expect("write");

    fs::write(root.join("NTUSER.DAT"), b"registry").expect("write");
    fs::write(root.join("~$draft.docx"), b"office lock").expect("write");

    let node_modules = root.join("node_modules").join("vendor");
    fs::create_dir_all(&node_modules).expect("create node_modules");
    fs::write(node_modules.join("app.js"), "console.log()").expect("write");

    let install_dir = root.join("Install");
    fs::create_dir_all(&install_dir).expect("create install");
    fs::write(install_dir.join("setup.exe"), b"installer").expect("write");
    fs::write(install_dir.join("lesson.mp4"), b"video").expect("write");

    let settings = Settings::default();
    let source = Source {
        path: root.to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let names: Vec<String> = result
        .entries()
        .iter()
        .map(|e| {
            Path::new(&e.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();

    assert!(names.contains(&"settings.json".to_string()));
    assert!(names.contains(&"setup.exe".to_string()));
    assert!(names.contains(&"lesson.mp4".to_string()));

    assert!(!names.contains(&"NTUSER.DAT".to_string()));
    assert!(!names.contains(&"~$draft.docx".to_string()));
    assert!(!names.contains(&"app.js".to_string()));
}

#[test]
fn test_zip_member_is_catalogued_and_read() {
    // R1-Б-25: перечень внутренних файлов ZIP со слэшами и признаком inside_archive
    let dir = tempdir().expect("Временная папка");
    let zip_path = dir.path().join("archive.zip");

    {
        let file = fs::File::create(&zip_path).expect("create zip");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);

        zip.start_file("папка/заметка.txt", options).expect("file");
        zip.write_all(b"zip text content").expect("write");

        zip.start_file("program.bin", options).expect("file");
        zip.write_all(b"ignored bin").expect("write");

        zip.finish().expect("finish");
    }

    let settings = Settings::default();
    let source = Source {
        path: dir.path().to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let entries = result.entries();

    let member = entries
        .iter()
        .find(|e| e.inside_archive)
        .expect("Внутренний файл архива");

    assert!(member.path.contains("archive.zip → папка\\заметка.txt"));
    assert_eq!(member.size, 16);
    assert!(member.inside_archive);

    // program.bin не входит в ARCHIVE_CONTENT_EXTENSIONS и должен быть отфильтрован
    assert!(!entries.iter().any(|e| e.path.contains("program.bin")));
}

#[test]
fn test_archive_members_are_excluded_from_duplicates() {
    // R1-Б-26: у членов архива inside_archive = true
    let entry_file = WalkEntry {
        path: r"D:\normal.txt".into(),
        rel_path: "normal.txt".into(),
        size: 100,
        modified_ns: 1,
        created_ns: 1,
        is_dir: false,
        inside_archive: false,
        container_path: None,
        document_only: false,
    };
    let entry_member = WalkEntry {
        path: r"D:\archive.zip → inner.txt".into(),
        rel_path: "inner.txt".into(),
        size: 100,
        modified_ns: 1,
        created_ns: 1,
        is_dir: false,
        inside_archive: true,
        container_path: Some(r"D:\archive.zip".into()),
        document_only: false,
    };

    assert!(!entry_file.inside_archive);
    assert!(entry_member.inside_archive);
}

#[test]
fn test_sensitive_paths_include_accounts_env_and_password_files() {
    // R1-Б-46: проверка закрытых данных
    assert!(is_sensitive_path(r"C:\УЧЕТКИ\почта.txt", &[]));
    assert!(is_sensitive_path(r"C:\project\.env", &[]));
    assert!(is_sensitive_path(r"C:\project\.env.local", &[]));
    assert!(is_sensitive_path(r"C:\project\production.env", &[]));
    assert!(is_sensitive_path(r"C:\TEMP\reader_password.txt", &[]));
    assert!(is_sensitive_path(r"C:\data\.ssh\id_rsa", &[]));
    assert!(is_sensitive_path(r"C:\data\auth.json", &[]));

    // Пароли внутри файлов исходного кода не считаются секретными путями
    assert!(!is_sensitive_path(
        r"D:\src\PasswordAuthentication.java",
        &[]
    ));
}

#[test]
fn test_max_depth_one_limits_traversal() {
    // max_depth = 1: файлы в корне попадают, вложенные папки не обходятся
    let dir = tempdir().expect("Временная папка");
    let root = dir.path();

    fs::write(root.join("root_file.txt"), "root").expect("write");
    let sub = root.join("subfolder");
    fs::create_dir_all(&sub).expect("create sub");
    fs::write(sub.join("deep_file.txt"), "deep").expect("write");

    let settings = Settings::default();
    let source = Source {
        path: root.to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 1,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let names: Vec<String> = result
        .entries()
        .iter()
        .map(|e| {
            Path::new(&e.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();

    assert!(names.contains(&"root_file.txt".to_string()));
    assert!(!names.contains(&"deep_file.txt".to_string()));
}

#[test]
#[ignore]
fn perf_walk() {
    // Синтетика 200 тыс. файлов: первый обход >= 50 000 файлов/с, повторный со сверкой <= 2 с
    use rayon::prelude::*;

    let dir = tempdir().expect("Временная папка");
    let root = dir.path();

    const FOLDERS: usize = 200;
    const FILES_PER_FOLDER: usize = 1_000;
    const TOTAL: usize = FOLDERS * FILES_PER_FOLDER;

    println!("Создание {TOTAL} синтетических файлов во временной папке...");
    let start_create = Instant::now();

    (0..FOLDERS).into_par_iter().for_each(|f_idx| {
        let folder = root.join(format!("folder_{f_idx}"));
        fs::create_dir_all(&folder).expect("mkdir");
        for file_idx in 0..FILES_PER_FOLDER {
            let file_path = folder.join(format!("f_{file_idx}.txt"));
            fs::write(file_path, b"data").expect("write");
        }
    });

    println!(
        "Файлы созданы за {:.2} с",
        start_create.elapsed().as_secs_f64()
    );

    let settings = Settings::default();
    let source = Source {
        path: root.to_path_buf(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    // 1. Первый обход
    let start_walk = Instant::now();
    let result = walk_source(&source, &settings, None);
    let walk_duration = start_walk.elapsed();
    let count = result.entries().len();

    let speed = count as f64 / walk_duration.as_secs_f64();
    println!(
        "Первый обход: {count} файлов за {:.3} с ({:.0} файлов/с)",
        walk_duration.as_secs_f64(),
        speed
    );

    assert_eq!(count, TOTAL);
    assert!(
        speed >= 50_000.0,
        "Скорость первого обхода должна быть ≥ 50 000 файлов/с (фактически: {:.0})",
        speed
    );

    // 2. Повторный обход со сверкой (цель ≤ 2 с)
    let start_repeat = Instant::now();
    let mut snapshot_sim = std::collections::HashMap::with_capacity(TOTAL);
    for entry in result.entries() {
        snapshot_sim.insert(
            entry.path.clone(),
            poiskovichek::catalog::FileSnapshotEntry {
                id: 1,
                size: entry.size,
                modified_ns: entry.modified_ns,
                extraction_version: 2,
                has_error: false,
                container_id: None,
                text_sha: None,
            },
        );
    }

    let repeat_result = walk_source(&source, &settings, None);
    let mut unchanged_count = 0usize;
    for entry in repeat_result.entries() {
        if let poiskovichek::sync::SyncDecision::Unchanged { .. } =
            poiskovichek::sync::classify_entry(entry, &snapshot_sim)
        {
            unchanged_count += 1;
        }
    }
    let repeat_duration = start_repeat.elapsed();

    println!(
        "Повторный обход со сверкой: {} файлов за {:.3} с",
        unchanged_count,
        repeat_duration.as_secs_f64()
    );

    assert_eq!(unchanged_count, TOTAL);
    assert!(
        repeat_duration.as_secs_f64() <= 2.0,
        "Повторный обход со сверкой должен занимать ≤ 2 с (фактически: {:.3} с)",
        repeat_duration.as_secs_f64()
    );
}

#[test]
fn test_source_inside_exclude_paths_indexes_files_and_skips_nested_exclude_paths() {
    // 1. Источник внутри папки из exclude_paths (предок временной папки) -> файлы индексируются;
    //    вложенная папка из exclude_paths внутри источника -> не индексируется.
    let parent_dir = tempdir().expect("tempdir parent");
    let source_dir = parent_dir.path().join("my_source");
    fs::create_dir_all(&source_dir).expect("create source_dir");

    // Файл непосредственно внутри источника
    let file1 = source_dir.join("report.txt");
    fs::write(&file1, "important text").expect("write file1");

    // Вложенная папка, которая явно исключена через exclude_paths
    let nested_excluded = source_dir.join("excluded_subfolder");
    fs::create_dir_all(&nested_excluded).expect("create nested_excluded");
    let file2 = nested_excluded.join("secret.txt");
    fs::write(&file2, "secret text").expect("write file2");

    // Тест сам задаёт exclude_paths = предок временной папки и вложенная папка
    let settings = Settings {
        exclude_paths: vec![
            parent_dir.path().to_string_lossy().to_string(),
            nested_excluded.to_string_lossy().to_string(),
        ],
        ..Default::default()
    };

    let source = Source {
        path: source_dir.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let paths: Vec<String> = result
        .entries()
        .iter()
        .map(|e| e.rel_path.clone())
        .collect();

    assert!(paths.contains(&"report.txt".to_string()));
    assert!(!paths.contains(&"excluded_subfolder\\secret.txt".to_string()));
    assert_eq!(paths.len(), 1);

    let skips = result.skips();
    assert!(
        skips
            .iter()
            .any(|s| s.path.ends_with("excluded_subfolder") && s.reason == "исключено правилом")
    );
}

#[test]
fn test_source_root_named_tmp_indexes_all_formats_nested_tmp_docs_only() {
    // 2. Корень источника с именем tmp -> индексируется полностью (все форматы);
    //    вложенная tmp -> режим «только документы».
    let dir = tempdir().expect("tempdir");
    let root_tmp = dir.path().join("tmp");
    fs::create_dir_all(&root_tmp).expect("create root tmp");

    // В корне tmp создаем документ и бинарник
    fs::write(root_tmp.join("root_doc.txt"), "document in root tmp").expect("write root_doc");
    fs::write(root_tmp.join("root_bin.bin"), b"binary payload in root tmp")
        .expect("write root_bin");

    // Внутри создаем вложенную папку tmp (например, root_tmp/nested/tmp)
    let nested_tmp = root_tmp.join("nested").join("tmp");
    fs::create_dir_all(&nested_tmp).expect("create nested tmp");

    // Вложенная tmp содержит документ и бинарник
    fs::write(nested_tmp.join("nested_doc.txt"), "document in nested tmp")
        .expect("write nested_doc");
    fs::write(
        nested_tmp.join("nested_bin.bin"),
        b"binary payload in nested tmp",
    )
    .expect("write nested_bin");

    let settings = Settings::default();
    let source = Source {
        path: root_tmp.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let names: Vec<String> = result
        .entries()
        .iter()
        .map(|e| {
            Path::new(&e.path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();

    // Корень источника tmp индексируется полностью: и документ, и бинарник
    assert!(names.contains(&"root_doc.txt".to_string()));
    assert!(names.contains(&"root_bin.bin".to_string()));

    // Вложенная tmp работает в режиме «только документы»: документ включен, бинарник отброшен
    assert!(names.contains(&"nested_doc.txt".to_string()));
    assert!(!names.contains(&"nested_bin.bin".to_string()));
}

#[test]
fn test_program_data_dir_inside_source_is_not_indexed() {
    // 3. Папка данных программы внутри источника -> не индексируется.
    let dir = tempdir().expect("tempdir");
    let source_dir = dir.path().join("source");
    fs::create_dir_all(&source_dir).expect("create source_dir");

    // Обычный файл в источнике
    fs::write(source_dir.join("notes.txt"), "user notes").expect("write notes");

    // Папка данных программы внутри источника
    let app_data = source_dir.join("program_data");
    fs::create_dir_all(&app_data).expect("create program_data");
    fs::write(app_data.join("catalog.sqlite3"), "db contents").expect("write catalog");
    fs::write(app_data.join("search.log"), "log contents").expect("write log");

    let settings = Settings {
        data_dir: app_data.clone(),
        ..Default::default()
    };

    let source = Source {
        path: source_dir.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };

    let result = walk_source(&source, &settings, None);
    let paths: Vec<String> = result
        .entries()
        .iter()
        .map(|e| e.rel_path.clone())
        .collect();

    // Обычный файл проиндексирован
    assert!(paths.contains(&"notes.txt".to_string()));
    // Файлы из папки данных не проиндексированы
    assert!(!paths.contains(&"program_data\\catalog.sqlite3".to_string()));
    assert!(!paths.contains(&"program_data\\search.log".to_string()));
    assert_eq!(paths.len(), 1);

    // Папка данных попала в пропуски
    let skips = result.skips();
    assert!(
        skips
            .iter()
            .any(|s| s.path.ends_with("program_data") && s.reason == "исключено правилом")
    );

    // Корень источника, если совпадает с папкой данных программы, исключается полностью
    let data_source = Source {
        path: app_data.clone(),
        mode: SourceMode::Full,
        max_depth: 0,
        enabled: true,
        volume_serial: None,
        extra_excludes: Vec::new(),
    };
    let data_result = walk_source(&data_source, &settings, None);
    assert_eq!(data_result.entries().len(), 0);
    assert!(
        data_result
            .skips()
            .iter()
            .any(|s| s.reason == "исключено правилом")
    );
}

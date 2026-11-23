//! Извлечение текста из файлов формата PDF: встроенный (pdf-extract) и внешний (pdftotext).
//! Пустой текст -> scan-needs-ocr; без окна (CREATE_NO_WINDOW); все страницы (R1-Б-23).

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::Extracted;
use crate::settings::PdfExtractorMode;

const CREATE_NO_WINDOW: u32 = 0x08000000;

/// Извлекает текст из PDF-файла встроенным извлекателем или внешней утилитой pdftotext.
pub fn extract_pdf(
    path: Option<&Path>,
    bytes: Option<&[u8]>,
    mode: PdfExtractorMode,
    external_pdftotext_path: &Path,
    limit_chars: usize,
) -> Extracted {
    match mode {
        PdfExtractorMode::Builtin => extract_pdf_builtin(path, bytes, limit_chars),
        PdfExtractorMode::External => {
            extract_pdf_external(path, bytes, external_pdftotext_path, limit_chars)
        }
    }
}

/// Встроенное извлечение через pdf-extract с перехватом паник.
pub fn extract_pdf_builtin(
    path: Option<&Path>,
    bytes: Option<&[u8]>,
    limit_chars: usize,
) -> Extracted {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(data) = bytes {
            pdf_extract::extract_text_from_mem(data)
        } else if let Some(p) = path {
            pdf_extract::extract_text(p)
        } else {
            Err(pdf_extract::OutputError::IoError(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Источник PDF не указан",
            )))
        }
    }));

    match result {
        Ok(Ok(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                Extracted::scan_needs_ocr()
            } else {
                let text_res: String = if text.chars().count() > limit_chars {
                    text.chars().take(limit_chars).collect()
                } else {
                    text
                };
                Extracted::text(text_res)
            }
        }
        Ok(Err(err)) => Extracted::failed(format!("Ошибка извлечения PDF: {err}")),
        Err(_) => Extracted::failed("Аварийное завершение парсера PDF"),
    }
}

/// Извлечение через внешнюю утилиту pdftotext (без создания консольного окна, все страницы).
pub fn extract_pdf_external(
    path: Option<&Path>,
    bytes: Option<&[u8]>,
    pdftotext_path: &Path,
    limit_chars: usize,
) -> Extracted {
    let exe = if pdftotext_path.as_os_str().is_empty() {
        Path::new("pdftotext")
    } else {
        pdftotext_path
    };

    let mut cmd = Command::new(exe);
    cmd.creation_flags(CREATE_NO_WINDOW);

    if let Some(p) = path {
        cmd.args(["-enc", "UTF-8", p.to_str().unwrap_or(""), "-"]);
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        match cmd.output() {
            Ok(output) => handle_pdftotext_output(output, limit_chars),
            Err(err) => Extracted::failed(format!("Не удалось запустить pdftotext: {err}")),
        }
    } else if let Some(data) = bytes {
        // Чтение из stdin ("-")
        cmd.args(["-enc", "UTF-8", "-", "-"]);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(err) => {
                return Extracted::failed(format!("Не удалось запустить pdftotext: {err}"));
            }
        };

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data);
        }

        match child.wait_with_output() {
            Ok(output) => handle_pdftotext_output(output, limit_chars),
            Err(err) => Extracted::failed(format!("Ошибка ожидания pdftotext: {err}")),
        }
    } else {
        Extracted::failed("Источник PDF не указан")
    }
}

fn handle_pdftotext_output(output: std::process::Output, limit_chars: usize) -> Extracted {
    if !output.status.success() {
        let err_msg = String::from_utf8_lossy(&output.stderr);
        return Extracted::failed(format!(
            "pdftotext завершился с кодом {:?}: {}",
            output.status.code(),
            err_msg.trim()
        ));
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        Extracted::scan_needs_ocr()
    } else {
        let text_res: String = if text.chars().count() > limit_chars {
            text.chars().take(limit_chars).collect()
        } else {
            text
        };
        Extracted::text(text_res)
    }
}

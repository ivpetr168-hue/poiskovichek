//! Извлечение текста из обычных текстовых форматов и исходного кода.
//! Определение кодировок: UTF-32, UTF-16 (по BOM и без BOM), UTF-8, CP1251, CP866, Latin-1.

use encoding_rs::{IBM866, UTF_16BE, UTF_16LE, WINDOWS_1251};

/// Расширения текстовых файлов и исходного кода (R1-В-1 TEXT_EXTENSIONS).
pub const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "rst", "csv", "tsv", "json", "jsonl", "xml", "yaml", "yml", "ini", "cfg", "log",
    "py", "ps1", "js", "ts", "tsx", "jsx", "java", "c", "cpp", "h", "cs", "sql", "sh", "bat",
    "cmd", "rs", "toml",
];

/// Проверяет, относится ли расширение к текстовым форматам или исходному коду.
pub fn is_text_extension(ext: &str) -> bool {
    let clean = ext.trim_start_matches('.').to_ascii_lowercase();
    TEXT_EXTENSIONS.contains(&clean.as_str())
}

/// Рассчитывает балл читаемости строки на русском/латинском языке
/// по формуле прежней программы (catalogizer.py 1741-1760, R1-А-102).
pub fn readability(value: &str) -> i64 {
    let mut basic_cyrillic = 0i64;
    let mut letters = 0i64;
    let mut separators = 0i64;
    let mut box_drawing = 0i64;
    let mut controls = 0i64;
    let mut replacements = 0i64;

    for ch in value.chars() {
        for lc in ch.to_lowercase() {
            if ('а'..='я').contains(&lc) || lc == 'ё' {
                basic_cyrillic += 1;
            }
        }
        if ch.is_alphabetic() {
            letters += 1;
        }
        if ch.is_whitespace() {
            separators += 1;
        }
        if ('\u{2500}'..='\u{257f}').contains(&ch) {
            box_drawing += 1;
        }
        if (ch as u32) < 32 && ch != '\t' && ch != '\r' && ch != '\n' {
            controls += 1;
        }
        if ch == '\u{fffd}' {
            replacements += 1;
        }
    }

    basic_cyrillic * 4 + letters + separators - box_drawing * 12 - controls * 20 - replacements * 20
}

/// Декодирует байты в текст с автоопределением кодировки (R1-А-98..103, R1-Б-16, 17).
pub fn decode_text(data: &[u8]) -> String {
    // 1. Сигнатуры BOM UTF-32 (R1-А-98)
    if data.starts_with(&[0xff, 0xfe, 0x00, 0x00]) {
        return data[4..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let code = u32::from_le_bytes(*c);
                char::from_u32(code).unwrap_or('\u{fffd}')
            })
            .collect();
    }
    if data.starts_with(&[0x00, 0x00, 0xfe, 0xff]) {
        return data[4..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let code = u32::from_be_bytes(*c);
                char::from_u32(code).unwrap_or('\u{fffd}')
            })
            .collect();
    }

    // 2. Сигнатуры BOM UTF-16 (R1-А-99)
    if data.starts_with(&[0xff, 0xfe]) {
        return UTF_16LE.decode(&data[2..]).0.into_owned();
    }
    if data.starts_with(&[0xfe, 0xff]) {
        return UTF_16BE.decode(&data[2..]).0.into_owned();
    }

    // 3. Эвристика UTF-16 без BOM: доля нулевых и 4-х байтов >= 60% (R1-А-100)
    if data.len() >= 4 {
        let even_count = data
            .iter()
            .step_by(2)
            .filter(|&&b| b == 0 || b == 4)
            .count();
        let odd_count = data
            .iter()
            .skip(1)
            .step_by(2)
            .filter(|&&b| b == 0 || b == 4)
            .count();
        let total_even = data.len().div_ceil(2);
        let total_odd = data.len() / 2;
        let even_utf16 = even_count as f64 / total_even as f64;
        let odd_utf16 = odd_count as f64 / total_odd as f64;

        if even_utf16.max(odd_utf16) >= 0.6 {
            let encoding = if even_utf16 > odd_utf16 {
                UTF_16BE
            } else {
                UTF_16LE
            };
            return encoding.decode(data).0.into_owned();
        }
    }

    // 4. Попытка UTF-8 с BOM или без BOM (R1-А-101)
    // Буфер может быть обрезан по пределу чтения в середине многобайтовой буквы:
    // незаконченная последовательность в самом конце (error_len() == None) — это UTF-8,
    // берём текст до неё. Ошибка посередине — не UTF-8.
    match std::str::from_utf8(data) {
        Ok(text) => return text.trim_start_matches('\u{feff}').to_string(),
        Err(e) if e.error_len().is_none() && e.valid_up_to() > 0 => {
            let text = std::str::from_utf8(&data[..e.valid_up_to()]).unwrap_or_default();
            return text.trim_start_matches('\u{feff}').to_string();
        }
        Err(_) => {}
    }

    // 5. Однобайтовые кодировки: cp1251, cp866, latin-1 по максимуму читаемости (R1-А-102, 103)
    let s_1251 = WINDOWS_1251.decode(data).0;
    let s_866 = IBM866.decode(data).0;
    let s_latin: String = data.iter().map(|&b| b as char).collect();

    let score_1251 = readability(&s_1251);
    let score_866 = readability(&s_866);
    let score_latin = readability(&s_latin);

    if score_1251 >= score_866 && score_1251 >= score_latin {
        s_1251.into_owned()
    } else if score_866 >= score_latin {
        s_866.into_owned()
    } else {
        s_latin
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUSSKIY: &str = "Сахалинский архив: съешь ещё этих мягких французских булок";

    #[test]
    fn obrezannyy_utf8_na_kazhdom_baite_v_posledney_bukve() {
        let bayty = RUSSKIY.as_bytes();
        // Обрезка в каждой позиции (длина 1 исключена: один оборванный байт без текста до него — не определить).
        for dlina in 2..=bayty.len() {
            let ozhidaemo = match std::str::from_utf8(&bayty[..dlina]) {
                Ok(s) => s.to_string(),
                Err(e) => std::str::from_utf8(&bayty[..e.valid_up_to()])
                    .unwrap()
                    .to_string(),
            };
            let tekst = decode_text(&bayty[..dlina]);
            assert!(
                RUSSKIY.starts_with(&tekst),
                "длина {dlina}: получено {tekst:?}"
            );
            assert_eq!(tekst, ozhidaemo, "длина {dlina}");
            assert!(!tekst.contains('\u{fffd}'));
        }
    }

    #[test]
    fn obrezannyy_utf8_posle_dlinnogo_teksta() {
        let mut bayty = RUSSKIY.repeat(100).into_bytes();
        let polnyy = decode_text(&bayty);
        for lishnee in 1..=3 {
            // Отрезаем 1..3 байта у последней буквы (она двухбайтовая — хватит одного).
            bayty.truncate(bayty.len() - 1);
            let tekst = decode_text(&bayty);
            assert!(polnyy.starts_with(&tekst), "лишнее {lishnee}");
            assert!(tekst.chars().count() + 2 >= polnyy.chars().count());
        }
    }

    #[test]
    fn utf8_s_trehbaytnoy_bukvoy_na_kontse() {
        // «€» — три байта E2 82 AC: обрезка после 1 и 2 байт.
        let polnyy = "цена 5 €".as_bytes();
        for srez in 1..=2 {
            let tekst = decode_text(&polnyy[..polnyy.len() - srez]);
            assert_eq!(tekst, "цена 5 ");
        }
        assert_eq!(decode_text(polnyy), "цена 5 €");
    }

    #[test]
    fn nastoyashchaya_oshibka_poseredine_ne_utf8() {
        // Текст в CP1251 с несколькими ошибочными для UTF-8 последовательностями.
        let (kod, _, _) = WINDOWS_1251.encode(RUSSKIY);
        assert!(std::str::from_utf8(&kod).is_err());
        assert_eq!(decode_text(&kod), RUSSKIY);

        // Оборванная последовательность посередине, а не в конце — не UTF-8.
        let mut smes = "привет ".as_bytes().to_vec();
        smes.push(0xd0);
        smes.extend_from_slice(b" world");
        assert!(!decode_text(&smes).starts_with("привет"));
    }

    #[test]
    fn cp1251_ostayotsya_cp1251_dazhe_s_obrezkoy() {
        let (kod, _, _) = WINDOWS_1251.encode(RUSSKIY);
        for dlina in 5..=kod.len() {
            let ozhidaemo = WINDOWS_1251.decode(&kod[..dlina]).0.into_owned();
            let tekst = decode_text(&kod[..dlina]);
            assert_eq!(tekst, ozhidaemo, "длина {dlina}");
        }
    }

    #[test]
    fn utf16_s_bom_kak_ranshe() {
        let mut le = vec![0xff, 0xfe];
        le.extend(RUSSKIY.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode_text(&le), RUSSKIY);

        let mut be = vec![0xfe, 0xff];
        be.extend(RUSSKIY.encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(decode_text(&be), RUSSKIY);
    }

    #[test]
    fn utf8_bom_otbrasyvaetsya_i_pri_obrezke() {
        let mut bayty = vec![0xef, 0xbb, 0xbf];
        bayty.extend_from_slice("Привет".as_bytes());
        assert_eq!(decode_text(&bayty), "Привет");
        bayty.pop();
        assert_eq!(decode_text(&bayty), "Приве");
    }
}

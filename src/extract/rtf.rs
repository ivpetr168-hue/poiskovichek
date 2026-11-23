//! Извлечение текста из файлов формата RTF.
//! Обработка дестинаций, шестнадцатеричных байтов \'hh в CP1251, команд \u с суррогатными парами (R1-А-107..110, R1-Б-15, 18).

use encoding_rs::WINDOWS_1251;

const RTF_DESTINATIONS: &[&str] = &[
    "aftncn",
    "aftnsep",
    "aftnsepc",
    "annotation",
    "atnauthor",
    "atndate",
    "atnicn",
    "atnid",
    "atnparent",
    "atnref",
    "atntime",
    "atrfend",
    "atrfstart",
    "author",
    "background",
    "bkmkend",
    "bkmkstart",
    "blipuid",
    "buptim",
    "category",
    "colorschememapping",
    "colortbl",
    "comment",
    "company",
    "creatim",
    "datafield",
    "datastore",
    "defchp",
    "defpap",
    "do",
    "doccomm",
    "docvar",
    "dptxbxtext",
    "ebcend",
    "ebcstart",
    "factoidname",
    "falt",
    "fchars",
    "ffdeftext",
    "ffentrymcr",
    "ffexitmcr",
    "ffformat",
    "ffhelptext",
    "ffl",
    "ffname",
    "ffstattext",
    "field",
    "file",
    "filetbl",
    "fldinst",
    "fldtype",
    "fname",
    "fontemb",
    "fontfile",
    "fonttbl",
    "footer",
    "footerf",
    "footerl",
    "footerr",
    "footnote",
    "formfield",
    "ftncn",
    "ftnsep",
    "ftnsepc",
    "g",
    "generator",
    "gridtbl",
    "header",
    "headerf",
    "headerl",
    "headerr",
    "hl",
    "hlfr",
    "hlinkbase",
    "htmltag",
    "info",
    "keycode",
    "keywords",
    "latentstyles",
    "lchars",
    "levelnumbers",
    "leveltext",
    "lfolevel",
    "linkval",
    "list",
    "listlevel",
    "listname",
    "listoverride",
    "listoverridetable",
    "listpicture",
    "liststylename",
    "listtable",
    "manager",
    "mhtmltag",
    "mmath",
    "mmathpr",
    "nextfile",
    "nonesttables",
    "objalias",
    "objclass",
    "objdata",
    "object",
    "objname",
    "objsect",
    "objtime",
    "oldcprops",
    "oldpprops",
    "oldsprops",
    "oldtprops",
    "oleclsid",
    "operator",
    "panose",
    "password",
    "passwordhash",
    "pgp",
    "pgptbl",
    "picprop",
    "pict",
    "pn",
    "pnseclvl",
    "pntext",
    "pntxta",
    "pntxtb",
    "printim",
    "private",
    "propname",
    "protend",
    "protstart",
    "protusertbl",
    "pxe",
    "result",
    "revtbl",
    "revtim",
    "rsidtbl",
    "rxe",
    "shp",
    "shpgrp",
    "shpinst",
    "shppict",
    "shprslt",
    "shptxt",
    "sn",
    "sp",
    "staticval",
    "stylesheet",
    "subject",
    "sv",
    "svb",
    "tc",
    "template",
    "themedata",
    "title",
    "txe",
    "ud",
    "upr",
    "userprops",
    "wgrffmtfilter",
    "windowcaption",
    "writereservation",
    "writereservhash",
    "xe",
    "xmlattrname",
    "xmlattrvalue",
    "xmlclose",
    "xmlname",
    "xmlnstbl",
    "xmlopen",
];

fn is_destination(word: &str) -> bool {
    RTF_DESTINATIONS.contains(&word)
}

/// Доступ к байтам как к символам Latin-1 (байт b -> символ U+00bb), без копирования.
struct Latin1<'a>(&'a [u8]);

impl std::ops::Index<usize> for Latin1<'_> {
    type Output = char;

    fn index(&self, index: usize) -> &char {
        // Таблица из 256 символов: Index должен вернуть ссылку.
        &LATIN1_TABLE[self.0[index] as usize]
    }
}

const LATIN1_TABLE: [char; 256] = {
    let mut t = ['\0'; 256];
    let mut k = 0;
    while k < 256 {
        t[k] = k as u8 as char;
        k += 1;
    }
    t
};

/// Извлекает текст из сырых байтов RTF (R1-А-107..110, R1-Б-15, 18).
pub fn extract_rtf_text(data: &[u8], limit: usize) -> String {
    // В RTF текст изначально латинский (ASCII / ANSI): каждый байт — один символ (Latin-1).
    // Копии файла в String/Vec<char> не делаем: символ читается прямо из байта.
    let len = data.len();
    let chars = Latin1(data);

    let mut stack: Vec<(usize, bool)> = Vec::new();
    let mut ucskip: usize = 1;
    let mut curskip: usize = 0;
    let mut ignorable = false;
    let mut output = String::new();
    let mut pending_high_surrogate: Option<u32> = None;

    // Число символов в output считаем накопительно: раньше на каждом шаге вызывался
    // output.chars().count() — полный проход по уже набранному тексту, то есть
    // O(длина файла × длина вывода), квадратично (задача 190).
    let mut counted_bytes: usize = 0;
    let mut counted_chars: usize = 0;

    let mut i = 0;
    while i < len {
        counted_chars += output[counted_bytes..].chars().count();
        counted_bytes = output.len();
        if counted_chars >= limit {
            break;
        }
        let ch = chars[i];
        if ch == '{' {
            stack.push((ucskip, ignorable));
            curskip = 0;
            i += 1;
        } else if ch == '}' {
            if let Some((prev_ucskip, prev_ignorable)) = stack.pop() {
                ucskip = prev_ucskip;
                ignorable = prev_ignorable;
            }
            curskip = 0;
            i += 1;
        } else if ch == '\\' {
            i += 1;
            if i >= len {
                break;
            }
            let next_ch = chars[i];
            if next_ch == '*' {
                ignorable = true;
                i += 1;
            } else if next_ch == '\'' {
                // Шестнадцатеричный байт \'hh
                i += 1;
                if i + 1 < len {
                    let h1 = chars[i];
                    let h2 = chars[i + 1];
                    i += 2;
                    if let Ok(byte_val) = u8::from_str_radix(&format!("{h1}{h2}"), 16) {
                        if curskip > 0 {
                            curskip -= 1;
                        } else if !ignorable {
                            let byte_buf = [byte_val];
                            let decoded = WINDOWS_1251.decode(&byte_buf).0;
                            output.push_str(&decoded);
                        }
                    }
                }
            } else if next_ch == '\\' || next_ch == '{' || next_ch == '}' {
                i += 1;
                if curskip > 0 {
                    curskip -= 1;
                } else if !ignorable {
                    output.push(next_ch);
                }
            } else if next_ch == '~' {
                // Неразрывный пробел
                i += 1;
                if curskip > 0 {
                    curskip -= 1;
                } else if !ignorable {
                    output.push(' ');
                }
            } else if next_ch == '_' {
                // Неразрывный дефис
                i += 1;
                if curskip > 0 {
                    curskip -= 1;
                } else if !ignorable {
                    output.push('-');
                }
            } else if next_ch.is_ascii_alphabetic() {
                // Управляющее слово: \word[number][ ]
                let word_start = i;
                while i < len && chars[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word: String = data[word_start..i]
                    .iter()
                    .map(|&b| b.to_ascii_lowercase() as char)
                    .collect();

                // Опциональный числовой аргумент (может начинаться с '-')
                let mut arg_str = String::new();
                if i < len && (chars[i] == '-' || chars[i].is_ascii_digit()) {
                    arg_str.push(chars[i]);
                    i += 1;
                    while i < len && chars[i].is_ascii_digit() {
                        arg_str.push(chars[i]);
                        i += 1;
                    }
                }

                // Опциональный завершающий пробел
                if i < len && chars[i] == ' ' {
                    i += 1;
                }

                if is_destination(&word) {
                    ignorable = true;
                } else if !ignorable && (word == "par" || word == "line") {
                    output.push('\n');
                } else if word == "uc" {
                    if let Ok(val) = arg_str.parse::<usize>() {
                        ucskip = val;
                    }
                } else if word == "u"
                    && !ignorable
                    && let Ok(mut val) = arg_str.parse::<i32>()
                {
                    if val < 0 {
                        val += 65536;
                    }
                    let code = val as u32;

                    if (0xd800..=0xdbff).contains(&code) {
                        // Верхний суррогат UTF-16
                        pending_high_surrogate = Some(code);
                    } else if (0xdc00..=0xdfff).contains(&code) {
                        // Нижний суррогат UTF-16
                        if let Some(high) = pending_high_surrogate.take() {
                            let combined = 0x10000 + ((high - 0xd800) << 10) + (code - 0xdc00);
                            if let Some(c) = char::from_u32(combined) {
                                output.push(c);
                            }
                        }
                    } else {
                        pending_high_surrogate = None;
                        if let Some(c) = char::from_u32(code) {
                            output.push(c);
                        }
                    }
                    curskip = ucskip;
                }
            } else {
                // Другие управляющие символы (пропускаем)
                i += 1;
            }
        } else if ch == '\r' || ch == '\n' {
            // Переводы строк в исходнике RTF игнорируются (переносы задаются \par)
            i += 1;
        } else {
            // Обычный символ
            i += 1;
            if curskip > 0 {
                curskip -= 1;
            } else if !ignorable {
                output.push(ch);
            }
        }
    }

    // Нормализация пробелов с сохранением переводов строк
    let lines: Vec<String> = output
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect();

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Задача 190: разбор должен быть линейным. Раньше на каждом байте считался
    /// output.chars().count(), и файл в десятки МБ разбирался минутами.
    #[test]
    fn rtf_parsing_is_linear_on_multi_megabyte_input() {
        let mut rtf: Vec<u8> = Vec::new();
        rtf.extend_from_slice(br"{\rtf1\ansi\ansicpg1251\deff0{\fonttbl{\f0 Arial;}}");
        let mut paragraphs = 0usize;
        while rtf.len() < 6 * 1024 * 1024 {
            // Абзац с текстом в CP1251 (\'hh), в Юникоде (\u) и обычными буквами, плюс
            // картинка (дестинация, содержимое пропускается) — как в настоящих книгах.
            rtf.extend_from_slice(br"{\pard\f0\fs24 \'cf\'f0\'e8\'e2\'e5\'f2, ");
            // Три символа «мир» в Юникоде; обратная косая записана как \x5c
            rtf.extend_from_slice(b"\x5cu1084?\x5cu1080?\x5cu1088?");
            rtf.extend_from_slice(br"! Hello world, plain text.\par}");
            rtf.extend_from_slice(b"\r\n{\\*\\shppict{\\pict\\jpegblip 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef}}\r\n");
            paragraphs += 1;
        }
        rtf.push(b'}');

        let started = std::time::Instant::now();
        let text = extract_rtf_text(&rtf, 20_000_000);
        let elapsed = started.elapsed();

        assert_eq!(text.lines().count(), paragraphs);
        assert!(text.starts_with("Привет, мир! Hello world, plain text."));
        // Линейный разбор занимает доли секунды; квадратичный — минуты.
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "разбор {} байт занял {:?}",
            rtf.len(),
            elapsed
        );
    }

    #[test]
    fn rtf_stops_at_character_limit() {
        let mut rtf = b"{\\rtf1 ".to_vec();
        rtf.extend(std::iter::repeat_n(b'a', 1000));
        rtf.push(b'}');
        assert_eq!(extract_rtf_text(&rtf, 10), "a".repeat(10));
        assert_eq!(extract_rtf_text(&rtf, 5000).len(), 1000);
    }
}

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crc32fast::Hasher;
use fst::{Map, MapBuilder};
use memmap2::Mmap;

pub const MAGIC: &[u8; 8] = b"PKMORPH1";
pub const CURRENT_VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum MorphError {
    #[error("файл словаря не найден: {0}")]
    NotFound(PathBuf),
    #[error("ошибка ввода-вывода при чтении словаря {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("повреждён словарь {path}: {reason}")]
    Corrupted { path: PathBuf, reason: String },
    #[error("ошибка базы данных SQLite {path}: {source}")]
    Database {
        path: PathBuf,
        #[source]
        source: rusqlite::Error,
    },
    #[error("ошибка построения FST: {0}")]
    FstBuild(#[from] fst::Error),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuildStats {
    pub form_count: u32,
    pub lexeme_count: u32,
    pub multi_lexeme_forms: u32,
    pub file_size: u64,
    pub elapsed_millis: u128,
}

struct MorphInner {
    _mmap: Mmap,
    map: Map<&'static [u8]>,
    lexemes: &'static [u32],
    form_count: u32,
    lexeme_count: u32,
}

#[derive(Clone)]
pub struct Morph {
    inner: Arc<MorphInner>,
}

impl std::fmt::Debug for Morph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Morph")
            .field("form_count", &self.form_count())
            .field("lexeme_count", &self.lexeme_count())
            .finish()
    }
}

impl Morph {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MorphError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(MorphError::NotFound(path.to_path_buf()));
        }
        let file = File::open(path).map_err(|e| MorphError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let metadata = file.metadata().map_err(|e| MorphError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        let file_len = metadata.len();
        if file_len < HEADER_SIZE as u64 {
            return Err(MorphError::Corrupted {
                path: path.to_path_buf(),
                reason: format!(
                    "размер файла ({file_len} байт) меньше минимального размера заголовка ({HEADER_SIZE} байт)"
                ),
            });
        }

        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| MorphError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;

        // 1. Проверка магии
        if &mmap[0..8] != MAGIC {
            return Err(MorphError::Corrupted {
                path: path.to_path_buf(),
                reason: format!(
                    "неверная сигнатура (магия): {:?}, ожидалась {:?}",
                    &mmap[0..8],
                    MAGIC
                ),
            });
        }

        // 2. Проверка версии
        let version = u32::from_le_bytes(mmap[8..12].try_into().unwrap());
        if version != CURRENT_VERSION {
            return Err(MorphError::Corrupted {
                path: path.to_path_buf(),
                reason: format!(
                    "неподдерживаемая версия: {version}, поддерживается только {CURRENT_VERSION}"
                ),
            });
        }

        // 3. Проверка контрольной суммы
        let stored_crc = u32::from_le_bytes(mmap[56..60].try_into().unwrap());
        let mut hasher = Hasher::new();
        hasher.update(&mmap[0..56]);
        hasher.update(&[0u8; 4]);
        hasher.update(&mmap[60..]);
        let computed_crc = hasher.finalize();
        if computed_crc != stored_crc {
            return Err(MorphError::Corrupted {
                path: path.to_path_buf(),
                reason: format!(
                    "контрольная сумма не совпала: вычислено {computed_crc:#010x}, в заголовке {stored_crc:#010x}"
                ),
            });
        }

        let form_count = u32::from_le_bytes(mmap[16..20].try_into().unwrap());
        let lexeme_count = u32::from_le_bytes(mmap[20..24].try_into().unwrap());
        let fst_offset = u64::from_le_bytes(mmap[24..32].try_into().unwrap()) as usize;
        let fst_len = u64::from_le_bytes(mmap[32..40].try_into().unwrap()) as usize;
        let lexemes_offset = u64::from_le_bytes(mmap[40..48].try_into().unwrap()) as usize;
        let lexemes_len = u64::from_le_bytes(mmap[48..56].try_into().unwrap()) as usize;

        if fst_offset < HEADER_SIZE || fst_offset + fst_len > mmap.len() {
            return Err(MorphError::Corrupted {
                path: path.to_path_buf(),
                reason: "смещение или длина FST выходит за границы файла".to_string(),
            });
        }

        if lexemes_offset < fst_offset + fst_len || lexemes_offset + lexemes_len > mmap.len() {
            return Err(MorphError::Corrupted {
                path: path.to_path_buf(),
                reason: "смещение или длина таблицы лексем выходит за границы файла".to_string(),
            });
        }

        if !lexemes_offset.is_multiple_of(4) || !lexemes_len.is_multiple_of(4) {
            return Err(MorphError::Corrupted {
                path: path.to_path_buf(),
                reason: "таблица лексем не выровнена по 4 байтам".to_string(),
            });
        }

        let mmap_ptr = mmap.as_ptr();
        let fst_slice: &'static [u8] =
            unsafe { std::slice::from_raw_parts(mmap_ptr.add(fst_offset), fst_len) };
        let lexemes_slice: &'static [u32] = unsafe {
            std::slice::from_raw_parts(mmap_ptr.add(lexemes_offset) as *const u32, lexemes_len / 4)
        };

        let map = Map::new(fst_slice).map_err(|e| MorphError::Corrupted {
            path: path.to_path_buf(),
            reason: format!("ошибка структуры FST: {e}"),
        })?;

        Ok(Self {
            inner: Arc::new(MorphInner {
                _mmap: mmap,
                map,
                lexemes: lexemes_slice,
                form_count,
                lexeme_count,
            }),
        })
    }

    #[inline(always)]
    pub fn lexemes(&self, normalized_word: &str) -> &[u32] {
        if let Some(offset) = self.inner.map.get(normalized_word) {
            let slice = self.inner.lexemes;
            let idx = (offset / 4) as usize;
            if idx < slice.len() {
                let count = slice[idx] as usize;
                if idx + 1 + count <= slice.len() {
                    return &slice[idx + 1..idx + 1 + count];
                }
            }
        }
        &[]
    }

    pub fn form_count(&self) -> u32 {
        self.inner.form_count
    }

    pub fn lexeme_count(&self) -> u32 {
        self.inner.lexeme_count
    }
}

pub fn normalize(word: &str) -> String {
    let mut result = String::with_capacity(word.len());
    for c in word.chars() {
        for lc in c.to_lowercase() {
            if lc == 'ё' {
                result.push('е');
            } else {
                result.push(lc);
            }
        }
    }
    result
}

pub fn build_morph_fst(
    sqlite_path: impl AsRef<Path>,
    output_path: impl AsRef<Path>,
) -> Result<BuildStats, MorphError> {
    let sqlite_path = sqlite_path.as_ref();
    let output_path = output_path.as_ref();
    let started = Instant::now();

    let conn = rusqlite::Connection::open_with_flags(
        sqlite_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| MorphError::Database {
        path: sqlite_path.to_path_buf(),
        source: e,
    })?;

    // Читаем все пары (word, lexeme_id)
    let mut stmt = conn
        .prepare("SELECT word, lexeme_id FROM word_primary")
        .map_err(|e| MorphError::Database {
            path: sqlite_path.to_path_buf(),
            source: e,
        })?;

    let mut rows = stmt.query([]).map_err(|e| MorphError::Database {
        path: sqlite_path.to_path_buf(),
        source: e,
    })?;

    let mut forms_map: HashMap<String, BTreeSet<u32>> = HashMap::with_capacity(3_100_000);
    let mut all_lexemes: BTreeSet<u32> = BTreeSet::new();

    while let Some(row) = rows.next().map_err(|e| MorphError::Database {
        path: sqlite_path.to_path_buf(),
        source: e,
    })? {
        let word: String = row.get(0).map_err(|e| MorphError::Database {
            path: sqlite_path.to_path_buf(),
            source: e,
        })?;
        let lexeme_id: u32 = row.get(1).map_err(|e| MorphError::Database {
            path: sqlite_path.to_path_buf(),
            source: e,
        })?;

        let norm = normalize(&word);
        all_lexemes.insert(lexeme_id);
        forms_map.entry(norm).or_default().insert(lexeme_id);
    }

    let form_count = forms_map.len() as u32;
    let lexeme_count = all_lexemes.len() as u32;
    let multi_lexeme_forms = forms_map.values().filter(|s| s.len() >= 2).count() as u32;

    // Сортируем формы по ключу для FST
    let mut sorted_forms: Vec<(String, Vec<u32>)> = forms_map
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().collect::<Vec<u32>>()))
        .collect();
    sorted_forms.sort_unstable_by(|a, b| a.0.cmp(&b.0));

    // Создаём таблицу лексем с дедупликацией списков
    let mut lexemes_u32: Vec<u32> = Vec::with_capacity(sorted_forms.len() * 2);
    let mut list_offsets: HashMap<Vec<u32>, u32> = HashMap::with_capacity(300_000);

    let mut fst_bytes = Vec::with_capacity(8 * 1024 * 1024);
    let mut fst_builder = MapBuilder::new(&mut fst_bytes)?;

    for (form, lexemes) in &sorted_forms {
        let offset = if let Some(&off) = list_offsets.get(lexemes) {
            off
        } else {
            let off = (lexemes_u32.len() * 4) as u32;
            list_offsets.insert(lexemes.clone(), off);
            lexemes_u32.push(lexemes.len() as u32);
            lexemes_u32.extend_from_slice(lexemes);
            off
        };

        fst_builder.insert(form.as_bytes(), offset as u64)?;
    }

    fst_builder.finish()?;

    // Формируем результирующий файл
    let fst_offset = HEADER_SIZE as u64;
    let fst_len = fst_bytes.len() as u64;

    // Выравнивание таблицы лексем по границе 8 байт
    let unaligned_lexemes_offset = fst_offset + fst_len;
    let lexemes_offset = (unaligned_lexemes_offset + 7) & !7;
    let fst_padding = (lexemes_offset - unaligned_lexemes_offset) as usize;

    let lexemes_len = (lexemes_u32.len() * 4) as u64;

    let total_file_size = (lexemes_offset + lexemes_len) as usize;
    let mut file_bytes = Vec::with_capacity(total_file_size);

    // Заголовок (64 байта)
    file_bytes.extend_from_slice(MAGIC); // 0..8
    file_bytes.extend_from_slice(&CURRENT_VERSION.to_le_bytes()); // 8..12
    file_bytes.extend_from_slice(&0u32.to_le_bytes()); // 12..16 flags
    file_bytes.extend_from_slice(&form_count.to_le_bytes()); // 16..20
    file_bytes.extend_from_slice(&lexeme_count.to_le_bytes()); // 20..24
    file_bytes.extend_from_slice(&fst_offset.to_le_bytes()); // 24..32
    file_bytes.extend_from_slice(&fst_len.to_le_bytes()); // 32..40
    file_bytes.extend_from_slice(&lexemes_offset.to_le_bytes()); // 40..48
    file_bytes.extend_from_slice(&lexemes_len.to_le_bytes()); // 48..56
    file_bytes.extend_from_slice(&0u32.to_le_bytes()); // 56..60 checksum placeholder
    file_bytes.extend_from_slice(&[0u8; 4]); // 60..64 reserved

    // FST данные
    file_bytes.extend_from_slice(&fst_bytes);

    // Выравнивающий отступ
    file_bytes.extend_from_slice(&vec![0u8; fst_padding]);

    // Таблица лексем
    for val in &lexemes_u32 {
        file_bytes.extend_from_slice(&val.to_le_bytes());
    }

    // Вычисление CRC32
    let mut hasher = Hasher::new();
    hasher.update(&file_bytes);
    let checksum = hasher.finalize();

    // Записываем контрольную сумму в заголовок
    file_bytes[56..60].copy_from_slice(&checksum.to_le_bytes());

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MorphError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let mut out_file = File::create(output_path).map_err(|e| MorphError::Io {
        path: output_path.to_path_buf(),
        source: e,
    })?;
    out_file
        .write_all(&file_bytes)
        .map_err(|e| MorphError::Io {
            path: output_path.to_path_buf(),
            source: e,
        })?;
    out_file.flush().map_err(|e| MorphError::Io {
        path: output_path.to_path_buf(),
        source: e,
    })?;

    let elapsed = started.elapsed();
    let stats = BuildStats {
        form_count,
        lexeme_count,
        multi_lexeme_forms,
        file_size: file_bytes.len() as u64,
        elapsed_millis: elapsed.as_millis(),
    };

    Ok(stats)
}

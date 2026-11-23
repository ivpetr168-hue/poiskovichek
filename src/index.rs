//! Модуль полнотекстового индекса на базе tantivy (раздел 2.3, 2.6 ТЗ).
//! Схема: `sha` (STRING, хранимое, ключ), `body` (текст с токенизатором `PoiskTokenizer`, WithFreqsAndPositions, не хранится).

use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    Directory, DirectoryLock, FileHandle, FileSlice, Lock, MmapDirectory, WatchCallback,
    WatchHandle, WritePtr,
};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::{
    Index, IndexReader, IndexSettings, IndexWriter, ReloadPolicy, TantivyDocument, Term,
};

use crate::journal;
use crate::morph::Morph;
use crate::tokens::PoiskTokenizer;

pub const WRITER_MEMORY_BUDGET_BYTES: usize = 256 * 1024 * 1024;
pub const TOKENIZER_NAME: &str = "poisk";

/// Ошибки работы с поисковым индексом.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("Ошибка ввода-вывода индекса: {0}")]
    Io(#[from] std::io::Error),
    #[error("Ошибка Tantivy: {0}")]
    Tantivy(#[from] tantivy::TantivyError),
    #[error("Ошибка открытия каталога индекса: {0}")]
    OpenDirectory(#[from] tantivy::directory::error::OpenDirectoryError),
    #[error("Ошибка чтения каталога индекса: {0}")]
    OpenRead(#[from] tantivy::directory::error::OpenReadError),
    #[error("Поле индекса не найдено: {0}")]
    MissingField(&'static str),
}

/// Число попыток записи, когда замену файла на миг запрещает чужая программа.
pub const ZAPIS_POPYTOK: u32 = 10;
const ZAPIS_PAUZA_NACHALNAYA: Duration = Duration::from_millis(50);
const ZAPIS_PAUZA_PREDEL: Duration = Duration::from_millis(500);

/// Ошибка «доступ запрещён» (5) или «файл занят» (32): антивирус или другая программа
/// держит файл в момент замены.
fn dostup_zapreshchen(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::PermissionDenied || matches!(e.raw_os_error(), Some(5 | 32))
}

/// Повторяет действие при отказе в доступе: до 10 попыток, пауза 50 мс, удваивается, не больше 500 мс.
/// Другая ошибка возвращается сразу.
pub fn povtor_pri_otkaze<T>(mut deystvie: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    povtor_s_pauzoy(&mut deystvie, ZAPIS_PAUZA_NACHALNAYA)
}

/// То же, но с заданной начальной паузой (чтобы проверить без ожидания).
pub fn povtor_s_pauzoy<T>(
    deystvie: &mut dyn FnMut() -> io::Result<T>,
    nachalnaya_pauza: Duration,
) -> io::Result<T> {
    let mut pauza = nachalnaya_pauza;
    let mut popytka = 1;
    loop {
        match deystvie() {
            Err(e) if dostup_zapreshchen(&e) && popytka < ZAPIS_POPYTOK => {
                journal::warn(
                    "index",
                    &format!("Отказ в доступе при записи индекса, попытка {popytka}: {e}"),
                );
                thread::sleep(pauza);
                pauza = (pauza * 2).min(ZAPIS_PAUZA_PREDEL);
                popytka += 1;
            }
            itog => return itog,
        }
    }
}

/// Каталог индекса на диске, повторяющий запись `meta.json` и `.managed.json`
/// при кратковременном отказе в доступе (Windows, антивирус).
#[derive(Clone, Debug)]
pub struct PovtornyiKatalog {
    vnutri: MmapDirectory,
}

impl PovtornyiKatalog {
    pub fn open(path: &Path) -> Result<Self, tantivy::directory::error::OpenDirectoryError> {
        Ok(Self {
            vnutri: MmapDirectory::open(path)?,
        })
    }
}

impl Directory for PovtornyiKatalog {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.vnutri.get_file_handle(path)
    }

    fn open_read(&self, path: &Path) -> Result<FileSlice, OpenReadError> {
        self.vnutri.open_read(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.vnutri.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.vnutri.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        self.vnutri.open_write(path)
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.vnutri.atomic_read(path)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        povtor_pri_otkaze(|| self.vnutri.atomic_write(path, data))
    }

    fn sync_directory(&self) -> io::Result<()> {
        self.vnutri.sync_directory()
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.vnutri.acquire_lock(lock)
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.vnutri.watch(watch_callback)
    }
}

/// Вычисляет число потоков писателя tantivy: по числу ядер, но не больше 4.
pub fn default_writer_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(2, |n| n.get())
        .min(4)
}

/// Создаёт схему tantivy для ПоисКовичеКа:
/// - `sha`: STRING | STORED
/// - `body`: WithFreqsAndPositions с токенизатором `poisk`, не хранится.
pub fn create_schema() -> (Schema, Field, Field) {
    let mut builder = Schema::builder();
    let sha_field = builder.add_text_field("sha", STRING | STORED);
    let body_indexing = TextFieldIndexing::default()
        .set_tokenizer(TOKENIZER_NAME)
        .set_index_option(IndexRecordOption::WithFreqsAndPositions);
    let body_options = TextOptions::default().set_indexing_options(body_indexing);
    let body_field = builder.add_text_field("body", body_options);
    (builder.build(), sha_field, body_field)
}

/// Регистрирует токенизатор `poisk` в менеджере токенизаторов индекса.
/// Если словарь не задан, выводит честное предупреждение в журнал.
pub fn register_poisk_tokenizer(index: &Index, morph: Option<Morph>) {
    if morph.is_none() {
        journal::warn(
            "index",
            "Словарь словоформ не задан; токенизатор poisk работает без словоформ",
        );
    }
    let tokenizer = PoiskTokenizer::new(morph);
    index.tokenizers().register(TOKENIZER_NAME, tokenizer);
}

/// Дескриптор писателя индекса tantivy.
pub struct IndexWriterHandle {
    writer: IndexWriter,
    index: Index,
    sha_field: Field,
    body_field: Field,
}

impl IndexWriterHandle {
    /// Добавляет документ в индекс по его SHA-256 хэшу и тексту.
    pub fn add(&mut self, sha: &str, text: &str) -> Result<(), IndexError> {
        let mut doc = TantivyDocument::default();
        doc.add_text(self.sha_field, sha);
        doc.add_text(self.body_field, text);
        self.writer.add_document(doc)?;
        Ok(())
    }

    /// Удаляет документ из индекса по SHA-256 хэшу.
    pub fn delete(&mut self, sha: &str) -> Result<(), IndexError> {
        let term = Term::from_field_text(self.sha_field, sha);
        self.writer.delete_term(term);
        Ok(())
    }

    /// Удаляет все документы из индекса (для полного перестроения).
    pub fn delete_all(&mut self) -> Result<(), IndexError> {
        self.writer.delete_all_documents()?;
        Ok(())
    }

    /// Фиксирует (коммитит) изменения в индексе.
    pub fn commit(&mut self) -> Result<(), IndexError> {
        self.writer.commit()?;
        Ok(())
    }

    /// Возвращает ридер для чтения и поиска по индексу.
    pub fn reader(&self) -> Result<IndexReader, IndexError> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        Ok(reader)
    }

    /// Ссылка на экземпляр `Index`.
    pub fn index(&self) -> &Index {
        &self.index
    }

    /// Ссылка на поле `sha`.
    pub fn sha_field(&self) -> Field {
        self.sha_field
    }

    /// Ссылка на поле `body`.
    pub fn body_field(&self) -> Field {
        self.body_field
    }

    /// Ссылка на tantivy `IndexWriter`.
    pub fn writer(&self) -> &IndexWriter {
        &self.writer
    }
}

/// Открывает существующий индекс или создает новый в указанной директории
/// с настройками памяти 256 МБ и до 4 потоков писателя.
pub fn open_or_create(
    index_dir: &Path,
    morph: Option<Morph>,
) -> Result<IndexWriterHandle, IndexError> {
    open_or_create_with_options(
        index_dir,
        morph,
        WRITER_MEMORY_BUDGET_BYTES,
        default_writer_threads(),
    )
}

/// Открывает существующий индекс или создает новый с заданными параметрами памяти и потоков.
pub fn open_or_create_with_options(
    index_dir: &Path,
    morph: Option<Morph>,
    memory_budget_bytes: usize,
    num_threads: usize,
) -> Result<IndexWriterHandle, IndexError> {
    fs::create_dir_all(index_dir)?;
    let mmap_dir = PovtornyiKatalog::open(index_dir)?;

    let index = if Index::exists(&mmap_dir)? {
        let index = Index::open(mmap_dir)?;
        register_poisk_tokenizer(&index, morph);
        index
    } else {
        let (schema, _, _) = create_schema();
        let index = Index::create(mmap_dir, schema, IndexSettings::default())?;
        register_poisk_tokenizer(&index, morph);
        index
    };

    let schema = index.schema();
    let sha_field = schema
        .get_field("sha")
        .map_err(|_| IndexError::MissingField("sha"))?;
    let body_field = schema
        .get_field("body")
        .map_err(|_| IndexError::MissingField("body"))?;

    let writer =
        index.writer_with_num_threads::<TantivyDocument>(num_threads, memory_budget_bytes)?;

    Ok(IndexWriterHandle {
        writer,
        index,
        sha_field,
        body_field,
    })
}

/// Открывает ридер для чтения существующего индекса без создания писателя.
pub fn reader(index_dir: &Path, morph: Option<Morph>) -> Result<IndexReader, IndexError> {
    let mmap_dir = PovtornyiKatalog::open(index_dir)?;
    let index = Index::open(mmap_dir)?;
    register_poisk_tokenizer(&index, morph);
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into()?;
    Ok(reader)
}

/// Создает индекс в памяти (для быстрых модульных тестов).
pub fn create_in_ram(morph: Option<Morph>) -> Result<IndexWriterHandle, IndexError> {
    create_in_ram_with_options(morph, WRITER_MEMORY_BUDGET_BYTES, 1)
}

/// Создает индекс в памяти с заданными параметрами памяти и потоков.
pub fn create_in_ram_with_options(
    morph: Option<Morph>,
    memory_budget_bytes: usize,
    num_threads: usize,
) -> Result<IndexWriterHandle, IndexError> {
    let (schema, sha_field, body_field) = create_schema();
    let index = Index::create_in_ram(schema);
    register_poisk_tokenizer(&index, morph);
    let writer =
        index.writer_with_num_threads::<TantivyDocument>(num_threads, memory_budget_bytes)?;
    Ok(IndexWriterHandle {
        writer,
        index,
        sha_field,
        body_field,
    })
}

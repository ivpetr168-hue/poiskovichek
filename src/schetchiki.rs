//! Счётчики времени этапов конвейера индексации: где он тратит время и где стоит в ожидании.
//!
//! Все счётчики атомарные (`Relaxed`), без блокировок: на каждый файл приходится несколько
//! чтений часов и сложений, это доли процента от времени разбора одного файла. Очереди
//! измеряются так: сначала быстрая попытка без ожидания, часы включаются только если пришлось
//! ждать, поэтому поведение конвейера счётчики не меняют.
//!
//! Времена в наносекундах, суммарно по всем потокам этапа. Занятость этапа =
//! сумма / (время прогона × число потоков этапа).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crossbeam_channel::{
    Receiver, RecvError, RecvTimeoutError, SendError, Sender, TryRecvError, TrySendError,
};

/// Порог (доля), с которого ожидание в очереди считается заметным.
const POROG_OZHIDANIYA: f64 = 0.10;

macro_rules! schetchiki {
    ($($(#[$meta:meta])* $name:ident),* $(,)?) => {
        /// Живые счётчики одного прогона конвейера (пишутся из всех потоков).
        #[derive(Debug, Default)]
        pub struct SchetchikiEtapov {
            $($(#[$meta])* pub $name: AtomicU64,)*
            /// Разбор по форматам файлов (не атомарные числа, поэтому вне макроса).
            pub formaty: SborshchikFormatov,
        }

        /// Снимок счётчиков по окончании прогона (то же, что `SchetchikiEtapov`, но числа).
        #[derive(Debug, Clone, Default, PartialEq, Eq)]
        pub struct OtchetEtapov {
            $($(#[$meta])* pub $name: u64,)*
            /// Время прогона конвейера, нс (от открытия базы до завершения всех потоков).
            pub wall_ns: u64,
            /// Разбор по форматам файлов.
            pub formaty: RazborPoFormatam,
        }

        impl SchetchikiEtapov {
            /// Снимок текущих значений; `wall_ns` — время прогона.
            pub fn snimok(&self, wall_ns: u64) -> OtchetEtapov {
                OtchetEtapov {
                    $($name: self.$name.load(Ordering::Relaxed),)*
                    wall_ns,
                    formaty: self.formaty.snimok(),
                }
            }
        }
    };
}

schetchiki! {
    /// Обход: время работы обходчиков (без ожидания места в очереди A), нс.
    obhod_ns,
    /// Обход: число потоков (по одному на источник).
    obhod_potokov,
    /// Обход: сколько записей о файлах отправлено.
    obhod_faylov,
    /// Обход ждал места в очереди A (сверка не успевает), нс.
    obhod_zhdal_a_ns,

    /// Сверка: время обработки сообщений вместе с отправкой в B и C, нс.
    sverka_ns,
    /// Сверка: сколько записей о файлах сверено.
    sverka_faylov,
    /// Сверка ждала сообщений из A (обход не успевает), нс.
    sverka_zhdala_a_ns,
    /// Сверка ждала места в очереди B, нс (входит в `sverka_ns`).
    sverka_zhdala_b_ns,
    /// Сверка ждала места в очереди C, нс (входит в `sverka_ns`).
    sverka_zhdala_c_ns,

    /// Чтение у читателей дисков: время чтения файлов в память, нс.
    chtenie_ns,
    /// Чтение: число потоков-читателей.
    chtenie_potokov,
    /// Чтение: сколько файлов прочитано в память.
    chtenie_faylov,
    /// Чтение: сколько байтов прочитано.
    chtenie_bayt,
    /// Читатели ждали места в очереди разбора, нс.
    chtenie_zhdali_ns,
    /// Распределитель ждал места в очереди диска, нс.
    raspredelitel_zhdal_ns,

    /// Разбор: время внутри `submit_task` (в процессе или во внешнем рабочем процессе), нс.
    razbor_ns,
    /// Разбор: число потоков разбора.
    razbor_potokov,
    /// Разбор: сколько файлов разобрано.
    razbor_faylov,
    /// Потоки разбора ждали задания (читатели, сверка и обход не успевают), нс.
    razbor_zhdal_zadaniy_ns,
    /// Потоки разбора стояли на отправке в C (писатель не успевает), нс.
    razbor_zhdal_c_ns,
    /// Чтение члена архива перед разбором, нс.
    razbor_arhiv_ns,

    /// Внешние рабочие процессы: сколько заданий передано.
    protsess_zadach,
    /// Сериализация заголовка задания в JSON (данные идут сырыми байтами), нс.
    protsess_serializ_ns,
    /// Запись задания в канал рабочего процесса, нс.
    protsess_zapis_ns,
    /// Ожидание ответа от рабочего процесса, нс (вместе с его работой).
    protsess_otvet_ns,
    /// Разбор JSON-ответа в основном процессе, нс.
    protsess_razbor_otveta_ns,
    /// Работа самого рабочего процесса: извлечение текста, нс.
    protsess_rabochiy_izvlechenie_ns,
    /// Работа рабочего процесса: чтение задания (разбор заголовка и получение данных), нс.
    protsess_rabochiy_dekodirovanie_ns,
    /// Сколько байтов (заголовки JSON и данные) передано рабочим процессам.
    protsess_bayt,

    /// Писатель: сколько записей о файлах записано.
    pisatel_zapisey,
    /// Писатель: подготовка записей (проверка файла, сборка записи), нс.
    pisatel_podgotovka_ns,
    /// Писатель: работа с базой SQLite, нс.
    pisatel_baza_ns,
    /// Писатель: добавление документов в индекс, нс.
    pisatel_indeks_dobavlenie_ns,
    /// Писатель: сколько документов добавлено в индекс.
    pisatel_dokumentov,
    /// Писатель: удаление документов из индекса, нс.
    pisatel_indeks_udalenie_ns,
    /// Писатель: фиксация индекса (commit), нс.
    pisatel_fiksaciya_ns,
    /// Писатель: сколько раз индекс зафиксирован.
    pisatel_fiksaciy,
    /// Писатель ждал данных из очереди C (разбор не успевает), нс.
    pisatel_zhdal_ns,
}

/// Прибавляет длительность к счётчику.
pub fn dobavit(schetchik: &AtomicU64, dlitelnost: Duration) {
    schetchik.fetch_add(dlitelnost.as_nanos() as u64, Ordering::Relaxed);
}

/// Прибавляет единицу или заданное число к счётчику количества.
pub fn pribavit(schetchik: &AtomicU64, n: u64) {
    schetchik.fetch_add(n, Ordering::Relaxed);
}

/// Измеритель: при уничтожении прибавляет прошедшее время к счётчику
/// (работает и при `continue`, `break` и `?`).
pub struct Izmeritel<'a> {
    schetchik: &'a AtomicU64,
    nachalo: Instant,
}

impl<'a> Izmeritel<'a> {
    pub fn new(schetchik: &'a AtomicU64) -> Self {
        Self {
            schetchik,
            nachalo: Instant::now(),
        }
    }
}

impl Drop for Izmeritel<'_> {
    fn drop(&mut self) {
        dobavit(self.schetchik, self.nachalo.elapsed());
    }
}

/// Блокирующая отправка; время ожидания считается, только если очередь была полна.
pub fn otpravit_s_uchetom<T>(
    tx: &Sender<T>,
    znachenie: T,
    zhdal: &AtomicU64,
) -> Result<(), SendError<T>> {
    match tx.try_send(znachenie) {
        Ok(()) => Ok(()),
        Err(TrySendError::Disconnected(v)) => Err(SendError(v)),
        Err(TrySendError::Full(v)) => {
            let nachalo = Instant::now();
            let itog = tx.send(v);
            dobavit(zhdal, nachalo.elapsed());
            itog
        }
    }
}

/// Блокирующее получение; время ожидания считается, только если очередь была пуста.
pub fn poluchit_s_uchetom<T>(rx: &Receiver<T>, zhdal: &AtomicU64) -> Result<T, RecvError> {
    match rx.try_recv() {
        Ok(v) => Ok(v),
        Err(TryRecvError::Disconnected) => Err(RecvError),
        Err(TryRecvError::Empty) => {
            let nachalo = Instant::now();
            let itog = rx.recv();
            dobavit(zhdal, nachalo.elapsed());
            itog
        }
    }
}

/// Получение с предельным ожиданием; время ожидания считается, только если очередь была пуста.
pub fn poluchit_s_tajmautom<T>(
    rx: &Receiver<T>,
    predel: Duration,
    zhdal: &AtomicU64,
) -> Result<T, RecvTimeoutError> {
    match rx.try_recv() {
        Ok(v) => Ok(v),
        Err(TryRecvError::Disconnected) => Err(RecvTimeoutError::Disconnected),
        Err(TryRecvError::Empty) => {
            let nachalo = Instant::now();
            let itog = rx.recv_timeout(predel);
            dobavit(zhdal, nachalo.elapsed());
            itog
        }
    }
}

/// Сколько самых долгих файлов хранить.
pub const DOLGIH_FAYLOV: usize = 20;

/// Название расширения для файлов без расширения.
const NET_RASSHIRENIYA: &str = "(нет)";

/// Один разобранный файл: чем он запомнился по времени.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DolgiyFayl {
    pub put: String,
    pub rasshirenie: String,
    pub bayt: u64,
    pub ns: u64,
}

/// Итог разбора по одному расширению.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItogFormata {
    pub rasshirenie: String,
    pub faylov: u64,
    pub bayt: u64,
    pub ns: u64,
    /// Сколько файлов разобрались с ошибкой или по тайм-ауту.
    pub neudach: u64,
    /// Самый долгий файл этого расширения.
    pub samyy_dolgiy: Option<DolgiyFayl>,
}

/// Снимок разбора по форматам: расширения по убыванию времени и самые долгие файлы.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RazborPoFormatam {
    /// По убыванию суммарного времени разбора.
    pub formaty: Vec<ItogFormata>,
    /// До `DOLGIH_FAYLOV` самых долгих файлов, самый долгий первым.
    pub dolgie: Vec<DolgiyFayl>,
}

#[derive(Debug, Default)]
struct VnutriFormatov {
    po_rasshireniyam: HashMap<String, ItogFormata>,
    dolgie: Vec<DolgiyFayl>,
}

/// Сборщик «разбор по форматам». Внутри `Mutex`: файлов десятки тысяч, блокировка на файл
/// не заметна на фоне разбора.
#[derive(Debug, Default)]
pub struct SborshchikFormatov {
    vnutri: Mutex<VnutriFormatov>,
}

impl SborshchikFormatov {
    /// Записывает один разобранный файл: расширение (любой регистр, пустое — «(нет)»),
    /// размер, время внутри `submit_task`, путь и признак неудачи или тайм-аута.
    pub fn zapisat(&self, rasshirenie: &str, bayt: u64, ns: u64, put: &str, neudacha: bool) {
        let rasshirenie = if rasshirenie.is_empty() {
            NET_RASSHIRENIYA.to_string()
        } else {
            rasshirenie.to_lowercase()
        };
        let mut v = self.vnutri.lock().unwrap_or_else(|e| e.into_inner());
        let VnutriFormatov {
            po_rasshireniyam,
            dolgie,
        } = &mut *v;

        // Путь копируем, только если файл попадёт в один из «самых долгих».
        let v_obshchiy_top =
            dolgie.len() < DOLGIH_FAYLOV || dolgie.last().is_some_and(|f| ns > f.ns);
        let itog = po_rasshireniyam
            .entry(rasshirenie.clone())
            .or_insert_with(|| ItogFormata {
                rasshirenie: rasshirenie.clone(),
                ..Default::default()
            });
        itog.faylov += 1;
        itog.bayt += bayt;
        itog.ns += ns;
        itog.neudach += u64::from(neudacha);
        let samyy_dolgiy = itog.samyy_dolgiy.as_ref().is_none_or(|f| ns > f.ns);

        if !(v_obshchiy_top || samyy_dolgiy) {
            return;
        }
        let fayl = DolgiyFayl {
            put: put.to_string(),
            rasshirenie,
            bayt,
            ns,
        };
        if v_obshchiy_top {
            // Список всегда отсортирован по убыванию времени.
            let mesto = dolgie.partition_point(|f| f.ns >= ns);
            dolgie.insert(mesto, fayl.clone());
            dolgie.truncate(DOLGIH_FAYLOV);
        }
        if samyy_dolgiy {
            itog.samyy_dolgiy = Some(fayl);
        }
    }

    /// Снимок: расширения по убыванию времени (при равенстве — по названию).
    pub fn snimok(&self) -> RazborPoFormatam {
        let v = self.vnutri.lock().unwrap_or_else(|e| e.into_inner());
        let mut formaty: Vec<ItogFormata> = v.po_rasshireniyam.values().cloned().collect();
        formaty.sort_by(|a, b| {
            b.ns.cmp(&a.ns)
                .then_with(|| a.rasshirenie.cmp(&b.rasshirenie))
        });
        RazborPoFormatam {
            formaty,
            dolgie: v.dolgie.clone(),
        }
    }
}

const MB: f64 = 1024.0 * 1024.0;

/// Число с запятой и заданным числом знаков.
fn chislo(x: f64, znakov: usize) -> String {
    format!("{x:.znakov$}").replace('.', ",")
}

impl RazborPoFormatam {
    /// Сумма времени разбора по всем форматам, нс.
    pub fn vsego_ns(&self) -> u64 {
        self.formaty.iter().map(|f| f.ns).sum()
    }

    /// Первые `n` форматов по времени; если есть остальные — последней строкой «прочие».
    pub fn stroki_formatov(&self, n: usize) -> Vec<ItogFormata> {
        let mut stroki: Vec<ItogFormata> = self.formaty.iter().take(n).cloned().collect();
        let ostalnye = self.formaty.get(n..).unwrap_or_default();
        if !ostalnye.is_empty() {
            let mut prochie = ItogFormata {
                rasshirenie: format!("прочие ({})", ostalnye.len()),
                ..Default::default()
            };
            for f in ostalnye {
                prochie.faylov += f.faylov;
                prochie.bayt += f.bayt;
                prochie.ns += f.ns;
                prochie.neudach += f.neudach;
            }
            stroki.push(prochie);
        }
        stroki
    }

    /// Таблица «формат | файлов | МБ | разбор, с | доля разбора % | мс на файл | МБ/с | неудач»:
    /// `n` форматов с наибольшим временем и строка «прочие».
    pub fn tablica_formatov(&self, n: usize) -> String {
        let vsego = self.vsego_ns().max(1) as f64;
        let mut s = String::new();
        let _ = writeln!(
            s,
            "{:<18}{:>9}{:>11}{:>12}{:>16}{:>12}{:>9}{:>8}",
            "формат", "файлов", "МБ", "разбор, с", "доля разбора %", "мс на файл", "МБ/с", "неудач"
        );
        for f in self.stroki_formatov(n) {
            let mb = f.bayt as f64 / MB;
            let sekund = f.ns as f64 / 1e9;
            let ms_na_fayl = if f.faylov > 0 {
                f.ns as f64 / 1e6 / f.faylov as f64
            } else {
                0.0
            };
            let mb_v_sek = if f.ns > 0 { mb / sekund } else { 0.0 };
            let _ = writeln!(
                s,
                "{:<18}{:>9}{:>11}{:>12}{:>16}{:>12}{:>9}{:>8}",
                f.rasshirenie,
                f.faylov,
                chislo(mb, 1),
                chislo(sekund, 2),
                chislo(f.ns as f64 / vsego * 100.0, 1),
                chislo(ms_na_fayl, 1),
                chislo(mb_v_sek, 1),
                f.neudach,
            );
        }
        s.trim_end().to_string()
    }

    /// Список «20 самых долгих файлов»: мс, размер МБ, расширение, путь.
    pub fn tablica_dolgih(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{DOLGIH_FAYLOV} самых долгих файлов:");
        let _ = writeln!(s, "{:>12}{:>11}  {:<10}путь", "мс", "размер, МБ", "формат");
        for f in &self.dolgie {
            let _ = writeln!(
                s,
                "{:>12}{:>11}  {:<10}{}",
                chislo(f.ns as f64 / 1e6, 1),
                chislo(f.bayt as f64 / MB, 2),
                f.rasshirenie,
                f.put,
            );
        }
        s.trim_end().to_string()
    }
}

/// Секунды с запятой и двумя знаками.
fn sek(ns: u64) -> String {
    format!("{:.2}", ns as f64 / 1e9).replace('.', ",")
}

/// Доля в процентах с одним знаком.
fn procent(dolya: f64) -> String {
    format!("{:.1}%", dolya * 100.0).replace('.', ",")
}

impl OtchetEtapov {
    fn wall(&self) -> f64 {
        self.wall_ns.max(1) as f64
    }

    /// Занятость: сумма / (время прогона × потоков).
    fn zanyatost(&self, ns: u64, potokov: u64) -> f64 {
        ns as f64 / (self.wall() * potokov.max(1) as f64)
    }

    /// Время сверки без ожидания места в B и C.
    pub fn sverka_chistoe_ns(&self) -> u64 {
        self.sverka_ns
            .saturating_sub(self.sverka_zhdala_b_ns)
            .saturating_sub(self.sverka_zhdala_c_ns)
    }

    /// Время писателя без ожидания: подготовка, база, индекс, фиксация.
    pub fn pisatel_ns(&self) -> u64 {
        self.pisatel_podgotovka_ns
            + self.pisatel_baza_ns
            + self.pisatel_indeks_dobavlenie_ns
            + self.pisatel_indeks_udalenie_ns
            + self.pisatel_fiksaciya_ns
    }

    /// Время разбора вне обмена с процессом: очередь пула и переключение потоков.
    pub fn razbor_prochee_ns(&self) -> u64 {
        self.razbor_ns
            .saturating_sub(self.protsess_serializ_ns)
            .saturating_sub(self.protsess_zapis_ns)
            .saturating_sub(self.protsess_otvet_ns)
            .saturating_sub(self.protsess_razbor_otveta_ns)
    }

    /// Занятость этапов, которые были в прогоне: (название, занятость 0..1).
    pub fn zanyatost_etapov(&self) -> Vec<(&'static str, f64)> {
        let mut etapy = Vec::new();
        if self.obhod_potokov > 0 {
            etapy.push(("обход", self.zanyatost(self.obhod_ns, self.obhod_potokov)));
        }
        if self.sverka_faylov > 0 {
            etapy.push(("сверка", self.zanyatost(self.sverka_chistoe_ns(), 1)));
        }
        if self.chtenie_potokov > 0 {
            etapy.push((
                "чтение",
                self.zanyatost(self.chtenie_ns, self.chtenie_potokov),
            ));
        }
        if self.razbor_potokov > 0 {
            etapy.push((
                "разбор",
                self.zanyatost(self.razbor_ns, self.razbor_potokov),
            ));
        }
        if self.pisatel_zapisey > 0 {
            etapy.push(("писатель", self.zanyatost(self.pisatel_ns(), 1)));
        }
        etapy
    }

    /// Заметные ожидания: (доля, пояснение). Порядок — от конца конвейера к началу:
    /// ожидание у писателя обычно причина, ожидание выше по течению — следствие.
    pub fn ozhidaniya(&self) -> Vec<(f64, String)> {
        let mut spisok = Vec::new();
        let mut dobavit_ozh = |dolya: f64, tekst: &str| {
            if dolya >= POROG_OZHIDANIYA {
                spisok.push((dolya, format!("{tekst} {}", procent(dolya))));
            }
        };
        dobavit_ozh(
            self.zanyatost(self.razbor_zhdal_c_ns, self.razbor_potokov),
            "потоки разбора стоят на отправке в C — писатель не успевает:",
        );
        dobavit_ozh(
            self.zanyatost(self.pisatel_zhdal_ns, 1),
            "писатель ждёт данные из C — разбор (и всё до него) не успевает:",
        );
        dobavit_ozh(
            self.zanyatost(self.chtenie_zhdali_ns, self.chtenie_potokov),
            "читатели ждут места в очереди разбора — разбор не успевает за чтением:",
        );
        dobavit_ozh(
            self.zanyatost(self.razbor_zhdal_zadaniy_ns, self.razbor_potokov),
            "потоки разбора ждут заданий — чтение, сверка или обход не успевают:",
        );
        dobavit_ozh(
            self.zanyatost(self.sverka_zhdala_a_ns, 1),
            "сверка ждёт записей из обхода — обход не успевает:",
        );
        dobavit_ozh(
            self.zanyatost(self.obhod_zhdal_a_ns, self.obhod_potokov),
            "обход ждёт места в очереди A — сверка не успевает:",
        );
        spisok
    }

    /// Строка «узкое место: …»: этап с наибольшей занятостью и где копится ожидание.
    pub fn uzkoe_mesto(&self) -> String {
        let etapy = self.zanyatost_etapov();
        let Some((nazvanie, zanyatost)) = etapy.iter().copied().max_by(|a, b| a.1.total_cmp(&b.1))
        else {
            return "узкое место: не определить (этапы не работали)".to_string();
        };
        let mut s = format!("узкое место: {nazvanie} (занятость {})", procent(zanyatost));
        let ozhidaniya = self.ozhidaniya();
        if ozhidaniya.is_empty() {
            s.push_str("; заметного ожидания в очередях нет");
        } else {
            s.push_str("; ожидание копится: ");
            let teksty: Vec<&str> = ozhidaniya.iter().map(|(_, t)| t.as_str()).collect();
            s.push_str(&teksty.join("; "));
        }
        s
    }

    /// Одна строка таблицы: название, потоки, сумма, доля прогона, занятость.
    fn stroka(&self, s: &mut String, nazvanie: &str, potokov: u64, ns: u64) {
        let potokov_text = if potokov > 0 {
            potokov.to_string()
        } else {
            String::new()
        };
        let zanyatost_text = if potokov > 0 {
            procent(self.zanyatost(ns, potokov))
        } else {
            String::new()
        };
        let _ = writeln!(
            s,
            "{nazvanie:<52}{potokov_text:>8}{:>11}{:>14}{zanyatost_text:>11}",
            sek(ns),
            procent(ns as f64 / self.wall()),
        );
    }

    /// Таблица времени этапов: сумма, доля от времени прогона, занятость.
    pub fn tablica(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "Время прогона конвейера: {} с", sek(self.wall_ns));
        let _ = writeln!(
            s,
            "{:<52}{:>8}{:>11}{:>14}{:>11}",
            "Этап", "потоков", "сумма, с", "доля прогона", "занятость"
        );
        self.stroka(&mut s, "обход", self.obhod_potokov, self.obhod_ns);
        self.stroka(&mut s, "сверка", 1, self.sverka_chistoe_ns());
        if self.chtenie_potokov > 0 {
            self.stroka(
                &mut s,
                "чтение у читателей",
                self.chtenie_potokov,
                self.chtenie_ns,
            );
        }
        self.stroka(
            &mut s,
            "разбор (внутри submit_task)",
            self.razbor_potokov,
            self.razbor_ns,
        );
        if self.protsess_zadach > 0 {
            let p = self.razbor_potokov;
            self.stroka(
                &mut s,
                "  сериализация задания (заголовок JSON)",
                p,
                self.protsess_serializ_ns,
            );
            self.stroka(
                &mut s,
                "  запись в канал рабочего процесса",
                p,
                self.protsess_zapis_ns,
            );
            self.stroka(
                &mut s,
                "  ожидание ответа процесса, всего",
                p,
                self.protsess_otvet_ns,
            );
            self.stroka(
                &mut s,
                "    из него: разбор файла в процессе",
                p,
                self.protsess_rabochiy_izvlechenie_ns,
            );
            self.stroka(
                &mut s,
                "    из него: чтение задания в процессе",
                p,
                self.protsess_rabochiy_dekodirovanie_ns,
            );
            self.stroka(
                &mut s,
                "  разбор JSON-ответа",
                p,
                self.protsess_razbor_otveta_ns,
            );
            self.stroka(
                &mut s,
                "  прочее: очередь пула, переключение потоков",
                p,
                self.razbor_prochee_ns(),
            );
        }
        if self.razbor_arhiv_ns > 0 {
            self.stroka(
                &mut s,
                "  чтение члена архива перед разбором",
                self.razbor_potokov,
                self.razbor_arhiv_ns,
            );
        }
        self.stroka(&mut s, "писатель, всего", 1, self.pisatel_ns());
        self.stroka(
            &mut s,
            "  подготовка записей",
            1,
            self.pisatel_podgotovka_ns,
        );
        self.stroka(&mut s, "  база SQLite", 1, self.pisatel_baza_ns);
        self.stroka(
            &mut s,
            "  индекс: добавление документов",
            1,
            self.pisatel_indeks_dobavlenie_ns,
        );
        self.stroka(
            &mut s,
            "  индекс: удаление документов",
            1,
            self.pisatel_indeks_udalenie_ns,
        );
        self.stroka(
            &mut s,
            "  индекс: фиксация (commit)",
            1,
            self.pisatel_fiksaciya_ns,
        );
        let _ = writeln!(s, "Ожидание:");
        self.stroka(
            &mut s,
            "  обход ждал места в очереди A",
            self.obhod_potokov,
            self.obhod_zhdal_a_ns,
        );
        self.stroka(
            &mut s,
            "  сверка ждала записей из A",
            1,
            self.sverka_zhdala_a_ns,
        );
        self.stroka(
            &mut s,
            "  сверка ждала места в B",
            1,
            self.sverka_zhdala_b_ns,
        );
        self.stroka(
            &mut s,
            "  сверка ждала места в C",
            1,
            self.sverka_zhdala_c_ns,
        );
        if self.chtenie_potokov > 0 {
            self.stroka(
                &mut s,
                "  читатели ждали места в очереди разбора",
                self.chtenie_potokov,
                self.chtenie_zhdali_ns,
            );
        }
        self.stroka(
            &mut s,
            "  разбор ждал заданий",
            self.razbor_potokov,
            self.razbor_zhdal_zadaniy_ns,
        );
        self.stroka(
            &mut s,
            "  разбор ждал места в C (писатель не успевает)",
            self.razbor_potokov,
            self.razbor_zhdal_c_ns,
        );
        self.stroka(
            &mut s,
            "  писатель ждал данных из C (разбор не успевает)",
            1,
            self.pisatel_zhdal_ns,
        );
        let _ = writeln!(
            s,
            "Файлов: обход {}, сверка {}, прочитано читателями {} ({} МБ), разобрано {}, \
             записей {}, документов в индекс {}, фиксаций индекса {}",
            self.obhod_faylov,
            self.sverka_faylov,
            self.chtenie_faylov,
            format!("{:.1}", self.chtenie_bayt as f64 / (1024.0 * 1024.0)).replace('.', ","),
            self.razbor_faylov,
            self.pisatel_zapisey,
            self.pisatel_dokumentov,
            self.pisatel_fiksaciy,
        );
        let _ = write!(s, "{}", self.uzkoe_mesto());
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn otchet() -> OtchetEtapov {
        OtchetEtapov {
            wall_ns: 10_000_000_000,
            obhod_potokov: 1,
            obhod_ns: 1_000_000_000,
            sverka_faylov: 100,
            sverka_ns: 500_000_000,
            razbor_potokov: 4,
            razbor_ns: 38_000_000_000,
            razbor_faylov: 100,
            pisatel_zapisey: 100,
            pisatel_baza_ns: 2_000_000_000,
            pisatel_zhdal_ns: 7_000_000_000,
            razbor_zhdal_c_ns: 100_000_000,
            ..Default::default()
        }
    }

    #[test]
    fn uzkoe_mesto_beret_samyy_zanyatyy_etap() {
        let s = otchet().uzkoe_mesto();
        assert!(
            s.starts_with("узкое место: разбор (занятость 95,0%)"),
            "{s}"
        );
        assert!(s.contains("писатель ждёт данные из C"), "{s}");
        assert!(!s.contains("стоят на отправке в C"), "{s}");
    }

    #[test]
    fn tablica_soderzhit_etapy_i_stroku_uzkogo_mesta() {
        let t = otchet().tablica();
        for chast in ["обход", "сверка", "разбор", "писатель", "узкое место:"]
        {
            assert!(t.contains(chast), "нет «{chast}» в таблице:\n{t}");
        }
        assert!(!t.contains("сериализация задания"), "{t}");
    }

    #[test]
    fn pustoy_otchet_ne_padaet() {
        let s = OtchetEtapov::default().uzkoe_mesto();
        assert!(s.contains("не определить"), "{s}");
        let _ = OtchetEtapov::default().tablica();
    }

    #[test]
    fn sborshchik_formatov_sortiruet_i_derzhit_top() {
        let sb = SborshchikFormatov::default();
        // 30 файлов pdf по 1..=30 мс; расширение в разном регистре сливается
        for i in 1..=30u64 {
            let ext = if i % 2 == 0 { "PDF" } else { "pdf" };
            sb.zapisat(ext, 1000, i * 1_000_000, &format!("a{i}.pdf"), i == 30);
        }
        sb.zapisat("", 10, 500, "Makefile", false);
        sb.zapisat("txt", 5, 100_000_000, "big.txt", false);
        let r = sb.snimok();
        assert_eq!(r.formaty.len(), 3);
        // pdf: 465 мс, txt: 100 мс, (нет): 0,0005 мс
        assert_eq!(r.formaty[0].rasshirenie, "pdf");
        assert_eq!(r.formaty[0].faylov, 30);
        assert_eq!(r.formaty[0].bayt, 30_000);
        assert_eq!(r.formaty[0].neudach, 1);
        assert_eq!(r.formaty[0].samyy_dolgiy.as_ref().unwrap().put, "a30.pdf");
        assert_eq!(r.formaty[1].rasshirenie, "txt");
        assert_eq!(r.formaty[2].rasshirenie, "(нет)");
        // top-20: txt 100 мс, затем pdf 30, 29, ... 12 мс (19 файлов)
        assert_eq!(r.dolgie.len(), DOLGIH_FAYLOV);
        assert_eq!(r.dolgie[0].put, "big.txt");
        assert_eq!(r.dolgie[1].put, "a30.pdf");
        assert_eq!(r.dolgie[19].put, "a12.pdf");
        assert!(r.dolgie.windows(2).all(|p| p[0].ns >= p[1].ns));
    }

    #[test]
    fn stroki_formatov_dobavlyayut_prochie() {
        let sb = SborshchikFormatov::default();
        for (i, ext) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            sb.zapisat(ext, 100, (10 - i as u64) * 1_000_000, "p", i == 4);
        }
        let r = sb.snimok();
        let stroki = r.stroki_formatov(3);
        assert_eq!(stroki.len(), 4);
        assert_eq!(stroki[0].rasshirenie, "a");
        assert_eq!(stroki[2].rasshirenie, "c");
        let prochie = &stroki[3];
        assert!(prochie.rasshirenie.starts_with("прочие"), "{prochie:?}");
        assert_eq!(prochie.faylov, 2);
        assert_eq!(prochie.bayt, 200);
        assert_eq!(prochie.ns, 13_000_000);
        assert_eq!(prochie.neudach, 1);
        // форматов не больше n: строки «прочие» нет
        assert_eq!(r.stroki_formatov(5).len(), 5);
        let t = r.tablica_formatov(3);
        assert!(t.contains("прочие") && t.contains("доля разбора %"), "{t}");
        assert!(r.tablica_dolgih().contains("самых долгих файлов"));
        assert_eq!(
            SborshchikFormatov::default().snimok(),
            RazborPoFormatam::default()
        );
    }

    #[test]
    fn izmeritel_dobavlyaet_vremya() {
        let schetchik = AtomicU64::new(0);
        {
            let _i = Izmeritel::new(&schetchik);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(schetchik.load(Ordering::Relaxed) >= 4_000_000);
    }
}

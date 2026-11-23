//! Самообновление: проверка новой версии в открытом хранилище выпусков, скачивание установщика
//! и тихий запуск. Описание версии (`latest.json`) подписано Ed25519: без верной подписи
//! ничего не скачивается и не ставится.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::journal;

/// Версия запущенной программы.
pub const TEKUSHCHAYA_VERSIYA: &str = env!("CARGO_PKG_VERSION");

/// Адрес описания последней версии.
pub const ADRES_OPISANIYA: &str = "https://github.com/ivpetr168-hue/poiskovichek-obnovleniya/releases/latest/download/latest.json";

/// Переменная среды для проверки: полный адрес `latest.json` (подпись — он же + `.sig`).
pub const PEREMENNAYA_ADRESA: &str = "POISKOVICHEK_OBNOVLENIE_ADRES";

/// Открытый ключ Ed25519 (32 байта, hex), которым подписан `latest.json`.
pub const OTKRYTYI_KLYUCH_HEX: &str =
    "ffb9b47fbbb1d582b2a24f2e576ddde2af9aa260b3e126ccf430c5a49f42e934";

/// Установщик разрешено скачивать только отсюда.
pub const PREFIKS_USTANOVSHCHIKA: &str =
    "https://github.com/ivpetr168-hue/poiskovichek-obnovleniya/";

/// Предел времени на запрос описания и подписи.
const PREDEL_ZAPROSA: Duration = Duration::from_secs(20);
/// Описание версии — маленький файл; больше не читаем.
const PREDEL_OPISANIYA: u64 = 64 * 1024;

/// Первая фоновая проверка — через 10 с после запуска, затем каждые 6 часов.
const PERVAYA_PROVERKA: Duration = Duration::from_secs(10);
const INTERVAL_PROVERKI: Duration = Duration::from_secs(6 * 60 * 60);

/// Ключи тихой установки.
pub const KLYUCHI_USTANOVKI: [&str; 3] = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"];

/// Содержимое `latest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Opisanie {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    #[serde(default)]
    pub notes: String,
}

/// Итог проверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Itog {
    /// Выпущенная версия не новее запущенной.
    NetObnovleniy,
    /// Вышла более новая версия.
    Est(Opisanie),
}

/// События фоновых работ для окна.
#[derive(Debug)]
pub enum Sobytie {
    Proverka {
        /// Проверку запросил пользователь через меню.
        ruchnaya: bool,
        itog: Result<Itog, String>,
    },
    /// Ход скачивания: получено и всего байт.
    Hod {
        gotovo: u64,
        vsego: u64,
    },
    /// Установщик скачан и проверен.
    Skachano(PathBuf),
    OshibkaSkachivaniya(String),
    /// Скачивание отменено пользователем.
    Otmeneno,
}

/// Адрес описания: из переменной среды (проверка) или постоянный.
fn adres_opisaniya() -> (String, bool) {
    match std::env::var(PEREMENNAYA_ADRESA) {
        Ok(a) if !a.trim().is_empty() => (a.trim().to_string(), true),
        _ => (ADRES_OPISANIYA.to_string(), false),
    }
}

/// Версия — числа через точку: «0.10.0». Всё остальное — отказ.
pub fn razobrat_versiyu(s: &str) -> Option<Vec<u64>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.split('.')
        .map(|chast| {
            if chast.is_empty() || !chast.bytes().all(|b| b.is_ascii_digit()) {
                None
            } else {
                chast.parse::<u64>().ok()
            }
        })
        .collect()
}

/// `novaya` строго новее `tekushchaya` (0.10.0 > 0.9.0; недостающие части — нули).
pub fn versiya_novee(novaya: &str, tekushchaya: &str) -> Result<bool, String> {
    let n =
        razobrat_versiyu(novaya).ok_or_else(|| format!("непонятный номер версии «{novaya}»"))?;
    let t = razobrat_versiyu(tekushchaya)
        .ok_or_else(|| format!("непонятный номер версии «{tekushchaya}»"))?;
    let dlina = n.len().max(t.len());
    for i in 0..dlina {
        let a = n.get(i).copied().unwrap_or(0);
        let b = t.get(i).copied().unwrap_or(0);
        if a != b {
            return Ok(a > b);
        }
    }
    Ok(false)
}

/// Проверяет подпись Ed25519 (64 байта) файла `dannye` открытым ключом.
pub fn proverit_podpis(dannye: &[u8], podpis: &[u8], klyuch: &[u8; 32]) -> Result<(), String> {
    let klyuch =
        VerifyingKey::from_bytes(klyuch).map_err(|_| "неверный открытый ключ".to_string())?;
    let podpis: [u8; 64] = podpis
        .try_into()
        .map_err(|_| format!("подпись должна быть 64 байта, получено {}", podpis.len()))?;
    klyuch
        .verify(dannye, &Signature::from_bytes(&podpis))
        .map_err(|_| "подпись не сошлась".to_string())
}

/// Проверка подписи встроенным открытым ключом.
pub fn proverit_podpis_programmy(dannye: &[u8], podpis: &[u8]) -> Result<(), String> {
    let mut klyuch = [0u8; 32];
    hex::decode_to_slice(OTKRYTYI_KLYUCH_HEX, &mut klyuch)
        .map_err(|_| "встроенный ключ повреждён".to_string())?;
    proverit_podpis(dannye, podpis, &klyuch)
}

/// Разбирает `latest.json` и проверяет, что поля пригодны.
pub fn razobrat_opisanie(dannye: &[u8]) -> Result<Opisanie, String> {
    let op: Opisanie =
        serde_json::from_slice(dannye).map_err(|e| format!("не разобрать latest.json: {e}"))?;
    if razobrat_versiyu(&op.version).is_none() {
        return Err(format!("непонятный номер версии «{}»", op.version));
    }
    let sha_ok = op.sha256.len() == 64 && op.sha256.bytes().all(|b| b.is_ascii_hexdigit());
    if !sha_ok {
        return Err("в latest.json неверная контрольная сумма sha256".to_string());
    }
    if op.size == 0 {
        return Err("в latest.json не указан размер установщика".to_string());
    }
    Ok(op)
}

/// Адрес установщика допустим: только наше хранилище; при подмене адреса для проверки
/// ещё и локальный `http://127.0.0.1`.
pub fn adres_ustanovshchika_dopustim(url: &str, podmena: bool) -> bool {
    if url.starts_with(PREFIKS_USTANOVSHCHIKA) {
        return true;
    }
    podmena && (url.starts_with("http://127.0.0.1:") || url.starts_with("http://127.0.0.1/"))
}

fn agent(vsyo_vremya: bool) -> ureq::Agent {
    // Прокси берутся из системных переменных среды (так по умолчанию в ureq)
    let mut b = ureq::Agent::config_builder();
    b = if vsyo_vremya {
        b.timeout_global(Some(PREDEL_ZAPROSA))
    } else {
        // Скачивание долгое: ограничиваем ожидание подключения и ответа, а не всё время
        b.timeout_connect(Some(PREDEL_ZAPROSA))
            .timeout_recv_response(Some(PREDEL_ZAPROSA))
    };
    b.build().into()
}

fn skachat_nebolshoe(url: &str) -> Result<Vec<u8>, String> {
    let mut otvet = agent(true)
        .get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    otvet
        .body_mut()
        .with_config()
        .limit(PREDEL_OPISANIYA)
        .read_to_vec()
        .map_err(|e| format!("{url}: {e}"))
}

/// Проверяет, вышла ли новая версия: скачивает `latest.json` и подпись, сверяет подпись.
pub fn proverit(tekushchaya: &str) -> Result<Itog, String> {
    let (adres, _) = adres_opisaniya();
    let dannye = skachat_nebolshoe(&adres)?;
    let podpis = skachat_nebolshoe(&format!("{adres}.sig"))?;
    proverit_podpis_programmy(&dannye, &podpis)?;
    let op = razobrat_opisanie(&dannye)?;
    if versiya_novee(&op.version, tekushchaya)? {
        Ok(Itog::Est(op))
    } else {
        Ok(Itog::NetObnovleniy)
    }
}

/// Папка для скачанных установщиков.
pub fn papka_obnovleniya() -> PathBuf {
    std::env::temp_dir().join("ПоисКовичеК-обновление")
}

/// Удаляет в папке скачанные установщики `Poiskovichek-setup-*.exe`, затем саму папку, если она
/// пуста. Чужие файлы не трогает; ошибки удаления пропускаются. Возвращает число удалённых файлов.
pub fn ubrat_ustanovshchiki(papka: &Path) -> usize {
    let Ok(zapisi) = fs::read_dir(papka) else {
        return 0;
    };
    let mut udaleno = 0;
    for zapis in zapisi.flatten() {
        let imya = zapis.file_name();
        let Some(imya) = imya.to_str() else {
            continue;
        };
        if !(imya.starts_with("Poiskovichek-setup-") && imya.ends_with(".exe")) {
            continue;
        }
        match fs::remove_file(zapis.path()) {
            Ok(()) => udaleno += 1,
            Err(e) => journal::warn("obnovlenie", &format!("не удалён установщик {imya}: {e}")),
        }
    }
    // remove_dir не удаляет непустую папку — ошибка здесь ожидаема
    let _ = fs::remove_dir(papka);
    udaleno
}

/// Скачивает установщик, сообщая ход через `hod(получено, всего)`. Проверяет размер и sha256;
/// при несовпадении или отмене файл удаляется. `Ok(None)` — отмена.
pub fn skachat_ustanovshchik(
    op: &Opisanie,
    otmena: &AtomicBool,
    mut hod: impl FnMut(u64, u64),
) -> Result<Option<PathBuf>, String> {
    let (_, podmena) = adres_opisaniya();
    if !adres_ustanovshchika_dopustim(&op.url, podmena) {
        return Err(format!(
            "адрес установщика не из хранилища обновлений: {}",
            op.url
        ));
    }
    let versiya = razobrat_versiyu(&op.version)
        .ok_or_else(|| format!("непонятный номер версии «{}»", op.version))?
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(".");
    let papka = papka_obnovleniya();
    fs::create_dir_all(&papka).map_err(|e| format!("не создать папку {}: {e}", papka.display()))?;
    let put = papka.join(format!("Poiskovichek-setup-{versiya}.exe"));

    let itog = zapisat_i_proverit(op, &put, otmena, &mut hod);
    match itog {
        Ok(true) => Ok(Some(put)),
        Ok(false) => {
            let _ = fs::remove_file(&put);
            Ok(None)
        }
        Err(e) => {
            let _ = fs::remove_file(&put);
            Err(e)
        }
    }
}

/// `Ok(false)` — отменено.
fn zapisat_i_proverit(
    op: &Opisanie,
    put: &Path,
    otmena: &AtomicBool,
    hod: &mut dyn FnMut(u64, u64),
) -> Result<bool, String> {
    let otvet = agent(false)
        .get(&op.url)
        .call()
        .map_err(|e| format!("скачивание установщика: {e}"))?;
    let mut chitat = otvet.into_body().into_reader();
    let mut fayl =
        fs::File::create(put).map_err(|e| format!("не создать {}: {e}", put.display()))?;
    let mut sha = Sha256::new();
    let mut bufer = vec![0u8; 64 * 1024];
    let mut polucheno = 0u64;
    let mut poslednee_soobshchenie = Instant::now() - Duration::from_secs(1);
    hod(0, op.size);
    loop {
        if otmena.load(Ordering::SeqCst) {
            return Ok(false);
        }
        let n = chitat
            .read(&mut bufer)
            .map_err(|e| format!("скачивание установщика: {e}"))?;
        if n == 0 {
            break;
        }
        polucheno += n as u64;
        if polucheno > op.size {
            return Err("установщик больше заявленного размера".to_string());
        }
        sha.update(&bufer[..n]);
        fayl.write_all(&bufer[..n])
            .map_err(|e| format!("запись установщика: {e}"))?;
        if poslednee_soobshchenie.elapsed() >= Duration::from_millis(100) {
            poslednee_soobshchenie = Instant::now();
            hod(polucheno, op.size);
        }
    }
    fayl.flush()
        .map_err(|e| format!("запись установщика: {e}"))?;
    drop(fayl);
    hod(polucheno, op.size);
    if polucheno != op.size {
        return Err(format!(
            "размер установщика не сошёлся: получено {polucheno}, ожидалось {}",
            op.size
        ));
    }
    let hesh = hex::encode(sha.finalize());
    if !hesh.eq_ignore_ascii_case(&op.sha256) {
        return Err("контрольная сумма установщика не сошлась".to_string());
    }
    Ok(true)
}

/// Запускает установщик отдельным процессом, не связанным с окном программы.
pub fn zapustit_ustanovshchik(put: &Path) -> io::Result<()> {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new(put)
        .args(KLYUCHI_USTANOVKI)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .spawn()
        .map(|_| ())
}

fn pererisovat(ctx: &OnceLock<egui::Context>) {
    if let Some(c) = ctx.get() {
        c.request_repaint();
    }
}

/// Одна проверка в фоне (для меню «Проверить обновления»).
pub fn proverit_v_fone(tx: Sender<Sobytie>, ctx: Arc<OnceLock<egui::Context>>) {
    std::thread::spawn(move || {
        let itog = proverit(TEKUSHCHAYA_VERSIYA);
        let _ = tx.send(Sobytie::Proverka {
            ruchnaya: true,
            itog,
        });
        pererisovat(&ctx);
    });
}

/// Фоновая проверка: первая через 10 с после запуска, потом каждые 6 часов.
pub fn zapustit_fonovuyu_proverku(tx: Sender<Sobytie>, ctx: Arc<OnceLock<egui::Context>>) {
    std::thread::spawn(move || {
        std::thread::sleep(PERVAYA_PROVERKA);
        ubrat_ustanovshchiki(&papka_obnovleniya());
        loop {
            let itog = proverit(TEKUSHCHAYA_VERSIYA);
            if let Err(ref e) = itog {
                journal::warn("obnovlenie", &format!("проверка обновлений: {e}"));
            }
            if tx
                .send(Sobytie::Proverka {
                    ruchnaya: false,
                    itog,
                })
                .is_err()
            {
                return;
            }
            pererisovat(&ctx);
            std::thread::sleep(INTERVAL_PROVERKI);
        }
    });
}

/// Скачивание в фоне: события хода и результата идут в `tx`.
pub fn skachat_v_fone(
    op: Opisanie,
    otmena: Arc<AtomicBool>,
    tx: Sender<Sobytie>,
    ctx: Arc<OnceLock<egui::Context>>,
) {
    std::thread::spawn(move || {
        let tx_hod = tx.clone();
        let ctx_hod = ctx.clone();
        let itog = skachat_ustanovshchik(&op, &otmena, move |gotovo, vsego| {
            let _ = tx_hod.send(Sobytie::Hod { gotovo, vsego });
            pererisovat(&ctx_hod);
        });
        let soobshchenie = match itog {
            Ok(Some(put)) => Sobytie::Skachano(put),
            Ok(None) => Sobytie::Otmeneno,
            Err(e) => {
                journal::warn("obnovlenie", &format!("скачивание обновления: {e}"));
                Sobytie::OshibkaSkachivaniya(e)
            }
        };
        let _ = tx.send(soobshchenie);
        pererisovat(&ctx);
    });
}

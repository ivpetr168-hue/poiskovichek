//! Самообновление без сети: версии, подпись, адрес установщика, разбор latest.json.

use ed25519_dalek::{Signer, SigningKey};
use poiskovichek::obnovlenie::{
    adres_ustanovshchika_dopustim, proverit_podpis, razobrat_opisanie, razobrat_versiyu,
    ubrat_ustanovshchiki, versiya_novee,
};

fn klyuch(semya: u8) -> SigningKey {
    SigningKey::from_bytes(&[semya; 32])
}

const OPISANIE: &str = r#"{"version":"0.2.0","url":"https://github.com/ivpetr168-hue/poiskovichek-obnovleniya/releases/download/v0.2.0/Poiskovichek-setup.exe","sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","size":10485760,"notes":"текст"}"#;

#[test]
fn versii_sravnivayutsya_po_chislam() {
    assert_eq!(versiya_novee("0.10.0", "0.9.0"), Ok(true));
    assert_eq!(versiya_novee("0.9.0", "0.10.0"), Ok(false));
    assert_eq!(versiya_novee("0.2.0", "0.1.0"), Ok(true));
    assert_eq!(versiya_novee("0.1.0", "0.1.0"), Ok(false));
    assert_eq!(versiya_novee("1.0", "0.9.9"), Ok(true));
    assert_eq!(versiya_novee("1.0", "1.0.0"), Ok(false));
    assert!(versiya_novee("1.x", "1.0.0").is_err());
    assert_eq!(razobrat_versiyu("0.10.0"), Some(vec![0, 10, 0]));
    assert_eq!(razobrat_versiyu(""), None);
    assert_eq!(razobrat_versiyu("1..2"), None);
    assert_eq!(razobrat_versiyu("v1.2"), None);
}

#[test]
fn podpis_verna() {
    let k = klyuch(1);
    let podpis = k.sign(OPISANIE.as_bytes());
    let otkrytyi = k.verifying_key().to_bytes();
    assert!(proverit_podpis(OPISANIE.as_bytes(), &podpis.to_bytes(), &otkrytyi).is_ok());
}

#[test]
fn isporchennyi_bait_otvergaetsya() {
    let k = klyuch(1);
    let podpis = k.sign(OPISANIE.as_bytes()).to_bytes();
    let otkrytyi = k.verifying_key().to_bytes();
    let mut dannye = OPISANIE.as_bytes().to_vec();
    dannye[10] ^= 1;
    assert!(proverit_podpis(&dannye, &podpis, &otkrytyi).is_err());
    let mut podpis_isporchena = podpis;
    podpis_isporchena[0] ^= 1;
    assert!(proverit_podpis(OPISANIE.as_bytes(), &podpis_isporchena, &otkrytyi).is_err());
}

#[test]
fn chuzhoi_klyuch_otvergaetsya() {
    let podpis = klyuch(1).sign(OPISANIE.as_bytes()).to_bytes();
    let chuzhoi = klyuch(2).verifying_key().to_bytes();
    assert!(proverit_podpis(OPISANIE.as_bytes(), &podpis, &chuzhoi).is_err());
}

#[test]
fn podpis_ne_toi_dliny_otvergaetsya() {
    let otkrytyi = klyuch(1).verifying_key().to_bytes();
    assert!(proverit_podpis(OPISANIE.as_bytes(), &[0u8; 63], &otkrytyi).is_err());
    assert!(proverit_podpis(OPISANIE.as_bytes(), &[], &otkrytyi).is_err());
}

#[test]
fn adres_ustanovshchika() {
    let svoi =
        "https://github.com/ivpetr168-hue/poiskovichek-obnovleniya/releases/download/v0.2.0/x.exe";
    assert!(adres_ustanovshchika_dopustim(svoi, false));
    assert!(adres_ustanovshchika_dopustim(svoi, true));
    // чужое хранилище с похожим началом
    assert!(!adres_ustanovshchika_dopustim(
        "https://github.com/ivpetr168-hue/poiskovichek-obnovleniya-evil/x.exe",
        false
    ));
    assert!(!adres_ustanovshchika_dopustim(
        "https://example.com/x.exe",
        false
    ));
    assert!(!adres_ustanovshchika_dopustim(
        "http://github.com/ivpetr168-hue/poiskovichek-obnovleniya/x.exe",
        false
    ));
    // локальный адрес — только при подмене адреса для проверки
    assert!(!adres_ustanovshchika_dopustim(
        "http://127.0.0.1:8000/x.exe",
        false
    ));
    assert!(adres_ustanovshchika_dopustim(
        "http://127.0.0.1:8000/x.exe",
        true
    ));
    assert!(adres_ustanovshchika_dopustim(
        "http://127.0.0.1/x.exe",
        true
    ));
    assert!(!adres_ustanovshchika_dopustim(
        "http://127.0.0.1.evil.com/x.exe",
        true
    ));
}

#[test]
fn razbor_latest_json() {
    let op = razobrat_opisanie(OPISANIE.as_bytes()).unwrap();
    assert_eq!(op.version, "0.2.0");
    assert_eq!(op.size, 10_485_760);
    assert_eq!(op.notes, "текст");
    assert!(op.url.ends_with("Poiskovichek-setup.exe"));

    // без заметок — можно
    let bez_zametok = OPISANIE.replace(r#","notes":"текст""#, "");
    assert!(razobrat_opisanie(bez_zametok.as_bytes()).is_ok());

    // неверная контрольная сумма, версия и размер, и вообще не json
    assert!(
        razobrat_opisanie(
            OPISANIE
                .replace(
                    "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "12"
                )
                .as_bytes()
        )
        .is_err()
    );
    assert!(
        razobrat_opisanie(OPISANIE.replace("0.2.0\",\"url", "abc\",\"url").as_bytes()).is_err()
    );
    assert!(razobrat_opisanie(OPISANIE.replace("10485760", "0").as_bytes()).is_err());
    assert!(razobrat_opisanie(b"{").is_err());
}

#[test]
fn vstroennyi_klyuch_razbiraetsya() {
    let mut b = [0u8; 32];
    hex::decode_to_slice(poiskovichek::obnovlenie::OTKRYTYI_KLYUCH_HEX, &mut b).unwrap();
    assert!(ed25519_dalek::VerifyingKey::from_bytes(&b).is_ok());
}

#[test]
fn ustanovshchiki_ubirayutsya_chuzhie_ostayutsya() {
    let tmp = tempfile::tempdir().unwrap();
    let papka = tmp.path().join("obnovlenie");
    std::fs::create_dir(&papka).unwrap();
    std::fs::write(papka.join("Poiskovichek-setup-0.2.0.exe"), b"a").unwrap();
    std::fs::write(papka.join("Poiskovichek-setup-0.3.0.exe"), b"b").unwrap();
    std::fs::write(papka.join("drugoy.txt"), b"c").unwrap();
    assert_eq!(ubrat_ustanovshchiki(&papka), 2);
    assert!(papka.join("drugoy.txt").exists());
    assert!(!papka.join("Poiskovichek-setup-0.2.0.exe").exists());
    assert!(!papka.join("Poiskovichek-setup-0.3.0.exe").exists());
    assert!(papka.exists());
}

#[test]
fn pustaya_papka_udalyaetsya_a_otsutstvuyushchaya_daet_nol() {
    let tmp = tempfile::tempdir().unwrap();
    let papka = tmp.path().join("obnovlenie");
    std::fs::create_dir(&papka).unwrap();
    std::fs::write(papka.join("Poiskovichek-setup-0.2.0.exe"), b"a").unwrap();
    assert_eq!(ubrat_ustanovshchiki(&papka), 1);
    assert!(!papka.exists());
    assert_eq!(ubrat_ustanovshchiki(&papka), 0);
}

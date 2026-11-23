//! Повтор записи индекса при кратковременном отказе в доступе (Windows, антивирус).

use std::io;
use std::time::Duration;

use poiskovichek::index::{PovtornyiKatalog, ZAPIS_POPYTOK, povtor_s_pauzoy};

const PAUZA: Duration = Duration::from_millis(1);

#[test]
fn tri_otkaza_potom_uspekh() {
    let mut vyzovov = 0;
    let itog = povtor_s_pauzoy(
        &mut || {
            vyzovov += 1;
            if vyzovov <= 3 {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            } else {
                Ok(vyzovov)
            }
        },
        PAUZA,
    );
    assert_eq!(itog.unwrap(), 4);
    assert_eq!(vyzovov, 4);
}

#[test]
fn postoyannyi_otkaz_daet_oshibku_posle_desyati_popytok() {
    let mut vyzovov = 0;
    let itog: io::Result<()> = povtor_s_pauzoy(
        &mut || {
            vyzovov += 1;
            Err(io::Error::from_raw_os_error(5))
        },
        PAUZA,
    );
    assert!(itog.is_err());
    assert_eq!(vyzovov, ZAPIS_POPYTOK);
    assert_eq!(vyzovov, 10);
}

#[test]
fn oshibka_32_tozhe_povtoryaetsya() {
    let mut vyzovov = 0;
    let itog = povtor_s_pauzoy(
        &mut || {
            vyzovov += 1;
            if vyzovov == 1 {
                Err(io::Error::from_raw_os_error(32))
            } else {
                Ok(())
            }
        },
        PAUZA,
    );
    assert!(itog.is_ok());
    assert_eq!(vyzovov, 2);
}

#[test]
fn drugaya_oshibka_srazu_bez_povtorov() {
    let mut vyzovov = 0;
    let itog: io::Result<()> = povtor_s_pauzoy(
        &mut || {
            vyzovov += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        },
        PAUZA,
    );
    assert_eq!(itog.unwrap_err().kind(), io::ErrorKind::NotFound);
    assert_eq!(vyzovov, 1);
}

#[test]
fn katalog_s_povtorom_sozdaet_i_otkryvaet_indeks() {
    let dir = tempfile::tempdir().unwrap();
    let mut w = poiskovichek::index::open_or_create(dir.path(), None).unwrap();
    w.add("a", "привет мир").unwrap();
    w.commit().unwrap();
    drop(w);
    assert!(PovtornyiKatalog::open(dir.path()).is_ok());
    assert!(poiskovichek::index::reader(dir.path(), None).is_ok());
}

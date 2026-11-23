use tempfile::tempdir;

use poiskovichek::single::{
    AlreadyRunning, acquire, mutex_name_for_data_dir, notify_existing, show_message_id,
};

#[test]
fn test_single_instance_second_acquire_returns_already_running() {
    let dir = tempdir().expect("tempdir");
    let data_dir = dir.path();

    // 1. Первый захват успешен
    let guard1 = acquire(data_dir).expect("Первый acquire должен пройти успешно");

    // 2. Второй захват для той же папки возвращает AlreadyRunning
    let second_res = acquire(data_dir);
    match second_res {
        Err(AlreadyRunning) => {} // Ожидаемое поведение
        Ok(_) => panic!("Второй acquire для той же папки не должен был пройти!"),
    }

    // 3. Освобождаем первый мьютекс
    drop(guard1);

    // 4. После освобождения acquire снова успешен
    let guard3 = acquire(data_dir).expect("После drop первый мьютекс должен быть доступен");
    drop(guard3);
}

#[test]
fn test_different_data_folders_have_different_mutexes() {
    let dir1 = tempdir().expect("tempdir1");
    let dir2 = tempdir().expect("tempdir2");

    let name1 = mutex_name_for_data_dir(dir1.path());
    let name2 = mutex_name_for_data_dir(dir2.path());
    assert_ne!(
        name1, name2,
        "Имена мьютексов для разных папок должны различаться"
    );

    let guard1 = acquire(dir1.path()).expect("acquire 1");
    let guard2 = acquire(dir2.path()).expect("acquire 2");

    drop(guard1);
    drop(guard2);
}

#[test]
fn test_show_message_id_and_notify_existing() {
    let msg_id = show_message_id();
    assert!(
        msg_id > 0xC000,
        "RegisterWindowMessageW возвращает ID в диапазоне 0xC000-0xFFFF: {msg_id}"
    );

    // notify_existing отправляет сообщение с HWND_BROADCAST
    let ok = notify_existing();
    assert!(ok, "notify_existing должен успешно отправить сообщение");
}

use std::path::Path;
use windows::Win32::System::Registry::HKEY_CURRENT_USER;

use poiskovichek::autostart::{
    AUTOSTART_VALUE_NAME, delete_key, get_autostart_command_custom, is_autostart_enabled_custom,
    set_autostart_custom,
};

const TEST_SUB_KEY: &str = r"Software\ПоисКовичеК-тест\Run";
const TEST_PARENT_KEY: &str = r"Software\ПоисКовичеК-тест";

#[test]
fn test_autostart_toggle_in_test_registry_key() {
    let fake_exe = Path::new(r"C:\Program Files\Poiskovichek\Poiskovichek.exe");

    // Гарантируем чистоту до начала теста
    let _ = delete_key(HKEY_CURRENT_USER, TEST_SUB_KEY);
    let _ = delete_key(HKEY_CURRENT_USER, TEST_PARENT_KEY);

    // 1. Изначально автозапуск выключен
    let enabled_before =
        is_autostart_enabled_custom(HKEY_CURRENT_USER, TEST_SUB_KEY, AUTOSTART_VALUE_NAME).unwrap();
    assert!(
        !enabled_before,
        "Изначально автозапуск должен быть выключен"
    );

    // 2. Включаем автозапуск
    set_autostart_custom(
        HKEY_CURRENT_USER,
        TEST_SUB_KEY,
        AUTOSTART_VALUE_NAME,
        true,
        fake_exe,
    )
    .expect("Включение автозапуска");

    // Проверяем: значение есть
    let enabled_after =
        is_autostart_enabled_custom(HKEY_CURRENT_USER, TEST_SUB_KEY, AUTOSTART_VALUE_NAME).unwrap();
    assert!(enabled_after, "Автозапуск должен быть включен");

    // Проверяем: строка содержит путь и ключ --background
    let cmd = get_autostart_command_custom(HKEY_CURRENT_USER, TEST_SUB_KEY, AUTOSTART_VALUE_NAME)
        .unwrap()
        .expect("Команда автозапуска должна присутствовать");

    assert!(
        cmd.contains(r#""C:\Program Files\Poiskovichek\Poiskovichek.exe" --background"#),
        "Команда должна быть в кавычках с аргументом --background: {cmd}"
    );

    // 3. Выключаем автозапуск
    set_autostart_custom(
        HKEY_CURRENT_USER,
        TEST_SUB_KEY,
        AUTOSTART_VALUE_NAME,
        false,
        fake_exe,
    )
    .expect("Выключение автозапуска");

    // Проверяем: значения нет
    let enabled_off =
        is_autostart_enabled_custom(HKEY_CURRENT_USER, TEST_SUB_KEY, AUTOSTART_VALUE_NAME).unwrap();
    assert!(!enabled_off, "Автозапуск должен быть выключен");

    let cmd_after =
        get_autostart_command_custom(HKEY_CURRENT_USER, TEST_SUB_KEY, AUTOSTART_VALUE_NAME)
            .unwrap();
    assert!(cmd_after.is_none(), "Значение должно быть удалено");

    // 4. Очистка тестового раздела реестра
    let _ = delete_key(HKEY_CURRENT_USER, TEST_SUB_KEY);
    let _ = delete_key(HKEY_CURRENT_USER, TEST_PARENT_KEY);
}

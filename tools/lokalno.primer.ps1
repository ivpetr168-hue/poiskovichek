# Локальные пути набора инструментов Rust для конкретной машины (вне системы контроля версий)
# Скопируйте этот файл в tools\lokalno.ps1 и укажите пути к rustup, cargo и исполняемому файлу cargo.exe.
$env:RUSTUP_HOME = ""
$env:CARGO_HOME = ""
$env:CARGO = ""
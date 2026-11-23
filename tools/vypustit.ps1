# Выпуск новой версии ПоисКовичеКа:
# - Обычная локальная сборка (без подписи и выкладки; сборка может требовать зависимости):
#     .\tools\vypustit.ps1 -Versiya 0.2.1
# - Сборка с подписанием манифеста обновления Ed25519 (требует закрытый ключ):
#     .\tools\vypustit.ps1 -Versiya 0.2.1 -KlyuchPem "путь\к\private_key.pem"
# - Полный выпуск с публикацией GitHub Release (требует ключ и явный switch -Opublikovat):
#     .\tools\vypustit.ps1 -Versiya 0.2.1 -KlyuchPem "путь\к\private_key.pem" -Opublikovat -Zametki "что нового"
# - Совместимый флаг -BezVykladki гарантирует отсутствие публикации:
#     .\tools\vypustit.ps1 -Versiya 0.2.1 -KlyuchPem "путь\к\private_key.pem" -BezVykladki
param(
    [Parameter(Mandatory = $true)]
    [string]$Versiya,
    [string]$Zametki = "",
    [string]$KlyuchPem = "",
    [string]$Hranilishche = "ivpetr168-hue/poiskovichek-obnovleniya",
    [switch]$Opublikovat,
    [switch]$BezVykladki
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$invokingDirectory = (Get-Location).Path
$publikovatFakticheski = $Opublikovat -and (-not $BezVykladki)
$podpisProverena = $false
if ($Hranilishche -notmatch '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' -or $Hranilishche -match '(^|/)(\.|\.\.)(/|$)') {
    throw "Хранилище должно иметь вид владелец/репозиторий"
}
# Для другого репозитория адрес обновлений в программе также нужно согласовать.
if ($publikovatFakticheski -and -not $KlyuchPem) {
    throw "Для публикации явно укажите файл закрытого ключа (-KlyuchPem)"
}
if ($KlyuchPem) {
    if (-not [IO.Path]::IsPathRooted($KlyuchPem)) {
        $KlyuchPem = Join-Path $invokingDirectory $KlyuchPem
    }
    if (-not (Test-Path -LiteralPath $KlyuchPem -PathType Leaf)) {
        throw "Указанный файл закрытого ключа не найден"
    }
    $KlyuchPem = (Resolve-Path -LiteralPath $KlyuchPem).ProviderPath
}
$projectRoot = (Resolve-Path "$PSScriptRoot\..").Path
Set-Location $projectRoot

# Открытый ключ, зашитый в программу (src\obnovlenie.rs)
$otkrytyiKlyuchHex = "ffb9b47fbbb1d582b2a24f2e576ddde2af9aa260b3e126ccf430c5a49f42e934"

if ($Versiya -notmatch '^\d+\.\d+\.\d+$') {
    Write-Host "ОШИБКА: версия должна быть вида 0.2.1, получено: $Versiya"
    exit 1
}

$utf8NoBom = New-Object System.Text.UTF8Encoding($false)

# 1. Версия в Cargo.toml ([package]); Cargo.lock обновится сборкой
Write-Host "=== Версия $Versiya в Cargo.toml ==="
$cargoToml = Join-Path $projectRoot "Cargo.toml"
$tekst = [IO.File]::ReadAllText($cargoToml)
$regex = New-Object System.Text.RegularExpressions.Regex('(?m)^version\s*=\s*"[^"]*"')
if (-not $regex.IsMatch($tekst)) {
    Write-Host "ОШИБКА: в Cargo.toml не найдена строка version"
    exit 1
}
$tekst = $regex.Replace($tekst, "version = `"$Versiya`"", 1)
[IO.File]::WriteAllText($cargoToml, $tekst, $utf8NoBom)

# 2. Установщик с этой версией
Write-Host "=== Сборка установщика ==="
& "$PSScriptRoot\sobrat-ustanovshik.ps1" -Versiya $Versiya
if ($LASTEXITCODE -ne 0) {
    Write-Host "ОШИБКА: сборка установщика завершилась с кодом $LASTEXITCODE"
    exit 1
}

# 3. Копия в dist\vypusk\<версия>
$papkaVypuska = Join-Path $projectRoot "dist\vypusk\$Versiya"
New-Item -ItemType Directory -Path $papkaVypuska -Force | Out-Null
$ustanovshchik = Join-Path $papkaVypuska "Poiskovichek-setup.exe"
$sobrannyiUstanovshchik = Join-Path $projectRoot "dist\ПоисКовичеК-установка.exe"
if (-not $KlyuchPem) {
    if (-not (Test-Path -LiteralPath (Join-Path $papkaVypuska "latest.json")) -and
        -not (Test-Path -LiteralPath (Join-Path $papkaVypuska "latest.json.sig"))) {
        Copy-Item -LiteralPath $sobrannyiUstanovshchik -Destination $ustanovshchik -Force
    }
    Write-Host "Ключ не указан: подготовлен только установщик; существующие манифесты не изменены."
    if ($BezVykladki) { Write-Host "Флаг -BezVykladki активен: публикация пропущена." }
    exit 0
}

# 4. latest.json (UTF-8 без BOM, без перевода строки в конце)
Write-Host "=== Формирование latest.json ==="
$fayl = Get-Item -LiteralPath $sobrannyiUstanovshchik
$sha = (Get-FileHash -LiteralPath $sobrannyiUstanovshchik -Algorithm SHA256).Hash.ToLowerInvariant()
$opisanie = [ordered]@{
    version = $Versiya
    url     = "https://github.com/$Hranilishche/releases/download/v$Versiya/Poiskovichek-setup.exe"
    sha256  = $sha
    size    = [int64]$fayl.Length
    notes   = $Zametki
}
$vremennayaPapka = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $vremennayaPapka | Out-Null
$latest = Join-Path $vremennayaPapka "latest.json"
$podpisFayl = Join-Path $vremennayaPapka "latest.json.sig"
[IO.File]::WriteAllText($latest, ($opisanie | ConvertTo-Json -Compress), $utf8NoBom)

# 5. Подписание (если передан путь к ключу $KlyuchPem)
if ($KlyuchPem) {
    Write-Host "=== Подписание latest.json закрытым ключом ==="
    # Поиск openssl: из PATH или Git
    $openssl = $null
    $cmd = Get-Command "openssl.exe" -ErrorAction SilentlyContinue
    if ($cmd) {
        $openssl = $cmd.Source
    } elseif (Test-Path "C:\Program Files\Git\usr\bin\openssl.exe") {
        $openssl = "C:\Program Files\Git\usr\bin\openssl.exe"
    } else {
        Write-Host "ОШИБКА: openssl не найден для подписания манифеста"
        exit 1
    }

    if (Test-Path $podpisFayl) { Remove-Item -LiteralPath $podpisFayl -Force }

    Push-Location $vremennayaPapka
    try {
        & $openssl pkeyutl -sign -inkey $KlyuchPem -rawin -in latest.json -out latest.json.sig
        if ($LASTEXITCODE -ne 0) { throw "openssl: не удалось подписать latest.json" }

        # Открытый ключ из закрытого: PEM для проверки и сырые 32 байта для сверки с зашитым в программу
        $otkrytyiPem = Join-Path $vremennayaPapka "otkrytyi.pem"
        $otkrytyiDer = Join-Path $vremennayaPapka "otkrytyi.der"
        & $openssl pkey -in $KlyuchPem -pubout -out $otkrytyiPem
        if ($LASTEXITCODE -ne 0) { throw "openssl: не удалось получить открытый ключ" }
        & $openssl pkey -in $KlyuchPem -pubout -outform DER -out $otkrytyiDer
        if ($LASTEXITCODE -ne 0) { throw "openssl: не удалось получить открытый ключ (DER)" }

        & $openssl pkeyutl -verify -pubin -inkey $otkrytyiPem -rawin -in latest.json -sigfile latest.json.sig
        if ($LASTEXITCODE -ne 0) { throw "подпись latest.json не прошла проверку" }
    } finally {
        Pop-Location
    }

    $razmerPodpisi = (Get-Item -LiteralPath $podpisFayl).Length
    if ($razmerPodpisi -ne 64) {
        Write-Host "ОШИБКА: подпись должна быть 64 байта, получено $razmerPodpisi"
        exit 1
    }
    $der = [IO.File]::ReadAllBytes((Join-Path $vremennayaPapka "otkrytyi.der"))
    $klyuchHex = ([BitConverter]::ToString($der[($der.Length - 32)..($der.Length - 1)]) -replace '-', '').ToLowerInvariant()
    if ($klyuchHex -ne $otkrytyiKlyuchHex) {
        Write-Host "ОШИБКА: открытый ключ из указанного файла ($klyuchHex) не совпадает с зашитым в программу ($otkrytyiKlyuchHex)"
        exit 1
    }
    Copy-Item -LiteralPath $sobrannyiUstanovshchik -Destination $ustanovshchik -Force
    Copy-Item -LiteralPath $latest -Destination (Join-Path $papkaVypuska "latest.json") -Force
    Copy-Item -LiteralPath $podpisFayl -Destination (Join-Path $papkaVypuska "latest.json.sig") -Force
    $podpisProverena = $true
    Remove-Item -LiteralPath $vremennayaPapka -Recurse -Force
    Write-Host "Подпись проверена. Файлы выпуска: $papkaVypuska"
} else {
    Write-Host "Закрытый ключ не указан (-KlyuchPem). Манифест latest.json сформирован без подписи (офлайн-режим)."
}

# 6. Выкладка в хранилище GitHub Release
# Выполняется только при явном указании switch-параметра -Opublikovat и отсутствии флага -BezVykladki
if ($publikovatFakticheski) {
    if (-not $podpisProverena) {
        Write-Host "ОШИБКА: Невозможно опубликовать релиз: отсутствует файл подписи latest.json.sig (укажите -KlyuchPem для подписания)"
        exit 1
    }

    $ghCmd = Get-Command "gh.exe" -ErrorAction SilentlyContinue
    if (-not $ghCmd) {
        Write-Host "ОШИБКА: Утилита GitHub CLI (gh) не найдена для публикации"
        exit 1
    }

    Write-Host "=== Публикация релиза v$Versiya в $Hranilishche ==="
    Push-Location $papkaVypuska
    try {
        & gh release create "v$Versiya" --repo $Hranilishche --title $Versiya --notes $Zametki "Poiskovichek-setup.exe" "latest.json" "latest.json.sig"
        if ($LASTEXITCODE -ne 0) { throw "gh release create завершился с кодом $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
    Write-Host "=== Релиз успешно опубликован ==="
} else {
    if ($BezVykladki) {
        Write-Host "Флаг -BezVykladki активен: публикация пропущена."
    } else {
        Write-Host "Публикация не запрашивалась (требуется явный switch -Opublikovat)."
    }
    Write-Host "=== Локальный выпуск готов в $papkaVypuska ==="
}
exit 0

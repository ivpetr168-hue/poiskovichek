param(
    # Версия для установщика (/DMyAppVersion); без неё берётся версия из сценария .iss
    [string]$Versiya = ""
)

[Console]::OutputEncoding = [Text.Encoding]::UTF8

if (Test-Path "$PSScriptRoot\lokalno.ps1") {
    . "$PSScriptRoot\lokalno.ps1"
}

$projectRoot = (Resolve-Path "$PSScriptRoot\..").Path
Set-Location $projectRoot

if (-not $env:RUSTUP_TOOLCHAIN) {
    $env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-msvc'
}

$cargo = if ($env:CARGO) {
    $env:CARGO
} elseif ($env:CARGO_HOME -and (Test-Path "$env:CARGO_HOME\bin\cargo.exe")) {
    "$env:CARGO_HOME\bin\cargo.exe"
} elseif (Get-Command "cargo" -ErrorAction SilentlyContinue) {
    "cargo"
} else {
    "cargo"
}

$iscc = if (Test-Path "C:\tools\Inno Setup 7\ISCC.exe") {
    "C:\tools\Inno Setup 7\ISCC.exe"
} elseif (Get-Command "ISCC.exe" -ErrorAction SilentlyContinue) {
    "ISCC.exe"
} else {
    Write-Host "ОШИБКА: Inno Setup компилятор (ISCC.exe) не найден"
    exit 1
}

# 1. Сборка THIRD_PARTY.md (если скрипт лицензий есть)
$licScript = Join-Path $PSScriptRoot "licenzii.ps1"
if (Test-Path $licScript) {
    Write-Host "=== Генерация актуального списка лицензий THIRD_PARTY.md ==="
    & $licScript
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ВНИМАНИЕ: Скрипт лицензий завершился с кодом $LASTEXITCODE"
    }
}

# 2. Сборка Poiskovichek в режиме release
Write-Host "=== Сборка Poiskovichek в режиме release ==="
$targetDir = Join-Path $projectRoot "target"
if (-not (Test-Path $targetDir)) {
    New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
}
$buildLog = Join-Path $targetDir "build-release.log"
$buildErr = Join-Path $targetDir "build-release.err.log"
Set-Content -Path $buildLog -Value ""
Set-Content -Path $buildErr -Value ""

$buildProc = Start-Process -FilePath $cargo -ArgumentList @("build", "--release") -WorkingDirectory $projectRoot -PassThru -NoNewWindow -RedirectStandardOutput $buildLog -RedirectStandardError $buildErr
$null = $buildProc.Handle
# Предел 40 минут = 2400000 мс
if (-not $buildProc.WaitForExit(2400000)) {
    Write-Host "Превышен предел времени сборки release (40 минут)"
    taskkill.exe /PID $buildProc.Id /T /F | Out-Null
    exit 1
}
$buildProc.WaitForExit()

if (Test-Path $buildErr) {
    $errContent = Get-Content $buildErr
    if ($errContent) {
        Add-Content -Path $buildLog -Value $errContent
    }
}

if ($buildProc.ExitCode -ne 0) {
    Write-Host "СБОРКА RELEASE: ОШИБКА"
    if (Test-Path $buildLog) {
        Get-Content $buildLog -Tail 40 | ForEach-Object { Write-Host $_ }
    }
    exit 1
}
Write-Host "=== Сборка release успешно завершена ==="

# Проверка наличия собранного исполняемого файла
$releaseExe = Join-Path $projectRoot "target\release\Poiskovichek.exe"
if (-not (Test-Path $releaseExe)) {
    Write-Host "ОШИБКА: Исполняемый файл не найден: $releaseExe"
    exit 1
}

# 3. Сборка инсталлятора через ISCC
Write-Host "=== Сборка инсталлятора Inno Setup ==="
$distDir = Join-Path $projectRoot "dist"
if (-not (Test-Path $distDir)) {
    New-Item -ItemType Directory -Path $distDir -Force | Out-Null
}

$issPath = Join-Path $projectRoot "installer\poiskovichek.iss"
if (-not (Test-Path $issPath)) {
    Write-Host "ОШИБКА: Файл сценария не найден: $issPath"
    exit 1
}

$isccLog = Join-Path $targetDir "iscc-build.log"
$isccErr = Join-Path $targetDir "iscc-build.err.log"
Set-Content -Path $isccLog -Value ""
Set-Content -Path $isccErr -Value ""

$isccArgs = @($issPath)
if ($Versiya) {
    $isccArgs = @("/DMyAppVersion=$Versiya", $issPath)
}
$isccProc = Start-Process -FilePath $iscc -ArgumentList $isccArgs -WorkingDirectory (Join-Path $projectRoot "installer") -PassThru -NoNewWindow -RedirectStandardOutput $isccLog -RedirectStandardError $isccErr
$null = $isccProc.Handle
# Предел 5 минут = 300000 мс
if (-not $isccProc.WaitForExit(300000)) {
    Write-Host "Превышен предел времени компиляции Inno Setup (5 минут)"
    taskkill.exe /PID $isccProc.Id /T /F | Out-Null
    exit 1
}
$isccProc.WaitForExit()

if (Test-Path $isccErr) {
    $errContent = Get-Content $isccErr
    if ($errContent) {
        Add-Content -Path $isccLog -Value $errContent
    }
}

if ($isccProc.ExitCode -ne 0) {
    Write-Host "СБОРКА УСТАНОВЩИКА: ОШИБКА (код $($isccProc.ExitCode))"
    if (Test-Path $isccLog) {
        Get-Content $isccLog -Tail 40 | ForEach-Object { Write-Host $_ }
    }
    exit 1
}

$installerExe = Join-Path $distDir "ПоисКовичеК-установка.exe"
if (-not (Test-Path $installerExe)) {
    Write-Host "ОШИБКА: Файл установщика не найден по пути: $installerExe"
    exit 1
}

$installerItem = Get-Item $installerExe
if ($installerItem.Length -le 0) {
    Write-Host "ОШИБКА: Файл установщика пустой: $installerExe"
    exit 1
}

$sizeMb = [Math]::Round($installerItem.Length / 1MB, 2)
Write-Host "=== УСТАНОВЩИК УСПЕШНО СОБРАН ==="
Write-Host "Путь: $installerExe"
Write-Host "Размер: $($installerItem.Length) байт ($sizeMb МБ)"

exit 0


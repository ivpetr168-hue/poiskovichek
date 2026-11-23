[CmdletBinding()]
param (
    [switch]$DlyaReadme
)

[Console]::OutputEncoding = [Text.Encoding]::UTF8

if (Test-Path "$PSScriptRoot\lokalno.ps1") {
    . "$PSScriptRoot\lokalno.ps1"
}

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

$projectRoot = (Resolve-Path "$PSScriptRoot\..").Path
Set-Location $projectRoot

Write-Host "=== 1. Сборка Poiskovichek в режиме release ==="
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

if ($DlyaReadme) {
    $tempDir = "C:\ПоисКовичеК-проба"
    $probaTarget = Join-Path $tempDir "документы"
} else {
    $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) "poiskovichek-test-$([System.Guid]::NewGuid().ToString('N'))"
    $probaTarget = Join-Path $tempDir "proba"
}

if (Test-Path $tempDir) {
    [System.IO.Directory]::Delete($tempDir, $true)
}
New-Item -ItemType Directory -Path $tempDir -Force | Out-Null
New-Item -ItemType Directory -Path $probaTarget -Force | Out-Null

try {
    $releaseExe = Join-Path $projectRoot "target\release\Poiskovichek.exe"
    if (-not (Test-Path $releaseExe)) {
        Write-Host "Исполняемый файл не найден: $releaseExe"
        exit 1
    }
    $tempExe = Join-Path $tempDir "Poiskovichek.exe"
    Copy-Item -Path $releaseExe -Destination $tempExe -Force

    $morphFile = Join-Path $projectRoot "assets\morph.fst"
    if (Test-Path $morphFile) {
        Copy-Item -Path $morphFile -Destination (Join-Path $tempDir "morph.fst") -Force
    }

    Set-Content -Path (Join-Path $tempDir "settings.toml") -Value ""

    $probaSource = Join-Path $projectRoot "tests\proba"
    Copy-Item -Path "$probaSource\*" -Destination $probaTarget -Recurse -Force

    if ($DlyaReadme) {
        # Добавляем второй документ с фразой в другой форме для снимка poisk-fraza.png (2+ карточки с подсветкой)
        $doc2Content = "Исследовательское судно наблюдало за арктическими льдами во время многодневного дрейфа."
        $doc2Path = Join-Path $probaTarget "zametka-polyarnaya.txt"
        [System.IO.File]::WriteAllText($doc2Path, $doc2Content, [System.Text.Encoding]::UTF8)

        # Обновляем self-test.txt, чтобы учесть 2 найденных документа по фразе "арктический лед"
        $selfTestFile = Join-Path $probaTarget "self-test.txt"
        if (Test-Path $selfTestFile) {
            $stText = [System.IO.File]::ReadAllText($selfTestFile, [System.Text.Encoding]::UTF8)
            $stText = $stText.Replace("арктический лед`t1`tdokument.txt", "арктический лед`t2`tdokument.txt")
            [System.IO.File]::WriteAllText($selfTestFile, $stText, [System.Text.Encoding]::UTF8)
        }
    }

    Write-Host "=== 2. Запуск самопроверки (--self-test) ==="
    $selfTestLog = Join-Path $probaTarget "self-test.log"
    $selfTestProc = Start-Process -FilePath $tempExe -ArgumentList @("--self-test", $probaTarget) -WorkingDirectory $tempDir -PassThru -NoNewWindow
    $null = $selfTestProc.Handle
    if (-not $selfTestProc.WaitForExit(600000)) {
        Write-Host "Превышен предел времени самопроверки (10 минут)"
        taskkill.exe /PID $selfTestProc.Id /T /F | Out-Null
        exit 1
    }
    $selfTestProc.WaitForExit()
    $selfTestExitCode = $selfTestProc.ExitCode

    Write-Host "--- Журнал самопроверки ---"
    if (Test-Path $selfTestLog) {
        Get-Content $selfTestLog -Encoding UTF8 | ForEach-Object { Write-Host $_ }
    } elseif (Test-Path (Join-Path $tempDir "self-test.log")) {
        Get-Content (Join-Path $tempDir "self-test.log") -Encoding UTF8 | ForEach-Object { Write-Host $_ }
    } else {
        Write-Host "Файл self-test.log не найден."
    }
    Write-Host "Код возврата самопроверки: $selfTestExitCode"

    if ($selfTestExitCode -ne 0) {
        Write-Host "САМОПРОВЕРКА: ПРОВАЛ"
        exit 1
    }
    Write-Host "САМОПРОВЕРКА: УСПЕХ"

    Write-Host "=== 3. Запуск создания снимков для проверки (--snimok) ==="
    $snimkiDir = Join-Path $targetDir "snimki"
    if (Test-Path $snimkiDir) {
        [System.IO.Directory]::Delete($snimkiDir, $true)
    }
    New-Item -ItemType Directory -Path $snimkiDir -Force | Out-Null

    $docsSnimkiDir = Join-Path $projectRoot "docs\snimki"
    if ($DlyaReadme -and -not (Test-Path $docsSnimkiDir)) {
        New-Item -ItemType Directory -Path $docsSnimkiDir -Force | Out-Null
    }

    function Take-Snimok {
        param (
            [string]$FileName,
            [string[]]$ExtraArgs
        )
        $localPath = Join-Path $tempDir $FileName
        $destPath = Join-Path $snimkiDir $FileName
        $argsList = @("--snimok", $localPath) + $ExtraArgs

        Write-Host "--- Создание снимка: $FileName (аргументы: $argsList) ---"
        $proc = Start-Process -FilePath $tempExe -ArgumentList $argsList -WorkingDirectory $tempDir -PassThru -NoNewWindow
        $null = $proc.Handle
        if (-not $proc.WaitForExit(30000)) {
            Write-Host "Превышен предел времени создания снимка $FileName (30 секунд)"
            taskkill.exe /PID $proc.Id /T /F | Out-Null
            exit 1
        }
        $proc.WaitForExit()
        $code = $proc.ExitCode
        if ($code -ne 0) {
            Write-Host "СНИМОК: Создание $FileName завершилось с кодом ошибки: $code"
            exit 1
        }

        if (-not (Test-Path $localPath)) {
            Write-Host "СНИМОК: Файл не найден по пути $localPath"
            exit 1
        }

        $item = Get-Item $localPath
        if ($item.Length -le 0) {
            Write-Host "СНИМОК: Файл пустой (0 байт): $localPath"
            exit 1
        }

        Copy-Item -Path $localPath -Destination $destPath -Force
        if ($DlyaReadme -and $FileName -ne "master.png") {
            Copy-Item -Path $localPath -Destination (Join-Path $docsSnimkiDir $FileName) -Force
        }
        Write-Host "Файл снимка успешно создан ($($item.Length) байт) и сохранён: $destPath"
    }

    # 1. Фраза из пробных документов
    Take-Snimok "poisk-fraza.png" @("--zapros", "арктические льды")

    # 2. Словоформа в творительном падеже
    Take-Snimok "poisk-slovoforma.png" @("--zapros", "арктическими льдами")

    # 3. Открытое меню «Действия»
    Take-Snimok "menu-deystviya.png" @("--okno", "menu")

    # 4. Окно «О программе»
    Take-Snimok "o-programme.png" @("--okno", "o-programme")

    # 5. Окно настроек
    Take-Snimok "nastroyki.png" @("--okno", "nastroyki")

    # 6. Мастер первого запуска
    Take-Snimok "master.png" @("--okno", "master")

    # Для обратной совместимости
    $legacySnimok = Join-Path $targetDir "snimok-poisk.png"
    Copy-Item -Path (Join-Path $snimkiDir "poisk-fraza.png") -Destination $legacySnimok -Force

    $logFiles = Get-ChildItem -Path (Join-Path $tempDir "data\logs") -Filter "*.log" -ErrorAction SilentlyContinue
    if ($logFiles) {
        $logContent = Get-Content $logFiles[0].FullName -Encoding UTF8
        $rendererLines = $logContent | Where-Object { $_ -match "рендерер:" }
        if ($rendererLines) {
            Write-Host "--- Информация о рендерере из журнала ---"
            $rendererLines | ForEach-Object { Write-Host $_ }
        }
    }

    Write-Host "САМОПРОВЕРКА И СОЗДАНИЕ ВСЕХ СНИМКОВ УСПЕШНО ЗАВЕРШЕНЫ"
}
finally {
    Write-Host "=== 4. Очистка временной папки ==="
    if (Test-Path $tempDir) {
        [System.IO.Directory]::Delete($tempDir, $true)
    }
    Write-Host "Временная папка очищена."
}

exit 0
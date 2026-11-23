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

function Run-Stage {
    param (
        [string]$StageName,
        [string[]]$Arguments
    )
    Write-Host "=== Запуск этапа: cargo $StageName ==="
    $targetDir = Join-Path $projectRoot "target"
    if (-not (Test-Path $targetDir)) {
        New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
    }
    $logFile = Join-Path $targetDir "proverka-$StageName.log"
    $errFile = Join-Path $targetDir "proverka-$StageName.err.log"
    Set-Content -Path $logFile -Value ""
    Set-Content -Path $errFile -Value ""

    # Без -Wait: в PS 5.1 он ждёт завершения фоновых процессов MSVC (vctip.exe, mspdbsrv.exe)
    $proc = Start-Process -FilePath $cargo -ArgumentList $Arguments -WorkingDirectory $projectRoot -PassThru -NoNewWindow -RedirectStandardOutput $logFile -RedirectStandardError $errFile
    $null = $proc.Handle
    if (-not $proc.WaitForExit(900000)) { Write-Host "Превышен предел времени 15 минут на этапе: $StageName"; taskkill.exe /PID $proc.Id /T /F | Out-Null; Write-Host "ПРОВЕРКА: ПРОВАЛ $StageName"; exit 1 }
    $proc.WaitForExit()

    if (Test-Path $errFile) {
        $errContent = Get-Content $errFile
        if ($errContent) {
            Add-Content -Path $logFile -Value $errContent
        }
    }

    if ($proc.ExitCode -ne 0) {
        Write-Host "ПРОВЕРКА: ПРОВАЛ $StageName"
        if (Test-Path $logFile) {
            Write-Host "--- Последние 40 строк журнала $logFile ---"
            Get-Content $logFile -Tail 40 | ForEach-Object { Write-Host $_ }
        }
        exit 1
    }
    Write-Host "=== Этап cargo $StageName успешно пройден ==="
}

function Get-LiveUnits {
    # Спрашивает у cargo, какие единицы сборки живы для check, clippy и test: cargo сам перечисляет
    # свои файлы (в том числе не пересобираемые заново). Возвращает $null, если узнать не удалось.
    $targetDir = Join-Path $projectRoot "target"
    if (-not (Test-Path $targetDir)) {
        New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
    }
    $commands = @(
        @("check", "--all-targets", "--message-format=json"),
        @("clippy", "--all-targets", "--message-format=json", "--", "-D", "warnings"),
        @("test", "--no-run", "--message-format=json")
    )
    $hashes = New-Object 'System.Collections.Generic.HashSet[string]'
    $units = @{}
    $n = 0
    foreach ($cmd in $commands) {
        $n++
        $jsonFile = Join-Path $targetDir "uborka-$n.json"
        $errFile = Join-Path $targetDir "uborka-$n.err.log"
        Set-Content -Path $jsonFile -Value ""
        Set-Content -Path $errFile -Value ""
        Write-Host "=== Уборка: cargo $($cmd[0]) (список живых файлов) ==="
        $proc = Start-Process -FilePath $cargo -ArgumentList $cmd -WorkingDirectory $projectRoot -PassThru -NoNewWindow -RedirectStandardOutput $jsonFile -RedirectStandardError $errFile
        $null = $proc.Handle
        if (-not $proc.WaitForExit(1800000)) {
            taskkill.exe /PID $proc.Id /T /F | Out-Null
            Write-Host "Уборка пропущена: превышен предел времени на cargo $($cmd[0])"
            return $null
        }
        $proc.WaitForExit()
        if ($proc.ExitCode -ne 0) {
            Write-Host "Уборка пропущена: cargo $($cmd[0]) завершился с ошибкой (подробности покажет этап проверки)"
            return $null
        }
        foreach ($line in [IO.File]::ReadLines($jsonFile)) {
            if (-not $line.Contains('"reason":"compiler-artifact"')) { continue }
            foreach ($m in [regex]::Matches($line, '-([0-9a-f]{16})\.[A-Za-z0-9.]+"')) {
                [void]$hashes.Add($m.Groups[1].Value)
            }
            if ($line.Contains('"package_id":"path+file')) {
                $a = $line | ConvertFrom-Json
                $crate = $a.target.name -replace '-', '_'
                if (-not $units.ContainsKey($crate)) {
                    $units[$crate] = New-Object 'System.Collections.Generic.HashSet[string]'
                }
                [void]$units[$crate].Add((($a.filenames -join ';') + '|' + $a.executable))
            }
        }
        Remove-Item -LiteralPath $jsonFile, $errFile -Force -ErrorAction SilentlyContinue
    }
    if ($hashes.Count -lt 10) {
        Write-Host "Уборка пропущена: cargo назвал слишком мало файлов ($($hashes.Count))"
        return $null
    }
    return [pscustomobject]@{ Hashes = $hashes; Units = $units }
}

function Clean-Stale {
    # Уборка устаревших копий в target\debug: в deps удаляются файлы с хэшем, которого cargo больше не использует,
    # в incremental по каждому крейту остаётся столько самых свежих папок, сколько у него живых единиц сборки.
    $debugDir = Join-Path $projectRoot "target\debug"
    $depsDir = Join-Path $debugDir "deps"
    $incDir = Join-Path $debugDir "incremental"
    if (-not (Test-Path -LiteralPath $debugDir)) { return }
    $live = Get-LiveUnits
    if (-not $live) { return }
    $removedFiles = 0
    $removedDirs = 0
    $removedBytes = 0L

    if (Test-Path -LiteralPath $depsDir) {
        foreach ($f in Get-ChildItem -LiteralPath $depsDir -File) {
            $m = [regex]::Match($f.Name, '^.+-(?<hash>[0-9a-f]{16})\..+$')
            if (-not $m.Success) { continue }
            if ($live.Hashes.Contains($m.Groups['hash'].Value)) { continue }
            if (-not $f.FullName.StartsWith($debugDir + "\", [StringComparison]::OrdinalIgnoreCase)) { continue }
            try {
                $size = $f.Length
                Remove-Item -LiteralPath $f.FullName -Force -ErrorAction Stop
                $removedFiles++
                $removedBytes += $size
            } catch {
                Write-Host "Не удалось удалить $($f.Name): $($_.Exception.Message)"
            }
        }
    }

    if (Test-Path -LiteralPath $incDir) {
        $dirs = @()
        foreach ($d in Get-ChildItem -LiteralPath $incDir -Directory) {
            $m = [regex]::Match($d.Name, '^(?<name>.+)-(?<hash>[0-9a-z]{6,})$')
            if ($m.Success) {
                $dirs += [pscustomobject]@{ Dir = $d; Crate = $m.Groups['name'].Value }
            }
        }
        foreach ($g in ($dirs | Group-Object Crate -CaseSensitive)) {
            $keep = 0
            if ($live.Units.ContainsKey($g.Name)) { $keep = $live.Units[$g.Name].Count }
            $stale = @($g.Group | Sort-Object { $_.Dir.LastWriteTimeUtc } -Descending | Select-Object -Skip $keep)
            foreach ($it in $stale) {
                if (-not $it.Dir.FullName.StartsWith($debugDir + "\", [StringComparison]::OrdinalIgnoreCase)) { continue }
                try {
                    $size = (Get-ChildItem -LiteralPath $it.Dir.FullName -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum
                    Remove-Item -LiteralPath $it.Dir.FullName -Recurse -Force -ErrorAction Stop
                    $removedDirs++
                    $removedBytes += [int64]$size
                } catch {
                    Write-Host "Не удалось удалить $($it.Dir.Name): $($_.Exception.Message)"
                }
            }
        }
    }

    $gb = [math]::Round($removedBytes / 1GB, 2)
    Write-Host "=== Уборка target\debug: удалено файлов в deps: $removedFiles, папок в incremental: $removedDirs, освобождено: $gb ГБ ==="
}

Clean-Stale

Run-Stage "fmt" @("fmt", "--check")
Run-Stage "clippy" @("clippy", "--all-targets", "--", "-D", "warnings")
Run-Stage "test" @("test")

$pyExe = $null
$pyPrefix = @()
if ($env:POISK_PYTHON -and (Test-Path $env:POISK_PYTHON)) {
    $pyExe = $env:POISK_PYTHON
} elseif (Get-Command python -ErrorAction SilentlyContinue) {
    $pyExe = "python"
} elseif (Get-Command py -ErrorAction SilentlyContinue) {
    $pyExe = "py"
    $pyPrefix = @("-3")
} elseif (Get-Command python3 -ErrorAction SilentlyContinue) {
    $pyExe = "python3"
}

if (-not $pyExe) {
    Write-Host "этап model_diskov.py пропущен: Python не найден"
} else {
    $modelScript = Join-Path $PSScriptRoot "model_diskov.py"
    $modelArgs = $pyPrefix + @($modelScript, "--proverka")
    & $pyExe @modelArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ПРОВЕРКА: ПРОВАЛ model"
        exit 1
    }
    Write-Host "=== Этап model_diskov.py успешно пройден ==="
}

Write-Host "ПРОВЕРКА: УСПЕХ"
exit 0

[CmdletBinding()]
param (
    [string]$SpecificTest = ""
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

$targetDir = Join-Path $projectRoot "target"
if (-not (Test-Path $targetDir)) {
    New-Item -ItemType Directory -Path $targetDir -Force | Out-Null
}

$zameryLog = Join-Path $targetDir "zamery.log"
if (-not $SpecificTest) {
    Set-Content -Path $zameryLog -Value ""
}

# Все интеграционные тесты собраны в одну программу tests\vse\main.rs (--test vse);
# Module — модуль (прежний файл tests\<Module>.rs), полное имя теста: <Module>::<Test>.
$tests = @(
    @{ Binary = "vse"; Module = "catalog";  Test = "perf_snapshot" },
    @{ Binary = "vse"; Module = "morph";    Test = "perf_tokenizer" },
    @{ Binary = "vse"; Module = "names";    Test = "perf_names" },
    @{ Binary = "vse"; Module = "pipeline"; Test = "perf_index" },
    @{ Binary = "vse"; Module = "search";   Test = "perf_search_text" },
    @{ Binary = "vse"; Module = "walk";     Test = "perf_walk" }
)

if ($SpecificTest) {
    $tests = $tests | Where-Object { $_.Test -eq $SpecificTest }
    if (-not $tests) {
        Write-Error "Unknown test: $SpecificTest"
        exit 1
    }
}

foreach ($t in $tests) {
    $binName = $t.Binary
    $testName = $t.Test
    $fullName = "$($t.Module)::$testName"
    Write-Host "============================================================"
    Write-Host "Running benchmark: $testName (tests/$binName/$($t.Module).rs)"
    Write-Host "Start time: $(Get-Date -Format 'HH:mm:ss')"
    Write-Host "============================================================"

    $header = "`n============================================================`n" +
              "BENCHMARK: $testName (tests/$binName/$($t.Module).rs)`n" +
              "Start time: $(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')`n" +
              "============================================================`n"
    Add-Content -Path $zameryLog -Value $header

    $tmpOut = Join-Path $targetDir "zamery-$testName.tmp.log"
    $tmpErr = Join-Path $targetDir "zamery-$testName.tmp.err.log"
    Set-Content -Path $tmpOut -Value ""
    Set-Content -Path $tmpErr -Value ""

    $args = @("test", "--profile", "zamery", "--test", $binName, "--", "--ignored", $fullName, "--exact", "--nocapture")

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    $proc = Start-Process -FilePath $cargo -ArgumentList $args -WorkingDirectory $projectRoot -PassThru -NoNewWindow -RedirectStandardOutput $tmpOut -RedirectStandardError $tmpErr
    $null = $proc.Handle

    # Timeout 10 min = 600000 ms
    $timeoutMs = 600000
    $finished = $proc.WaitForExit($timeoutMs)
    $sw.Stop()
    $elapsedSec = [Math]::Round($sw.Elapsed.TotalSeconds, 2)

    if (-not $finished) {
        Write-Host "WARNING: Timeout 10 minutes exceeded for test $testName! Killing process..."
        taskkill.exe /PID $proc.Id /T /F | Out-Null
        $timeoutMsg = "`n[TIMEOUT] Test $testName did not finish in 10 minutes ($elapsedSec s). Marked as not finished.`n"
        Add-Content -Path $zameryLog -Value $timeoutMsg
        Write-Host $timeoutMsg
        continue
    }

    $proc.WaitForExit()
    $exitCode = $proc.ExitCode

    if (Test-Path $tmpErr) {
        $errText = Get-Content -Path $tmpErr -Raw -ErrorAction SilentlyContinue
        if ($errText) {
            Add-Content -Path $tmpOut -Value "`n--- STDERR ---`n$errText"
        }
    }

    if (Test-Path $tmpOut) {
        $outText = Get-Content -Path $tmpOut -Raw -ErrorAction SilentlyContinue
        if ($outText) {
            Add-Content -Path $zameryLog -Value $outText
            Write-Host $outText
        }
    }

    $footer = "`nFinished: $testName, Exit code: $exitCode, Elapsed: $elapsedSec s`n"
    Add-Content -Path $zameryLog -Value $footer
    Write-Host $footer
}

Write-Host "All benchmarks finished. Result log: $zameryLog"

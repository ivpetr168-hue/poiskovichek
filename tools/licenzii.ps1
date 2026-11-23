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

Write-Host "Получение метаданных зависимостей через $cargo metadata..."
$metaJson = & $cargo metadata --format-version 1
if (-not $metaJson) {
    Write-Host "ОШИБКА: Не удалось получить метаданные cargo metadata"
    exit 1
}

$meta = $metaJson | ConvertFrom-Json

# Находим корневой пакет
$rootPkg = $meta.packages | Where-Object { $_.name -eq "poiskovichek" }
if (-not $rootPkg) {
    Write-Host "ОШИБКА: Корневой пакет 'poiskovichek' не найден в метаданных"
    exit 1
}

$nodesById = @{}
foreach ($node in $meta.resolve.nodes) {
    $nodesById[$node.id] = $node
}

$pkgsById = @{}
foreach ($pkg in $meta.packages) {
    $pkgsById[$pkg.id] = $pkg
}

# Обход графа зависимостей (BFS), исключая dev-зависимости
$visited = New-Object 'System.Collections.Generic.HashSet[string]'
$queue = New-Object 'System.Collections.Generic.Queue[string]'

$queue.Enqueue($rootPkg.id)
$visited.Add($rootPkg.id) | Out-Null

while ($queue.Count -gt 0) {
    $currId = $queue.Dequeue()
    $currNode = $nodesById[$currId]
    if (-not $currNode) { continue }

    foreach ($dep in $currNode.deps) {
        $isNonDev = $false
        foreach ($dk in $dep.dep_kinds) {
            # kind: null - обычная зависимость выпуска, kind: "build" - сборка (build.rs)
            if ($dk.kind -ne "dev") {
                $isNonDev = $true
                break
            }
        }
        if ($isNonDev) {
            if (-not $visited.Contains($dep.pkg)) {
                $visited.Add($dep.pkg) | Out-Null
                $queue.Enqueue($dep.pkg)
            }
        }
    }
}

$depPackages = @()
foreach ($id in $visited) {
    $p = $pkgsById[$id]
    if ($p.name -ne "poiskovichek") {
        $depPackages += $p
    }
}

$sortedPackages = $depPackages | Sort-Object -Property name, version

Write-Host "Найдено зависимостей выпуска (без dev): $($sortedPackages.Count)"

# Проверка лицензий на GPL / AGPL / LGPL
$copyleftStrict = @()
$copyleftDual = @()

foreach ($p in $sortedPackages) {
    $lic = $p.license
    if ($lic -match "GPL|LGPL|AGPL") {
        # Проверяем, является ли лицензия строго копилефтной или разрешает MIT/Apache (через OR)
        if ($lic -match "\bOR\b") {
            $copyleftDual += "$($p.name) $($p.version): $lic"
        } else {
            $copyleftStrict += "$($p.name) $($p.version): $lic"
        }
    }
}

if ($copyleftStrict.Count -gt 0) {
    Write-Host "ВНИМАНИЕ: Обнаружены строгие GPL/AGPL/LGPL зависимости:"
    $copyleftStrict | ForEach-Object { Write-Host "  $_" }
} else {
    Write-Host "Строгих (обязательных) GPL/AGPL/LGPL зависимостей не обнаружено."
}

if ($copyleftDual.Count -gt 0) {
    Write-Host "Зависимости с мульти-лицензией (OR), включающей GPL/LGPL опцию:"
    $copyleftDual | ForEach-Object { Write-Host "  $_" }
}

# Формирование THIRD_PARTY.md
$md = New-Object System.Text.StringBuilder

[void]$md.AppendLine("# Сторонние компоненты и лицензии")
[void]$md.AppendLine("")
[void]$md.AppendLine('Код **ПоисКовичеКа** распространяется под [MIT](LICENSE).')
[void]$md.AppendLine("Ниже приведён перечень сторонних компонентов, библиотек и словарей, включённых в поставку программы или используемых при её сборке.")
[void]$md.AppendLine("")
[void]$md.AppendLine("---")
[void]$md.AppendLine("")
[void]$md.AppendLine("## Словарь словоформ русского языка")
[void]$md.AppendLine("")
[void]$md.AppendLine('- **Компонент**: `morph.fst` (морфологический словарь словоформ русского языка).')
[void]$md.AppendLine("- **Источник данных и атрибуция**:")
[void]$md.AppendLine("  - Данные корпуса русского языка [OpenCorpora](https://opencorpora.org/) (авторы и участники проекта OpenCorpora).")
[void]$md.AppendLine("  - Морфологический словарь [pymorphy3-dicts-ru](https://pypi.org/project/pymorphy3-dicts-ru/) (репозиторий: https://github.com/no-plagiarism/pymorphy3-dicts).")
[void]$md.AppendLine("  - Точная версия исходного пакета: в доступных файлах репозитория версия пакета `pymorphy3-dicts-ru` не зафиксирована.")
[void]$md.AppendLine("- **Лицензии**:")
[void]$md.AppendLine('  - Данные OpenCorpora и производный словарь `morph.fst`: [Creative Commons Attribution-ShareAlike 3.0 Unported (CC BY-SA 3.0)](https://creativecommons.org/licenses/by-sa/3.0/).')
[void]$md.AppendLine('  - Код на Python в пакете `pymorphy3-dicts-ru`: MIT.')
[void]$md.AppendLine("- **Выполненные преобразования (производная работа)**:")
[void]$md.AppendLine("  1. Извлечение пар `(словоформа, идентификатор лексемы)` из SQLite-базы словаря (`morphology.sqlite3`, таблица `word_primary`).")
[void]$md.AppendLine("  2. Нормализация словоформ: приведение к нижнему регистру по стандарту Unicode и замена буквы «ё» на «е».")
[void]$md.AppendLine("  3. Дедупликация и объединение наборов лексем для совпадающих нормализованных форм (включая омонимы и вариативные формы).")
[void]$md.AppendLine("  4. Построение FST-автомата (библиотека `fst` 0.4) отображения «нормализованная форма -> смещение в таблице лексем».")
[void]$md.AppendLine("  5. Сериализация в компактный бинарный файл `assets/morph.fst` (~5 МБ) с 64-байтным заголовком (`PKMORPH1`, версия 1, счётчики, CRC32), FST-индексом и выровненной по 8 байтам таблицей лексем.")
[void]$md.AppendLine("")
[void]$md.AppendLine("### Условия лицензии Creative Commons Attribution-ShareAlike 3.0 Unported (CC BY-SA 3.0)")
[void]$md.AppendLine("")
[void]$md.AppendLine("Вы можете свободно делиться (копировать и распространять материал на любом носителе и в любом формате) и адаптировать (видоизменять и создавать новое на основе этого материала для любых целей, включая коммерческие) при соблюдении условий:")
[void]$md.AppendLine("- **Attribution (Атрибуция)**: вы должны указать авторство, предоставить ссылку на лицензию и обозначить изменения, если они были внесены.")
[void]$md.AppendLine("- **ShareAlike (С сохранением условий)**: если вы перерабатываете или видоизменяете материал, вы должны распространять свои результаты под той же лицензией, что и оригинал.")
[void]$md.AppendLine("")
[void]$md.AppendLine("Канонический текст лицензии CC BY-SA 3.0: https://creativecommons.org/licenses/by-sa/3.0/legalcode")
[void]$md.AppendLine("")
[void]$md.AppendLine("---")
[void]$md.AppendLine("")
[void]$md.AppendLine("## Зависимости программы (Rust Crates)")
[void]$md.AppendLine("")
[void]$md.AppendLine('Перечислены зависимости сборки и выполнения из `Cargo.lock`, включая зависимости для других платформ.')
[void]$md.AppendLine("")
[void]$md.AppendLine("| Крейт | Версия | Лицензия |")
[void]$md.AppendLine("|---|---|---|")

foreach ($p in $sortedPackages) {
    $name = $p.name
    $ver = $p.version
    $lic = if ($p.license) { $p.license } else { "—" }
    # Экранируем вертикальные черты для markdown-таблицы
    $licSafe = $lic.Replace("|", "\|")
    [void]$md.AppendLine("| $name | $ver | $licSafe |")
}

[void]$md.AppendLine("")
[void]$md.AppendLine("---")
[void]$md.AppendLine("")
[void]$md.AppendLine("## Сводка по лицензиям")
[void]$md.AppendLine("")
[void]$md.AppendLine("Таблица показывает лицензии, заявленные в метаданных пакетов. Тексты лицензий и уведомления авторов находятся в исходных пакетах.")
[void]$md.AppendLine("")
if ($copyleftDual.Count -gt 0) {
    [void]$md.AppendLine("Компоненты с двойной/тройной лицензией, упоминающей GPL/LGPL:")
    foreach ($item in $copyleftDual) {
        [void]$md.AppendLine("- " + [char]96 + $item + [char]96)
    }
    [void]$md.AppendLine("")
}
[void]$md.AppendLine("Исходный пакет [option-ext 0.2.0](https://crates.io/api/v1/crates/option-ext/0.2.0/download) доступен по [MPL 2.0](https://www.mozilla.org/en-US/MPL/2.0/).")

$outPath = Join-Path $projectRoot "THIRD_PARTY.md"
$utf8Bom = New-Object System.Text.UTF8Encoding($true)
[System.IO.File]::WriteAllText($outPath, $md.ToString(), $utf8Bom)

Write-Host "Файл успешно сформирован: $outPath"

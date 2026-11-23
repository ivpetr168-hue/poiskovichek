# -*- coding: utf-8 -*-
"""
tools/model_diskov.py - Подгонка модели дисков по замерам.
Используется только стандартная библиотека Python.
"""

import csv
import datetime
import json
import math
import os
import statistics
import sys
import tempfile

if sys.stdout and hasattr(sys.stdout, "reconfigure"):
    try:
        sys.stdout.reconfigure(encoding="utf-8")
    except Exception:
        pass
if sys.stderr and hasattr(sys.stderr, "reconfigure"):
    try:
        sys.stderr.reconfigure(encoding="utf-8")
    except Exception:
        pass

NABOR_SIZES = {
    "melkie": 4096,
    "srednie": 204800,
    "krupnye": 20971520,
}


def round_sig(val, sig=3):
    """Округление числа с плавающей точкой до sig значащих цифр для JSON."""
    if val is None:
        return None
    try:
        val = float(val)
    except (ValueError, TypeError):
        return None
    if math.isnan(val) or math.isinf(val):
        return None
    if val == 0:
        return 0.0
    return float(f"{val:.{sig}g}")


def fmt_md(val, sig=3):
    """Форматирование числа для Markdown с запятой в качестве десятичного разделителя."""
    if val is None:
        return "нет данных"
    try:
        val = float(val)
    except (ValueError, TypeError):
        return str(val)
    if math.isnan(val) or math.isinf(val):
        return "нет данных"
    if val == 0:
        return "0"
    abs_v = abs(val)
    if 0.0001 <= abs_v < 1000000:
        d = math.floor(math.log10(abs_v))
        decimals = max(0, sig - 1 - d)
        s = f"{val:.{decimals}f}"
        if "." in s:
            s = s.rstrip("0").rstrip(".")
    else:
        s = f"{val:.{sig}g}"
    return s.replace(".", ",")


def linear_regression(xs, ys):
    """
    МНК-регрессия y = a + b * x.
    Возвращает (intercept, slope, r2).
    """
    n = len(xs)
    if n < 2:
        return None, None, None
    mx = sum(xs) / n
    my = sum(ys) / n
    var_x = sum((x - mx) ** 2 for x in xs)
    if var_x == 0:
        return None, None, None
    cov_xy = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    slope = cov_xy / var_x
    intercept = my - slope * mx

    ss_tot = sum((y - my) ** 2 for y in ys)
    ss_res = sum((y - (intercept + slope * x)) ** 2 for x, y in zip(xs, ys))

    if ss_tot == 0:
        r2 = 1.0 if ss_res < 1e-12 else 0.0
    else:
        r2 = max(0.0, min(1.0, 1.0 - (ss_res / ss_tot)))

    return intercept, slope, r2


def read_diski_csv(diski_path):
    """
    Чтение файла классов дисков (<имя>-diski.csv).
    Возвращает (disk_classes, diski_path_if_found).
    """
    if not os.path.isfile(diski_path):
        return {}, None
    disk_classes = {}
    try:
        with open(diski_path, mode="r", encoding="utf-8", errors="replace") as f:
            reader = csv.reader(f)
            header = None
            for row in reader:
                if not row or not any(field.strip() for field in row):
                    continue
                if header is None:
                    header = [c.strip().lower() for c in row]
                    continue
                if len(row) >= 2:
                    d = row[0].strip()
                    c = row[1].strip()
                    if d and c:
                        disk_classes[d] = c
        return disk_classes, diski_path
    except OSError:
        return {}, None


def read_zamery_csv(vhod_path):
    """
    Чтение замеров из CSV.
    Возвращает (valid_rows, stats).
    """
    stats = {
        "vsego_strok": 0,
        "ispolzovano": 0,
        "propuscheno_faylov_0": 0,
        "propuscheno_bityh": 0,
        "kesh_da": 0,
        "povtor_da": 0,
    }
    valid_rows = []
    header_seen = False

    with open(vhod_path, mode="r", encoding="utf-8", errors="replace") as f:
        reader = csv.reader(f)
        for row in reader:
            if not row or not any(field.strip() for field in row):
                continue
            if not header_seen:
                if row[0].strip().lower() == "opyt":
                    header_seen = True
                    continue

            stats["vsego_strok"] += 1
            if len(row) < 12:
                stats["propuscheno_bityh"] += 1
                continue

            try:
                opyt = row[0].strip()
                diski = row[1].strip()
                k = int(row[2].strip())
                nabor = row[3].strip()
                poryadok = row[4].strip()
                povtor_n = row[5].strip()
                faylov = int(row[6].strip())
                bayt = float(row[7].strip())
                sekund = float(row[8].strip())
                kesh = row[9].strip().lower()
                povtor = row[10].strip().lower()
                potokov_str = row[11].strip()
                potokov = int(potokov_str) if potokov_str else k
            except (ValueError, IndexError):
                stats["propuscheno_bityh"] += 1
                continue

            if faylov == 0:
                stats["propuscheno_faylov_0"] += 1
                continue

            if sekund <= 0 or bayt < 0 or k <= 0 or faylov < 0 or potokov <= 0:
                stats["propuscheno_bityh"] += 1
                continue

            stats["ispolzovano"] += 1
            if kesh == "da":
                stats["kesh_da"] += 1
            if povtor == "da":
                stats["povtor_da"] += 1

            valid_rows.append({
                "opyt": opyt,
                "diski": diski,
                "k": k,
                "nabor": nabor,
                "poryadok": poryadok,
                "povtor_n": povtor_n,
                "faylov": faylov,
                "bayt": bayt,
                "sekund": sekund,
                "kesh": kesh,
                "povtor": povtor,
                "potokov": potokov,
                "t": sekund * k / faylov,
                "x_faylov": faylov / sekund,
                "x_bayt": bayt / sekund,
            })

    return valid_rows, stats


def fit_model(rows, disk_classes):
    """
    Расчёт модели дисков по опытам A, USL, D, E, C.
    """
    # Множество уникальных дисков
    single_disks = set()
    for r in rows:
        for d in r["diski"].split("+"):
            d_clean = d.strip()
            if d_clean:
                single_disks.add(d_clean)
        if r["poryadok"].startswith("zapis:"):
            d_clean = r["poryadok"][len("zapis:"):].strip()
            if d_clean:
                single_disks.add(d_clean)

    # 1. Опыт A
    rows_a = [r for r in rows if r["opyt"] == "A"]
    # Группировка (diski, k, nabor)
    grouped_a = {}
    for r in rows_a:
        key = (r["diski"], r["k"], r["nabor"])
        grouped_a.setdefault(key, []).append(r)

    a_agg = {}
    for (d, k, nab), g_rows in grouped_a.items():
        t_mean = statistics.mean(r["t"] for r in g_rows)
        x_faylov_mean = statistics.mean(r["x_faylov"] for r in g_rows)
        x_bayt_mean = statistics.mean(r["x_bayt"] for r in g_rows)
        # Размер
        if nab in NABOR_SIZES:
            s = NABOR_SIZES[nab]
        elif nab.isdigit():
            s = int(nab)
        else:
            s = statistics.mean(r["bayt"] / r["faylov"] for r in g_rows)

        a_agg[(d, k, nab)] = {
            "t_mean": t_mean,
            "x_faylov_mean": x_faylov_mean,
            "x_bayt_mean": x_bayt_mean,
            "size": s,
            "count": len(g_rows),
        }

    # По каждому (diski, k) - МНК t на s
    a_by_disk_k = {}
    for (d, k, nab), data in a_agg.items():
        a_by_disk_k.setdefault((d, k), []).append((data["size"], data["t_mean"], data["count"]))

    fit_a = {}
    skipped_diski_k = []
    for (d, k), pts in sorted(a_by_disk_k.items()):
        distinct_sizes = set(p[0] for p in pts)
        if len(distinct_sizes) < 2:
            skipped_diski_k.append(f"{d} (k={k}): меньше 2 разных размеров наборов")
            continue
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        intercept, slope, r2 = linear_regression(xs, ys)
        if slope is None or slope <= 0:
            skipped_diski_k.append(f"{d} (k={k}): некорректный наклон МНК (b <= 0)")
            continue
        alfa = intercept
        beta = 1.0 / slope
        alfa_ms = alfa * 1000.0
        beta_mb_s = beta / 1048576.0
        total_rows = sum(p[2] for p in pts)
        fit_a[(d, k)] = {
            "alfa_ms": alfa_ms,
            "beta_mb_s": beta_mb_s,
            "r2": r2,
            "n_rows": total_rows,
        }

    # 2. USL по каждому (diski, nabor)
    # X(k) по k = 1, 2, 4, 8 из опыта A
    usl_groups = {}
    for (d, k, nab), data in a_agg.items():
        usl_groups.setdefault((d, nab), {})[k] = data["x_faylov_mean"]

    fit_usl = {}
    for (d, nab), k_map in sorted(usl_groups.items()):
        if 1 not in k_map or k_map[1] <= 0:
            continue
        x1 = k_map[1]
        pts = []
        for k_val in (2, 4, 8):
            if k_val in k_map and k_map[k_val] > 0:
                c_k = k_map[k_val] / x1
                y = (k_val / c_k - 1.0) / (k_val - 1.0)
                pts.append((float(k_val), y))
        if len(pts) < 2:
            continue
        xs = [p[0] for p in pts]
        ys = [p[1] for p in pts]
        sigma, kappa, r2 = linear_regression(xs, ys)
        if sigma is None:
            continue
        if kappa > 0 and sigma < 1.0:
            k_opt = math.sqrt((1.0 - sigma) / kappa)
        else:
            k_opt = None
        fit_usl[(d, nab)] = {
            "sigma": sigma,
            "kappa": kappa,
            "k_opt": k_opt,
            "r2": r2,
        }

    # 3. Опыт D: (формат, потоков)
    rows_d = [r for r in rows if r["opyt"] == "D"]
    d_agg = {}
    for r in rows_d:
        fmt = r["nabor"]
        potokov = r["potokov"]
        ns_na_bayt = r["sekund"] * 1e9 / r["bayt"]
        mb_v_s = r["bayt"] / r["sekund"] / 1e6
        d_agg.setdefault((fmt, potokov), []).append((ns_na_bayt, mb_v_s))

    fit_d_table = {}
    for (fmt, potokov), vals in sorted(d_agg.items()):
        mean_ns = statistics.mean(v[0] for v in vals)
        mean_mb = statistics.mean(v[1] for v in vals)
        fit_d_table.setdefault(fmt, {})[potokov] = {
            "ns_na_bayt": mean_ns,
            "mb_v_s": mean_mb,
            "count": len(vals),
        }

    # Ускорение от потоков для D
    d_speedups = {}
    for fmt, p_map in fit_d_table.items():
        if 1 in p_map and 4 in p_map and p_map[1]["mb_v_s"] > 0:
            d_speedups[fmt] = p_map[4]["mb_v_s"] / p_map[1]["mb_v_s"]
        else:
            d_speedups[fmt] = None

    # razbor для JSON
    razbor_json = {}
    for fmt in fit_d_table:
        if 1 in fit_d_table[fmt]:
            razbor_json[fmt] = {
                "ns_na_bayt": round_sig(fit_d_table[fmt][1]["ns_na_bayt"]),
                "mb_v_s": round_sig(fit_d_table[fmt][1]["mb_v_s"]),
            }
        else:
            fmt_rows = [r for r in rows_d if r["nabor"] == fmt]
            if fmt_rows:
                ns_all = [r["sekund"] * 1e9 / r["bayt"] for r in fmt_rows]
                mb_all = [r["bayt"] / r["sekund"] / 1e6 for r in fmt_rows]
                razbor_json[fmt] = {
                    "ns_na_bayt": round_sig(statistics.median(ns_all)),
                    "mb_v_s": round_sig(statistics.median(mb_all)),
                }

    # 4. Опыт E: помехи записи
    rows_e = [r for r in rows if r["opyt"] == "E"]
    # Базовая скорость по диску: строки A того же диска с k = 4 и nabor == srednie (если нет - k = 1)
    e_baseline = {}
    for d in single_disks:
        a_k4_srednie = [r for r in rows_a if r["diski"] == d and r["k"] == 4 and r["nabor"] == "srednie"]
        if a_k4_srednie:
            e_baseline[d] = statistics.mean(r["bayt"] / r["sekund"] for r in a_k4_srednie)
        else:
            a_k1_srednie = [r for r in rows_a if r["diski"] == d and r["k"] == 1 and r["nabor"] == "srednie"]
            if a_k1_srednie:
                e_baseline[d] = statistics.mean(r["bayt"] / r["sekund"] for r in a_k1_srednie)
            else:
                e_baseline[d] = None

    grouped_e = {}
    for r in rows_e:
        grouped_e.setdefault((r["diski"], r["poryadok"]), []).append(r)

    fit_e = []
    skipped_e = []
    zapis_na_tom_zhe_vals = []
    zapis_na_drugom_vals = []

    for (d, poryadok), g_rows in sorted(grouped_e.items()):
        g_srednie = [r for r in g_rows if r["nabor"] == "srednie"]
        sel_rows = g_srednie if g_srednie else g_rows
        base_spd = e_baseline.get(d)
        if base_spd is None or base_spd <= 0:
            skipped_e.append(f"{d} ({poryadok}): нет базовой скорости из опыта A (k=4 или k=1 для srednie)")
            continue
        fakt = statistics.mean(r["bayt"] / r["sekund"] for r in sel_rows)
        if fakt <= 0:
            continue
        slowdown = base_spd / fakt
        w_disk = poryadok[len("zapis:"):].strip() if poryadok.startswith("zapis:") else poryadok.strip()
        is_same = (w_disk in d)
        fit_e.append({
            "diski": d,
            "poryadok": poryadok,
            "slowdown": slowdown,
            "is_same": is_same,
            "count": len(sel_rows),
        })
        if is_same:
            zapis_na_tom_zhe_vals.append(slowdown)
        else:
            zapis_na_drugom_vals.append(slowdown)

    pomehi_json = {}
    if zapis_na_tom_zhe_vals:
        pomehi_json["zapis_na_tom_zhe"] = round_sig(statistics.median(zapis_na_tom_zhe_vals))
    if zapis_na_drugom_vals:
        pomehi_json["zapis_na_drugom"] = round_sig(statistics.median(zapis_na_drugom_vals))

    # 5. Опыт C: совместная работа дисков
    rows_c = [r for r in rows if r["opyt"] == "C"]
    grouped_c = {}
    for r in rows_c:
        grouped_c.setdefault((r["diski"], r["k"]), []).append(r)

    fit_c = []
    skipped_c = []
    sovmestno_json = {}

    for (combo, k_val), g_rows in sorted(grouped_c.items()):
        g_srednie = [r for r in g_rows if r["nabor"] == "srednie"]
        if not g_srednie:
            skipped_c.append(f"{combo} (k={k_val}): нет строк опыта C с набором srednie")
            continue
        fakt = statistics.mean(r["bayt"] / r["sekund"] for r in g_srednie)
        parts = [p.strip() for p in combo.split("+") if p.strip()]
        summa = 0.0
        all_ok = True
        for part in parts:
            part_a = [r for r in rows_a if r["diski"] == part and r["k"] == k_val and r["nabor"] == "srednie"]
            if part_a:
                summa += statistics.mean(r["bayt"] / r["sekund"] for r in part_a)
            else:
                all_ok = False
                break
        if not all_ok:
            skipped_c.append(f"{combo} (k={k_val}): нет данных опыта A для отдельных дисков с k={k_val} и набором srednie")
            continue
        if summa <= 0:
            continue
        koeff = fakt / summa
        fit_c.append({
            "combo": combo,
            "k": k_val,
            "summa_mb": summa / 1048576.0,
            "fakt_mb": fakt / 1048576.0,
            "koeff": koeff,
            "count": len(g_srednie),
        })
        sovmestno_json.setdefault(combo, {})[str(k_val)] = round_sig(koeff)

    if sovmestno_json:
        pomehi_json["sovmestno"] = sovmestno_json

    # 6. Опыт B: порядок чтения на HDD (k = 1, набор melkie)
    rows_b = [r for r in rows if r["opyt"] == "B"]
    b_group = {}
    for r in rows_b:
        b_group.setdefault((r["diski"], r["poryadok"]), []).append(r)

    fit_b = []
    poryadok_hdd_json = {}
    for (d, poryadok), g_rows in sorted(b_group.items()):
        fps = statistics.mean(r["x_faylov"] for r in g_rows)
        papki_rows = b_group.get((d, "papki"))
        if papki_rows:
            fps_papki = statistics.mean(r["x_faylov"] for r in papki_rows)
            ratio = fps / fps_papki if fps_papki > 0 else None
        else:
            ratio = None
        fit_b.append({
            "diski": d,
            "poryadok": poryadok,
            "faylov_v_s": fps,
            "vo_skolko_raz": ratio,
            "count": len(g_rows),
            "povtor_da": sum(1 for r in g_rows if r["povtor"] == "da"),
        })
        poryadok_hdd_json.setdefault(d, {})[poryadok] = {
            "faylov_v_s": round_sig(fps),
            "vo_skolko_raz_bystree_papki": round_sig(ratio),
            "povtorov": len(g_rows),
        }

    # Агрегация по классам для JSON
    classes_map = {}
    for d in single_disks:
        cls_name = disk_classes.get(d, "Unknown")
        classes_map.setdefault(cls_name, []).append(d)

    klassy_json = {}
    for cls_name, c_disks in sorted(classes_map.items()):
        # Число строк A, использованных для этого класса
        cls_a_rows = [r for r in rows_a if any(d in r["diski"].split("+") for d in c_disks)]
        n_obs = len(cls_a_rows)

        # alfa_ms и beta_mb_v_s по k
        alfa_by_k = {}
        beta_by_k = {}
        for k_val in (1, 2, 4, 8):
            alfas = [fit_a[(d, k_val)]["alfa_ms"] for d in c_disks if (d, k_val) in fit_a]
            betas = [fit_a[(d, k_val)]["beta_mb_s"] for d in c_disks if (d, k_val) in fit_a]
            if alfas:
                alfa_by_k[str(k_val)] = round_sig(statistics.median(alfas))
            if betas:
                beta_by_k[str(k_val)] = round_sig(statistics.median(betas))

        # USL по наборам
        usl_by_nab = {}
        for nab in ("melkie", "srednie", "krupnye"):
            sigmas = [fit_usl[(d, nab)]["sigma"] for d in c_disks if (d, nab) in fit_usl]
            kappas = [fit_usl[(d, nab)]["kappa"] for d in c_disks if (d, nab) in fit_usl]
            r2s = [fit_usl[(d, nab)]["r2"] for d in c_disks if (d, nab) in fit_usl]
            if sigmas and kappas and r2s:
                med_sigma = statistics.median(sigmas)
                med_kappa = statistics.median(kappas)
                med_r2 = statistics.median(r2s)
                if med_kappa > 0 and med_sigma < 1.0:
                    med_k_opt = math.sqrt((1.0 - med_sigma) / med_kappa)
                else:
                    med_k_opt = None
                usl_by_nab[nab] = {
                    "sigma": round_sig(med_sigma),
                    "kappa": round_sig(med_kappa),
                    "k_opt": round_sig(med_k_opt),
                    "r2": round_sig(med_r2),
                }

        # Добавляем класс, если для него есть наблюдения или подобранные параметры
        if n_obs > 0 or alfa_by_k or beta_by_k:
            klassy_json[cls_name] = {
                "alfa_ms": alfa_by_k,
                "beta_mb_v_s": beta_by_k,
                "usl": usl_by_nab,
                "nablyudeniy": n_obs,
            }

    # Анализ дисков без класса и без опыта B
    diski_no_class = [d for d in sorted(single_disks) if d not in disk_classes]
    diski_with_b = set(r["diski"] for r in rows if r["opyt"] == "B")
    diski_no_b = [d for d in sorted(single_disks) if d not in diski_with_b]

    return {
        "single_disks": single_disks,
        "fit_a": fit_a,
        "fit_usl": fit_usl,
        "fit_d_table": fit_d_table,
        "d_speedups": d_speedups,
        "fit_e": fit_e,
        "fit_c": fit_c,
        "fit_b": fit_b,
        "poryadok_hdd_json": poryadok_hdd_json,
        "klassy_json": klassy_json,
        "razbor_json": razbor_json,
        "pomehi_json": pomehi_json,
        "skipped_diski_k": skipped_diski_k,
        "skipped_e": skipped_e,
        "skipped_c": skipped_c,
        "diski_no_class": diski_no_class,
        "diski_no_b": diski_no_b,
    }


def generate_markdown(vhod_path, diski_file_found, disk_classes, stats, fit_res):
    """
    Генерация отчёта docs/model-diskov-dannye.md.
    """
    now_str = datetime.datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    lines = []
    lines.append("# Модель дисков: что подобрано по замерам")
    lines.append("")

    # ## Что измерено
    lines.append("## Что измерено")
    lines.append(f"- **Файл CSV:** `{os.path.basename(vhod_path)}`")
    lines.append(f"- **Дата и время расчёта:** {now_str}")
    lines.append(f"- **Всего строк в файле:** {stats['vsego_strok']}")
    lines.append(f"- **Использовано строк:** {stats['ispolzovano']}")
    propuscheno_vsego = stats["propuscheno_faylov_0"] + stats["propuscheno_bityh"]
    lines.append(
        f"- **Пропущено строк:** {propuscheno_vsego} "
        f"(faylov == 0: {stats['propuscheno_faylov_0']}, битые строки: {stats['propuscheno_bityh']})"
    )
    lines.append(f"- **Число дисков:** {len(fit_res['single_disks'])}")
    if diski_file_found:
        lines.append(f"- **Файл классов дисков:** найден (`{os.path.basename(diski_file_found)}`)")
    else:
        lines.append("- **Файл классов дисков:** не найден (для всех дисков принят класс Unknown)")
    lines.append("")

    # ## Задержка и полоса по дискам
    lines.append("## Задержка и полоса по дискам")
    fit_a = fit_res["fit_a"]
    if not fit_a:
        lines.append("нет данных")
    else:
        lines.append("| Диск | Класс | k | alfa (мс) | beta (МБ/с) | R² |")
        lines.append("|---|---|---|---|---|---|")
        for (d, k), item in sorted(fit_a.items()):
            cls_name = disk_classes.get(d, "Unknown")
            alfa_str = fmt_md(item["alfa_ms"])
            beta_str = fmt_md(item["beta_mb_s"])
            r2_str = fmt_md(item["r2"])
            lines.append(f"| {d} | {cls_name} | {k} | {alfa_str} | {beta_str} | {r2_str} |")
    lines.append("")

    # ## Масштабируемость по числу читателей (USL)
    lines.append("## Масштабируемость по числу читателей (USL)")
    fit_usl = fit_res["fit_usl"]
    if not fit_usl:
        lines.append("нет данных")
    else:
        lines.append("| Диск | Набор | sigma | kappa | k* | R² |")
        lines.append("|---|---|---|---|---|---|")
        for (d, nab), item in sorted(fit_usl.items()):
            sigma_str = fmt_md(item["sigma"])
            kappa_str = fmt_md(item["kappa"])
            k_opt_str = fmt_md(item["k_opt"]) if item["k_opt"] is not None else "нет оптимума"
            r2_str = fmt_md(item["r2"])
            lines.append(f"| {d} | {nab} | {sigma_str} | {kappa_str} | {k_opt_str} | {r2_str} |")
    lines.append("")

    # ## Цена разбора текста
    lines.append("## Цена разбора текста")
    fit_d_table = fit_res["fit_d_table"]
    if not fit_d_table:
        lines.append("нет данных")
    else:
        lines.append("| Формат | Потоков | нс на байт | МБ/с |")
        lines.append("|---|---|---|---|")
        for fmt, p_map in sorted(fit_d_table.items()):
            for p, item in sorted(p_map.items()):
                ns_str = fmt_md(item["ns_na_bayt"])
                mb_str = fmt_md(item["mb_v_s"])
                lines.append(f"| {fmt} | {p} | {ns_str} | {mb_str} |")
        lines.append("")
        speedups = fit_res["d_speedups"]
        for fmt, spd in sorted(speedups.items()):
            if spd is not None:
                lines.append(f"Ускорение от потоков для формата `{fmt}` (4 / 1): {fmt_md(spd)}")
            else:
                lines.append(f"Ускорение от потоков для формата `{fmt}` (4 / 1): нет данных")
    lines.append("")

    # ## Помехи записи
    lines.append("## Помехи записи")
    fit_e = fit_res["fit_e"]
    if not fit_e:
        lines.append("нет данных")
    else:
        lines.append("| Вариант записи | Во сколько раз медленнее |")
        lines.append("|---|---|")
        for item in fit_e:
            z_str = fmt_md(item["slowdown"])
            loc_str = "запись на тот же диск" if item["is_same"] else "запись на другой диск"
            lines.append(f"| {item['diski']} ({item['poryadok']}, {loc_str}) | {z_str} |")
    lines.append("")

    # ## Совместная работа дисков
    lines.append("## Совместная работа дисков")
    fit_c = fit_res["fit_c"]
    if not fit_c:
        lines.append("нет данных")
    else:
        lines.append("| Комбинация | k | Сумма по отдельности (МБ/с) | Вместе (МБ/с) | Коэффициент |")
        lines.append("|---|---|---|---|---|")
        for item in fit_c:
            sum_str = fmt_md(item["summa_mb"])
            fakt_str = fmt_md(item["fakt_mb"])
            koeff_str = fmt_md(item["koeff"])
            lines.append(f"| {item['combo']} | {item['k']} | {sum_str} | {fakt_str} | {koeff_str} |")
    lines.append("")

    # ## Порядок чтения на HDD
    lines.append("## Порядок чтения на HDD")
    fit_b = fit_res["fit_b"]
    if not fit_b:
        lines.append("нет данных")
    else:
        lines.append("| Диск | Порядок | Файлов в секунду | Во сколько раз быстрее papki | Повторов |")
        lines.append("|---|---|---|---|---|")
        for item in fit_b:
            fps_str = fmt_md(item["faylov_v_s"])
            ratio_str = fmt_md(item["vo_skolko_raz"])
            lines.append(
                f"| {item['diski']} | {item['poryadok']} | {fps_str} | {ratio_str} | {item['count']} |"
            )
        n_krug = sum(item["povtor_da"] for item in fit_b)
        if n_krug:
            lines.append("")
            lines.append(f"Строк опыта B с чтением по кругу (`povtor=da`): {n_krug}")
    lines.append("")

    # ## Оговорки
    lines.append("## Оговорки")
    lines.append(f"- Строк с чтением через кэш (`kesh=da`): {stats['kesh_da']}")
    lines.append(f"- Строк с повторными замерами (`povtor=da`): {stats['povtor_da']}")
    lines.append(
        "- beta приведена в МиБ/с (1 МиБ = 1 048 576 байт, как в задании 250 МБ/с = 262144000 байт/с), "
        "цена разбора текста — в МБ/с (10⁶ байт) по формуле задания."
    )

    if fit_res["diski_no_class"]:
        lines.append(f"- Диски без указанного класса (Unknown): {', '.join(fit_res['diski_no_class'])}")
    else:
        lines.append("- Диски без указанного класса (Unknown): нет")

    if fit_res["diski_no_b"]:
        lines.append(f"- Диски без замеров опыта B: {', '.join(fit_res['diski_no_b'])}")
    else:
        lines.append("- Диски без замеров опыта B: нет")

    if fit_res["skipped_diski_k"]:
        lines.append(f"- Не удалось подогнать (diski, k): {'; '.join(fit_res['skipped_diski_k'])}")
    else:
        lines.append("- Не удалось подогнать (diski, k): нет")

    if fit_res["skipped_c"]:
        lines.append(f"- Не удалось посчитать совместную работу: {'; '.join(fit_res['skipped_c'])}")

    return "\n".join(lines) + "\n"


def generate_json(vhod_path, fit_res):
    """
    Генерация структуры для assets/model-diskov.json.
    """
    now_iso = datetime.datetime.now().strftime("%Y-%m-%dT%H:%M:%S")
    return {
        "versiya": 1,
        "model": "t(s)=alfa+s/beta; X(k)=lambda*k/(1+sigma*(k-1)+kappa*k*(k-1))",
        "istochnik": {
            "csv": os.path.basename(vhod_path),
            "vremya": now_iso,
            "diskov": len(fit_res["single_disks"]),
        },
        "klassy": fit_res["klassy_json"],
        "razbor": fit_res["razbor_json"],
        "pomehi": fit_res["pomehi_json"],
        "poryadok_hdd": fit_res["poryadok_hdd_json"],
    }


def run_proverka():
    """
    Режим самопроверки (--proverka).
    """
    temp_dir = tempfile.mkdtemp(prefix="proverka_model_diskov_")
    try:
        csv_path = os.path.join(temp_dir, "zamery_test.csv")
        diski_csv_path = os.path.join(temp_dir, "zamery_test-diski.csv")

        # 1. Запись zamery_test-diski.csv
        with open(diski_csv_path, mode="w", encoding="utf-8", newline="") as f:
            f.write("disk,class,physical_disk_id,bus,seek,sektor\n")
            f.write("C:,Nvme,1,NVMe,net,512\n")

        # 2. Синтез данных zamery_test.csv
        alfa0 = 0.004
        beta0 = 262144000.0  # 250 * 1024 * 1024 байт/с
        sigma_true = 0.08
        kappa_true = 0.006

        k_values = [1, 2, 4, 8]
        nabors = [
            ("melkie", 4096),
            ("srednie", 204800),
            ("krupnye", 20971520),
        ]

        with open(csv_path, mode="w", encoding="utf-8", newline="") as f:
            writer = csv.writer(f)
            writer.writerow([
                "opyt", "diski", "k", "nabor", "poryadok", "povtor_n",
                "faylov", "bayt", "sekund", "kesh", "povtor", "potokov"
            ])
            for k in k_values:
                f_k = 1.0 + sigma_true * (k - 1) + kappa_true * k * (k - 1)
                for nabor_name, s in nabors:
                    t = f_k * (alfa0 + s / beta0)
                    for rep in (1, 2, 3):
                        faylov = 50
                        sekund = faylov * t / k
                        bayt = faylov * s
                        writer.writerow([
                            "A", "C:", k, nabor_name, "", rep,
                            faylov, bayt, f"{sekund:.12f}", "obhod", "net", k
                        ])

            # Опыт B: файлов в секунду по порядкам (истина: papki 100, mft 400, sluchayno 50)
            for poryadok_b, fps_b in (("papki", 100.0), ("mft", 400.0), ("sluchayno", 50.0)):
                for rep in (1, 2, 3):
                    # чуть разные повторы, среднее по ним точно равно fps_b
                    fps_rep = fps_b * (1.0 + 0.1 * (rep - 2))
                    sekund_b = 10.0
                    faylov_b = round(fps_rep * sekund_b)
                    writer.writerow([
                        "B", "D:", 1, "melkie", poryadok_b, rep,
                        faylov_b, faylov_b * 4096, f"{sekund_b:.6f}", "net", "net", 1
                    ])

            # Строка D
            writer.writerow([
                "D", "C:", 1, "txt", "", 1,
                1, 1000000, 0.0123, "obhod", "net", 1
            ])

        # Чтение и расчёт теми же функциями
        disk_classes, _ = read_diski_csv(diski_csv_path)
        rows, stats = read_zamery_csv(csv_path)
        fit_res = fit_model(rows, disk_classes)

        # Проверка расхождений
        # alfa(k) истина = f(k)*4 мс, beta(k) истина = 250/f(k) МБ/с
        # sigma истина = 0.08, kappa истина = 0.006
        checks = []
        for k in k_values:
            f_k = 1.0 + sigma_true * (k - 1) + kappa_true * k * (k - 1)
            alfa_true = f_k * 4.0
            beta_true = 250.0 / f_k

            item_a = fit_res["fit_a"].get(("C:", k))
            if item_a is None:
                checks.append((f"alfa (k={k}, мс)", alfa_true, None, 100.0))
                checks.append((f"beta (k={k}, МБ/с)", beta_true, None, 100.0))
            else:
                alfa_found = item_a["alfa_ms"]
                beta_found = item_a["beta_mb_s"]
                alfa_diff = abs(alfa_found - alfa_true) / abs(alfa_true) * 100.0
                beta_diff = abs(beta_found - beta_true) / abs(beta_true) * 100.0
                checks.append((f"alfa (k={k}, мс)", alfa_true, alfa_found, alfa_diff))
                checks.append((f"beta (k={k}, МБ/с)", beta_true, beta_found, beta_diff))

        for nab in ("melkie", "srednie", "krupnye"):
            usl_item = fit_res["fit_usl"].get(("C:", nab))
            if usl_item is None:
                checks.append((f"sigma ({nab})", sigma_true, None, 100.0))
                checks.append((f"kappa ({nab})", kappa_true, None, 100.0))
                continue
            sigma_found = usl_item["sigma"]
            kappa_found = usl_item["kappa"]
            sigma_diff = abs(sigma_found - sigma_true) / abs(sigma_true) * 100.0
            kappa_diff = abs(kappa_found - kappa_true) / abs(kappa_true) * 100.0
            checks.append((f"sigma ({nab})", sigma_true, sigma_found, sigma_diff))
            checks.append((f"kappa ({nab})", kappa_true, kappa_found, kappa_diff))

        # Опыт B: файлов в секунду и ускорение относительно papki
        b_found = {item["poryadok"]: item for item in fit_res["fit_b"] if item["diski"] == "D:"}
        for poryadok_b, fps_true in (("papki", 100.0), ("mft", 400.0), ("sluchayno", 50.0)):
            item_b = b_found.get(poryadok_b)
            if item_b is None:
                checks.append((f"B {poryadok_b} (файлов/с)", fps_true, None, 100.0))
            else:
                fps_found = item_b["faylov_v_s"]
                checks.append((f"B {poryadok_b} (файлов/с)", fps_true, fps_found,
                               abs(fps_found - fps_true) / fps_true * 100.0))
            ratio_true = fps_true / 100.0
            ratio_found = item_b["vo_skolko_raz"] if item_b is not None else None
            if ratio_found is None:
                checks.append((f"B {poryadok_b} (к papki)", ratio_true, None, 100.0))
            else:
                checks.append((f"B {poryadok_b} (к papki)", ratio_true, ratio_found,
                               abs(ratio_found - ratio_true) / ratio_true * 100.0))

        # Опыт B в JSON и в документе
        json_b = generate_json(csv_path, fit_res).get("poryadok_hdd", {}).get("D:", {})
        mft_json = json_b.get("mft", {}).get("vo_skolko_raz_bystree_papki")
        if mft_json is None or abs(mft_json - 4.0) > 0.05:
            checks.append(("B JSON poryadok_hdd mft", 4.0, mft_json, 100.0))
        md_text = generate_markdown(csv_path, diski_csv_path, disk_classes, stats, fit_res)
        if "## Порядок чтения на HDD" not in md_text:
            checks.append(("B раздел документа", 1.0, None, 100.0))

        # Вывод таблицы
        print("-" * 75)
        print(f"{'Показатель':<22} {'Истина':<16} {'Найдено':<16} {'Расхождение %':<15}")
        print("-" * 75)
        all_passed = True
        failed_items = []
        for name, truth, found, diff in checks:
            t_str = f"{truth:.4f}" if truth is not None else "нет"
            f_str = f"{found:.4f}" if found is not None else "нет"
            d_str = f"{diff:.2f}%"
            print(f"{name:<22} {t_str:<16} {f_str:<16} {d_str:<15}")
            if diff > 5.0:
                all_passed = False
                failed_items.append((name, truth, found, diff))
        print("-" * 75)

        if all_passed:
            print("САМОПРОВЕРКА: УСПЕХ")
            return 0
        else:
            for name, truth, found, diff in failed_items:
                print(f"Отказ показателя {name}: истина={truth}, найдено={found}, расхождение={diff:.2f}%")
            print("САМОПРОВЕРКА: ПРОВАЛ")
            return 1

    finally:
        # Очистка временной папки только средствами os
        for root, dirs, files in os.walk(temp_dir, topdown=False):
            for file in files:
                try:
                    os.remove(os.path.join(root, file))
                except OSError:
                    pass
            for d in dirs:
                try:
                    os.rmdir(os.path.join(root, d))
                except OSError:
                    pass
        try:
            os.rmdir(temp_dir)
        except OSError:
            pass


def main():
    script_dir = os.path.dirname(os.path.abspath(__file__))
    base_dir = os.path.abspath(os.path.join(script_dir, ".."))

    # Ручной разбор sys.argv
    vhod_arg = None
    dokument_arg = None
    json_arg = None
    proverka = False

    i = 1
    while i < len(sys.argv):
        arg = sys.argv[i]
        if arg == "--proverka":
            proverka = True
            i += 1
        elif arg == "--vhod":
            if i + 1 >= len(sys.argv):
                sys.stderr.write("Ошибка: для ключа --vhod не указан путь к файлу.\n")
                sys.exit(1)
            vhod_arg = sys.argv[i + 1]
            i += 2
        elif arg == "--dokument":
            if i + 1 >= len(sys.argv):
                sys.stderr.write("Ошибка: для ключа --dokument не указан путь к файлу.\n")
                sys.exit(1)
            dokument_arg = sys.argv[i + 1]
            i += 2
        elif arg == "--json":
            if i + 1 >= len(sys.argv):
                sys.stderr.write("Ошибка: для ключа --json не указан путь к файлу.\n")
                sys.exit(1)
            json_arg = sys.argv[i + 1]
            i += 2
        elif arg in ("-h", "--help", "--pomosch"):
            print("Использование: model_diskov.py [--vhod <файл.csv>] [--dokument <файл.md>] [--json <файл.json>] [--proverka]")
            sys.exit(0)
        else:
            sys.stderr.write(f"Ошибка: неизвестный параметр: {arg}\n")
            sys.exit(1)

    if proverka:
        code = run_proverka()
        sys.exit(code)

    # Стандартный запуск
    if vhod_arg is None:
        vhod_path = os.path.join(base_dir, "zamery.csv")
    elif not os.path.isabs(vhod_arg):
        vhod_path = os.path.join(base_dir, vhod_arg)
    else:
        vhod_path = vhod_arg

    if dokument_arg is None:
        dokument_path = os.path.join(base_dir, "docs", "model-diskov-dannye.md")
    elif not os.path.isabs(dokument_arg):
        dokument_path = os.path.join(base_dir, dokument_arg)
    else:
        dokument_path = dokument_arg

    if json_arg is None:
        json_path = os.path.join(base_dir, "assets", "model-diskov.json")
    elif not os.path.isabs(json_arg):
        json_path = os.path.join(base_dir, json_arg)
    else:
        json_path = json_arg

    if not os.path.isfile(vhod_path):
        sys.stderr.write(f"Ошибка: входной файл замеров не найден: {vhod_path}\n")
        sys.exit(1)

    # Ищем файл классов дисков: <имя>-diski.csv
    base_no_ext = os.path.splitext(vhod_path)[0]
    diski_csv_cand = f"{base_no_ext}-diski.csv"
    disk_classes, diski_file_found = read_diski_csv(diski_csv_cand)

    valid_rows, stats = read_zamery_csv(vhod_path)
    fit_res = fit_model(valid_rows, disk_classes)

    # Создание директорий при необходимости
    doc_dir = os.path.dirname(os.path.abspath(dokument_path))
    if doc_dir and not os.path.exists(doc_dir):
        os.makedirs(doc_dir, exist_ok=True)

    json_dir = os.path.dirname(os.path.abspath(json_path))
    if json_dir and not os.path.exists(json_dir):
        os.makedirs(json_dir, exist_ok=True)

    # Запись Markdown
    md_content = generate_markdown(vhod_path, diski_file_found, disk_classes, stats, fit_res)
    with open(dokument_path, mode="w", encoding="utf-8", newline="") as f:
        f.write(md_content)

    # Запись JSON
    json_data = generate_json(vhod_path, fit_res)
    with open(json_path, mode="w", encoding="utf-8", newline="") as f:
        json.dump(json_data, f, ensure_ascii=False, indent=2)
        f.write("\n")

    # Короткий вывод в консоль по-русски
    propuscheno_vsego = stats["propuscheno_faylov_0"] + stats["propuscheno_bityh"]
    classes_str = ", ".join(sorted(fit_res["klassy_json"].keys())) if fit_res["klassy_json"] else "нет"
    print(f"Обработан файл замеров: {os.path.basename(vhod_path)}")
    print(f"Строк всего: {stats['vsego_strok']}, использовано: {stats['ispolzovano']}, пропущено: {propuscheno_vsego} (faylov == 0: {stats['propuscheno_faylov_0']}, битых: {stats['propuscheno_bityh']})")
    print(f"Подобраны модели для классов: {classes_str}")
    print(f"Результаты сохранены:")
    print(f"  Отчёт: {dokument_path}")
    print(f"  JSON:  {json_path}")


if __name__ == "__main__":
    main()

"""唯讀比較現有 Python parser 與 Rust v2 parser。"""

from __future__ import annotations

import argparse
import json
import sqlite3
import subprocess
import sys
from collections import Counter
from datetime import datetime
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PYTHON_ROOT = ROOT.parent / "doujin-tagger"
DEFAULT_DB = PYTHON_ROOT / "doujin.db"
DEFAULT_RUST_BIN = ROOT / "target" / "debug" / "doujin-parser.exe"
CORE_FIELDS = ("event", "circle", "author_raw", "title", "is_dl", "subcategory")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db", type=Path, default=DEFAULT_DB)
    parser.add_argument("--rust-bin", type=Path, default=DEFAULT_RUST_BIN)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--limit", type=int)
    parser.add_argument("--batch-size", type=int, default=500)
    parser.add_argument("--examples", type=int, default=20)
    return parser.parse_args()


def immutable_connection(path: Path) -> sqlite3.Connection:
    absolute = path.resolve()
    uri = f"file:{absolute.as_posix()}?mode=ro&immutable=1"
    return sqlite3.connect(uri, uri=True)


def load_rows(path: Path, limit: int | None) -> list[tuple[int, str]]:
    query = "SELECT id, filename FROM doujinshi ORDER BY id"
    params: tuple[int, ...] = ()
    if limit is not None:
        query += " LIMIT ?"
        params = (limit,)
    with immutable_connection(path) as connection:
        return [(row[0], row[1]) for row in connection.execute(query, params)]


def chunks(items: list[tuple[int, str]], size: int):
    for start in range(0, len(items), size):
        yield items[start : start + size]


def rust_results(
    binary: Path, rows: list[tuple[int, str]], batch_size: int
) -> list[dict]:
    if not binary.is_file():
        raise FileNotFoundError(
            f"Rust parser 不存在：{binary}；請先在 Rust 專案根目錄執行 cargo build -p doujin-parser"
        )

    results = []
    for batch in chunks(rows, batch_size):
        requests = [
            {"filename": filename, "parody_evidence": []} for _, filename in batch
        ]
        completed = subprocess.run(
            [str(binary)],
            input=json.dumps(requests, ensure_ascii=False),
            text=True,
            encoding="utf-8",
            capture_output=True,
            check=False,
        )
        if completed.returncode != 0:
            raise RuntimeError(f"Rust parser 失敗：{completed.stderr.strip()}")
        parsed = json.loads(completed.stdout)
        if len(parsed) != len(batch):
            raise RuntimeError(
                f"Rust parser 回傳 {len(parsed)} 筆，但輸入為 {len(batch)} 筆"
            )
        results.extend(parsed)
    return results


def python_result(filename: str) -> dict:
    if str(PYTHON_ROOT) not in sys.path:
        sys.path.insert(0, str(PYTHON_ROOT))
    from parser import parse_filename  # pylint: disable=import-outside-toplevel

    parsed = parse_filename(filename)
    return {
        "event": parsed.event,
        "circle": parsed.circle,
        "author_raw": parsed.author,
        "title": parsed.title,
        "parody": parsed.parody,
        "is_dl": parsed.is_dl,
        "subcategory": parsed.detected_category,
    }


def rust_core(result: dict) -> dict:
    return {
        "event": result["event"],
        "circle": result["circle"],
        "author_raw": result["authors"]["raw"],
        "title": result["title"],
        "parody": result["parody"]["canonical"] if result["parody"] else None,
        "is_dl": result["is_dl"],
        "subcategory": result["classification"]["subcategory"],
    }


def difference_fields(python: dict, rust: dict) -> list[str]:
    return [field for field in CORE_FIELDS if python[field] != rust[field]]


def contains_percent_escape(value: str) -> bool:
    hexadecimal = set("0123456789abcdefABCDEF")
    return any(
        value[index] == "%"
        and value[index + 1] in hexadecimal
        and value[index + 2] in hexadecimal
        for index in range(len(value) - 2)
    )


def has_leading_source_tag(value: str) -> bool:
    if not value.startswith("["):
        return False
    closing = value.find("]")
    return closing > 1 and "@" in value[1:closing]


def archive_stem(value: str) -> str:
    return value[:-4] if value.lower().endswith(".zip") else value


def has_unbalanced_delimiters(value: str) -> bool:
    stem = archive_stem(value)
    return stem.count("(") != stem.count(")") or stem.count("[") != stem.count("]")


def reason_labels(filename: str, python: dict, rust_result: dict, fields: list[str]):
    labels = []
    if contains_percent_escape(filename) and fields:
        labels.append("url_percent_decoding_difference")
    if has_leading_source_tag(filename) and fields:
        labels.append("leading_source_tag_difference")
    if (
        "circle" in fields
        and "[digital" in filename.lower()
        and python["circle"] is None
        and rust_result["circle"] is not None
    ):
        labels.append("legacy_skip_tag_consumes_circle")
    if has_unbalanced_delimiters(filename) and fields:
        labels.append("malformed_delimiter_difference")
    if "title" in fields and archive_stem(filename).rstrip().endswith("]"):
        labels.append("trailing_square_bracket_policy_difference")
    if "title" in fields and archive_stem(filename).rstrip().endswith("】"):
        labels.append("fullwidth_trailing_marker_moved_to_other_info")
    if "title" in fields and archive_stem(filename).lower().endswith(".zip"):
        labels.append("duplicate_archive_extension_difference")
    if "title" in fields and ("（" in filename or "）" in filename):
        labels.append("fullwidth_parenthesis_normalization_difference")
    if "title" in fields and any(
        marker in rust_result["title"].lower()
        for marker in ("[dl版]", "[digital]", "[chinese]", "[english]", "[korean]")
    ):
        labels.append("nonterminal_known_marker_difference")
    if "title" in fields and any(
        item["reason"] == "insufficient_parody_evidence"
        for item in rust_result["other_info"]
    ):
        labels.append("trailing_parentheses_moved_to_other_info")
    if rust_result["next_action"] == "external_metadata":
        labels.append("creator_deferred_to_external_metadata")
    if "author_raw" in fields and python["author_raw"] is None and rust_result["authors"]["raw"]:
        labels.append("nested_author_supported")
    if "is_dl" in fields and rust_result["is_dl"]:
        labels.append("expanded_dl_detection")
    if "is_dl" in fields and python["is_dl"] and not rust_result["is_dl"]:
        labels.append("legacy_dl_marker_not_recognized")
    if "_" in filename and fields:
        labels.append("underscore_normalization_difference")
    return labels


def report(
    db_path: Path,
    rust_binary: Path,
    rows: list[tuple[int, str]],
    rust_parsed: list[dict],
    examples_limit: int,
    database_unchanged: bool,
) -> str:
    field_counts: Counter[str] = Counter()
    signatures: Counter[tuple[str, ...]] = Counter()
    reasons: Counter[str] = Counter()
    classifications: Counter[str] = Counter()
    statuses: Counter[str] = Counter()
    core_differences = []
    parody_differences = 0
    parody_evidence_gaps = 0

    for (record_id, filename), rust_result in zip(rows, rust_parsed, strict=True):
        python = python_result(filename)
        rust = rust_core(rust_result)
        fields = difference_fields(python, rust)
        field_counts.update(fields)
        if fields:
            signatures[tuple(fields)] += 1
            labels = reason_labels(filename, python, rust_result, fields)
            reasons.update(labels or ["unclassified_core_difference"])
            core_differences.append(
                {
                    "id": record_id,
                    "filename": filename,
                    "fields": fields,
                    "labels": labels,
                    "python": {field: python[field] for field in CORE_FIELDS},
                    "rust": {field: rust[field] for field in CORE_FIELDS},
                }
            )

        if python["parody"] != rust["parody"]:
            parody_differences += 1
            if rust["parody"] is None and any(
                item["reason"] == "insufficient_parody_evidence"
                for item in rust_result["other_info"]
            ):
                parody_evidence_gaps += 1

        classification = rust_result["classification"]
        classification_name = classification["top_level"]
        if classification["subcategory"]:
            classification_name += f" / {classification['subcategory']}"
        classifications[classification_name] += 1
        statuses[rust_result["parse_status"]] += 1

    total = len(rows)
    different = sum(signatures.values())
    lines = [
        "# Python／Rust Parser Shadow Comparison",
        "",
        f"- 產生時間：{datetime.now().astimezone().isoformat(timespec='seconds')}",
        f"- 資料庫：`{db_path.resolve()}`",
        f"- Rust binary：`{rust_binary.resolve()}`",
        f"- 比較筆數：{total:,}",
        f"- 資料庫 size／mtime 檢查未變更：{'是' if database_unchanged else '否'}",
        "- Rust 輸入未提供原作 evidence；原作差異獨立統計，不計入核心結構差異。",
        "- 作者比較使用 `authors.raw`，不把 Rust 新增的作者清單視為差異。",
        "",
        "## 摘要",
        "",
        f"- 核心欄位完全相同：{total - different:,}（{percent(total - different, total)}）",
        f"- 至少一個核心欄位不同：{different:,}（{percent(different, total)}）",
        f"- 原作結果不同：{parody_differences:,}",
        f"- 其中屬於缺少 evidence：{parody_evidence_gaps:,}",
        "",
        "## 核心差異欄位",
        "",
    ]
    lines.extend(counter_table(field_counts, "欄位"))
    lines.extend(["", "## 差異組合", ""])
    signature_rows = Counter({", ".join(key): value for key, value in signatures.items()})
    lines.extend(counter_table(signature_rows, "欄位組合", limit=20))
    lines.extend(["", "## 初步原因標籤", ""])
    lines.extend(counter_table(reasons, "原因"))
    lines.extend(["", "## Rust 分類分布", ""])
    lines.extend(counter_table(classifications, "分類"))
    lines.extend(["", "## Rust Parse Status", ""])
    lines.extend(counter_table(statuses, "狀態"))
    lines.extend(["", "## 核心差異範例", ""])
    if not core_differences:
        lines.append("沒有核心差異。")
    core_differences.sort(
        key=lambda item: (bool(item["labels"]), -len(item["fields"]), item["id"])
    )
    for index, item in enumerate(core_differences[:examples_limit], start=1):
        lines.extend(
            [
                f"### {index}. DB id {item['id']}",
                "",
                f"- 檔名：`{escape_code(item['filename'])}`",
                f"- 差異欄位：`{', '.join(item['fields'])}`",
                f"- 初步標籤：`{', '.join(item['labels']) if item['labels'] else '未分類'}`",
                f"- Python：`{escape_code(json.dumps(item['python'], ensure_ascii=False, sort_keys=True))}`",
                f"- Rust：`{escape_code(json.dumps(item['rust'], ensure_ascii=False, sort_keys=True))}`",
                "",
            ]
        )
    return "\n".join(lines).rstrip() + "\n"


def percent(value: int, total: int) -> str:
    return "0.00%" if total == 0 else f"{value / total:.2%}"


def counter_table(counter: Counter[str], label: str, limit: int | None = None):
    rows = counter.most_common(limit)
    lines = [f"| {label} | 筆數 |", "|---|---:|"]
    lines.extend(f"| `{escape_code(name)}` | {count:,} |" for name, count in rows)
    if not rows:
        lines.append("| （無） | 0 |")
    return lines


def escape_code(value: str) -> str:
    return value.replace("`", "\\`").replace("|", "\\|")


def main() -> int:
    args = parse_args()
    if args.batch_size <= 0:
        raise ValueError("--batch-size 必須大於 0")
    db_before = (args.db.stat().st_size, args.db.stat().st_mtime_ns)
    rows = load_rows(args.db, args.limit)
    rust_parsed = rust_results(args.rust_bin, rows, args.batch_size)
    db_after = (args.db.stat().st_size, args.db.stat().st_mtime_ns)
    rendered = report(
        args.db,
        args.rust_bin,
        rows,
        rust_parsed,
        args.examples,
        db_before == db_after,
    )
    if args.output:
        args.output.write_text(rendered, encoding="utf-8", newline="\n")
        print(f"已寫入 {args.output}")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

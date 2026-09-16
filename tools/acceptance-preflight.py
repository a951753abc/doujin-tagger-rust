"""驗收接正式 catalog 前的資料保護 preflight。

依序檢查：catalog 檔存在、沒有 doujin 相關程序在跑、沒有未 checkpoint 的 WAL，
再把 catalog 複製到 backups/ 並以 SHA-256 驗證副本完整性。任何一關不過即 exit 1，
且不留半成品備份目錄。對來源 catalog 只做讀取，不以 sqlite3 開啟它。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from typing import Callable, Iterable, Sequence

WATCHED_PROCESS_NAMES = {
    "doujin-http.exe",
    "doujin-desktop.exe",
    "doujin-launcher.exe",
    "JP6 Doujin Archive.exe",
}

REPO_ROOT = Path(__file__).resolve().parents[1]
CHUNK_SIZE = 1024 * 1024


class PreflightError(Exception):
    """preflight 檢查未通過。"""


def default_catalog_path() -> Path:
    """從 %LOCALAPPDATA%\\Doujin Tagger\\launcher.json 讀取正式 catalog 路徑。"""
    local_app_data = os.environ.get("LOCALAPPDATA")
    if not local_app_data:
        raise PreflightError("環境變數 LOCALAPPDATA 未設定，無法定位 launcher.json")
    launcher_path = Path(local_app_data) / "Doujin Tagger" / "launcher.json"
    if not launcher_path.is_file():
        raise PreflightError(f"找不到 launcher.json：{launcher_path}")
    try:
        data = json.loads(launcher_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PreflightError(f"讀取 launcher.json 失敗：{error}") from error
    catalog = data.get("catalog")
    if not catalog:
        raise PreflightError(f"launcher.json 缺少 catalog 欄位：{launcher_path}")
    return Path(catalog)


def list_processes_via_powershell() -> list[dict]:
    """透過 Get-CimInstance Win32_Process 取得目前程序清單（Name／ProcessId／CommandLine）。"""
    command = (
        "Get-CimInstance Win32_Process | "
        "Select-Object ProcessId, Name, CommandLine | ConvertTo-Json -Compress"
    )
    last_error: Exception | None = None
    for executable in ("pwsh", "powershell"):
        try:
            completed = subprocess.run(
                [executable, "-NoProfile", "-NonInteractive", "-Command", command],
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                check=False,
            )
        except FileNotFoundError as error:
            last_error = error
            continue
        if completed.returncode != 0:
            last_error = RuntimeError(completed.stderr.strip())
            continue
        output = completed.stdout.strip()
        if not output:
            return []
        parsed = json.loads(output)
        if isinstance(parsed, dict):
            parsed = [parsed]
        return parsed
    raise PreflightError(f"無法透過 pwsh 或 powershell 取得程序清單：{last_error}")


def find_watched_processes(processes: Iterable[dict]) -> list[dict]:
    return [process for process in processes if process.get("Name") in WATCHED_PROCESS_NAMES]


def sha256_of_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while True:
            chunk = handle.read(CHUNK_SIZE)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="驗收接正式 catalog 前的資料保護 preflight")
    parser.add_argument("--topic", required=True, help="備份目錄名稱使用的主題 slug")
    parser.add_argument("--catalog", type=Path, default=None, help="正式 catalog 路徑；預設讀 launcher.json")
    parser.add_argument("--backups-dir", type=Path, default=None, help="備份根目錄；預設 <repo>/backups")
    parser.add_argument("--dry-run", action="store_true", help="只做檢查，不複製備份")
    parser.add_argument("--json", action="store_true", help="以 JSON 格式輸出機器可讀摘要")
    return parser


def run(args: argparse.Namespace, list_processes: Callable[[], list[dict]]) -> dict:
    """執行 preflight 檢查；成功回傳摘要 dict，失敗拋出 PreflightError。"""
    catalog = Path(args.catalog) if args.catalog is not None else default_catalog_path()

    if not catalog.is_file():
        raise PreflightError(f"catalog 檔不存在：{catalog}")

    watched = find_watched_processes(list_processes())
    if watched:
        details = "\n".join(
            f"  PID={process.get('ProcessId')} Name={process.get('Name')} "
            f"CommandLine={process.get('CommandLine')}"
            for process in watched
        )
        raise PreflightError(f"偵測到 doujin 相關程序仍在執行，請先正常關閉：\n{details}")

    wal_path = catalog.with_name(catalog.name + "-wal")
    if wal_path.is_file() and wal_path.stat().st_size > 0:
        raise PreflightError(
            f"WAL 檔尚未 checkpoint（{wal_path}，{wal_path.stat().st_size} bytes）："
            "先用程式正常關閉讓 WAL checkpoint，備份不會包含 WAL 內容"
        )

    source_sha256 = sha256_of_file(catalog)
    source_size = catalog.stat().st_size

    summary: dict = {
        "topic": args.topic,
        "catalog": str(catalog),
        "size": source_size,
        "sha256": source_sha256,
        "dry_run": bool(args.dry_run),
        "backup_path": None,
    }

    if args.dry_run:
        return summary

    backups_dir = Path(args.backups_dir) if args.backups_dir is not None else REPO_ROOT / "backups"
    timestamp = datetime.now().strftime("%Y%m%d-%H%M%S")
    backup_dir = backups_dir / f"pre-{args.topic}-{timestamp}"
    backup_path = backup_dir / catalog.name

    backup_dir_created = False
    try:
        backup_dir.mkdir(parents=True, exist_ok=False)
        backup_dir_created = True
        shutil.copyfile(catalog, backup_path)
        copy_sha256 = sha256_of_file(backup_path)
        if copy_sha256 != source_sha256:
            raise PreflightError(
                f"備份 SHA-256 與來源不一致（來源 {source_sha256}，副本 {copy_sha256}）"
            )
    except PreflightError:
        if backup_dir_created:
            shutil.rmtree(backup_dir, ignore_errors=True)
        raise
    except OSError as error:
        if backup_dir_created:
            shutil.rmtree(backup_dir, ignore_errors=True)
        raise PreflightError(f"建立備份失敗：{error}") from error

    summary["backup_path"] = str(backup_path)
    return summary


def format_report(summary: dict) -> str:
    lines = [f"catalog 路徑：{summary['catalog']}"]
    if summary.get("backup_path"):
        lines.append(f"備份路徑：{summary['backup_path']}")
    else:
        lines.append("備份路徑：（dry-run，未建立）")
    lines.append(f"大小：{summary['size']} bytes")
    lines.append(f"SHA-256：{summary['sha256']}")
    return "\n".join(lines)


def main(
    argv: Sequence[str] | None = None,
    list_processes: Callable[[], list[dict]] | None = None,
) -> int:
    parser = build_arg_parser()
    args = parser.parse_args(argv)
    processes_fn = list_processes if list_processes is not None else list_processes_via_powershell

    try:
        summary = run(args, processes_fn)
    except PreflightError as error:
        print(f"[FAIL] {error}", file=sys.stderr)
        return 1

    if args.json:
        print(json.dumps(summary, ensure_ascii=False))
    else:
        print(format_report(summary))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

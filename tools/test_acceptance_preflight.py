"""tools/acceptance-preflight.py 的驗收測試。

以 importlib 載入腳本模組（檔名含連字號，無法直接 import），呼叫其 main() 並注入
假程序清單；全部使用 tmp_path 內的假 catalog 檔配合 --catalog 與 --backups-dir，
不碰正式 DB。
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path

MODULE_PATH = Path(__file__).resolve().parent / "acceptance-preflight.py"


def _load_module():
    spec = importlib.util.spec_from_file_location("acceptance_preflight", MODULE_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


preflight = _load_module()


def _no_processes() -> list[dict]:
    return []


def _with_doujin_http() -> list[dict]:
    return [
        {
            "ProcessId": 4242,
            "Name": "doujin-http.exe",
            "CommandLine": "doujin-http.exe L:\\doujin-tagger-rust\\doujin-v2.db 5000",
        }
    ]


def _make_catalog(tmp_path: Path) -> Path:
    catalog = tmp_path / "doujin-v2.db"
    catalog.write_bytes(b"fake catalog payload for preflight test\x00\x01\x02" * 137)
    return catalog


def test_success_no_process_no_wal_creates_verified_backup(tmp_path):
    catalog = _make_catalog(tmp_path)
    expected_sha256 = hashlib.sha256(catalog.read_bytes()).hexdigest()
    backups_dir = tmp_path / "backups"

    exit_code = preflight.main(
        [
            "--topic", "unit-test",
            "--catalog", str(catalog),
            "--backups-dir", str(backups_dir),
            "--json",
        ],
        list_processes=_no_processes,
    )

    assert exit_code == 0
    backup_dirs = list(backups_dir.glob("pre-unit-test-*"))
    assert len(backup_dirs) == 1
    backup_file = backup_dirs[0] / catalog.name
    assert backup_file.is_file()
    assert hashlib.sha256(backup_file.read_bytes()).hexdigest() == expected_sha256


def test_json_summary_contains_source_sha256(tmp_path, capsys):
    catalog = _make_catalog(tmp_path)
    expected_sha256 = hashlib.sha256(catalog.read_bytes()).hexdigest()
    backups_dir = tmp_path / "backups"

    exit_code = preflight.main(
        [
            "--topic", "unit-test",
            "--catalog", str(catalog),
            "--backups-dir", str(backups_dir),
            "--json",
        ],
        list_processes=_no_processes,
    )
    captured = capsys.readouterr()
    summary = json.loads(captured.out)

    assert exit_code == 0
    assert summary["sha256"] == expected_sha256


def test_watched_process_running_blocks_and_leaves_no_backup(tmp_path):
    catalog = _make_catalog(tmp_path)
    backups_dir = tmp_path / "backups"

    exit_code = preflight.main(
        ["--topic", "unit-test", "--catalog", str(catalog), "--backups-dir", str(backups_dir)],
        list_processes=_with_doujin_http,
    )

    assert exit_code == 1
    assert not list(backups_dir.glob("pre-unit-test-*"))


def test_nonempty_wal_blocks_and_leaves_no_backup(tmp_path):
    catalog = _make_catalog(tmp_path)
    wal_path = catalog.with_name(catalog.name + "-wal")
    wal_path.write_bytes(b"pending wal frames")
    backups_dir = tmp_path / "backups"

    exit_code = preflight.main(
        ["--topic", "unit-test", "--catalog", str(catalog), "--backups-dir", str(backups_dir)],
        list_processes=_no_processes,
    )

    assert exit_code == 1
    assert not list(backups_dir.glob("pre-unit-test-*"))


def test_dry_run_creates_no_backup(tmp_path):
    catalog = _make_catalog(tmp_path)
    backups_dir = tmp_path / "backups"

    exit_code = preflight.main(
        [
            "--topic", "unit-test",
            "--catalog", str(catalog),
            "--backups-dir", str(backups_dir),
            "--dry-run",
        ],
        list_processes=_no_processes,
    )

    assert exit_code == 0
    assert not backups_dir.exists()


def test_missing_catalog_fails(tmp_path):
    missing_catalog = tmp_path / "does-not-exist.db"
    backups_dir = tmp_path / "backups"

    exit_code = preflight.main(
        [
            "--topic", "unit-test",
            "--catalog", str(missing_catalog),
            "--backups-dir", str(backups_dir),
        ],
        list_processes=_no_processes,
    )

    assert exit_code == 1

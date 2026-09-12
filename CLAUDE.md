# CLAUDE.md

JP6 Doujin Archive 是本機優先的同人作品收藏管理工具（Rust workspace）；專案說明見 [README.md](README.md)。

## 驗收硬規則

- 驗收一律接正式 catalog：路徑以 `%LOCALAPPDATA%\Doujin Tagger\launcher.json` 的 `catalog` 欄位為準，臨時 fixture 對使用者沒有說服力。
- 接正式 catalog 之前先跑：
  ```powershell
  python tools/acceptance-preflight.py --topic <主題>
  ```
  exit 非 0 不得繼續。
- 掃描只用 `POST /api/scans/preflight` 回報預計新增／tombstone 數；套用（`POST /api/scans` 含 `apply_safe_renames`）留給使用者按，不代為套用。
- 規則變更、重建 release（exe 會被鎖）前先停 server；停用期間提醒使用者勿自行套用。
- 殺程序規則見 `~/.claude/rules/common/rules.md`「Shell 慣例」，不在此重述。

## 開發驗證

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --locked
cargo build --release --locked -p doujin-http -p doujin-launcher
```

起 server：

```powershell
target/release/doujin-http.exe <db> 5000
```

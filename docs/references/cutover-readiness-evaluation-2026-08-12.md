# 2026-08-12 Rust v2 cutover readiness 評估

## 結論

批次 20 的非切換演練通過；正式切換仍未執行。新的 `test_cutover_readiness.ps1` 以目前修正版 v2 演練產物通過 24 項 checks，來源與 candidate 在 preflight 前後 SHA-256 均未改變，沒有 catalog sidecar、server process 或 port 5000 listener。

## 自動化邊界

Preflight 只讀取 reports、計算 SHA-256、檢查程序／listener，並以 immutable read-only path audit binary 即時重查 13,566 個 current paths。它不建立或修改 catalog、不啟動 HTTP server，也不把 readiness 視為正式 Go 授權。

`run_migration_rehearsal.ps1` 新增安全的 `TargetFileName` 參數；預設仍是 `doujin-v2-rehearsal.db`，正式 runbook 使用單一 leaf 名稱 `doujin-v2.db`。實際跑過一次隔離測試，migration gate 通過、target 使用指定名稱、預設名稱沒有被建立；測試產物隨後移除。

## 驗證結果

- Legacy SHA-256：`2E0733C6E3700D6410335242C30738DCCB2EAAE848441F65B211EDB06D592385`。
- Candidate SHA-256：`F7F8A0F3F31DEC5A1BB6AC3F8580DFCFE3340EE0613235EFA4EE00B7DBD55A90`。
- Migration gate、saved path audit 與 fresh path audit 全部通過。
- 13,566 個 current paths：ZIP 11,763、圖片資料夾 1,803；missing 與 media-kind mismatch 均為 0。
- Honeyview 路徑、`480x640` 與品質 100 均可重套。
- Python／Rust server 均未執行；port 5000 未占用。
- 本機工具鏈：Rust 1.97.1、Cargo 1.97.1，Cargo.lock 存在。
- Locked release build 成功；`doujin-http.exe` SHA-256 為 `DCB6095323441D47D2F5B099B3BFE80A752EABDB4AF438A78A44C7852F8CA9C8`，release path-audit binary SHA-256 為 `F75D2B8B83972BB4386A6159CE4EF0119E6F5033612CCB82AEF0AC2B46641208`。
- 最終一輪 24 項 preflight 使用上述 release binaries，而非 debug build。
- Fail-closed 測試由作業系統配置一個暫時 listener；preflight 只讓 `cutover_port_is_free` 失敗，正確回傳 `blocked` 與 exit code 2。測試 listener 隨後停止，沒有使用正式 port 5000。
- 子 PowerShell 測試同時確認 path-audit stdout／stderr 必須明確以 UTF-8 解碼，避免含日文路徑的 JSON 受主機預設 encoding 影響。

## Rollback 風險分流

Runbook 將 rollback 分成 Go 前與 Go 後。Go 前僅允許 health、查詢與可重建 thumbnail 驗收，可直接停止 Rust 並回到未修改的 legacy catalog。Go 後若已有 metadata／tag 寫入，回到 Python 會遺失 v2-only 變更；若已有 scan、move 或 delete，舊 filepath 可能失效，必須先人工核對 file-operation journal 與實體檔案，不能自動回切。

固定天數的 rollback retention 尚未由收藏管理者決定，因此採較安全的條件：在明確關閉 rollback window 且另有已驗證備份前，不刪除 legacy catalog 或 cutover artifacts。

## 產物

- `tools/test_cutover_readiness.ps1`
- `docs/references/formal-cutover-and-rollback-runbook.md`
- `target/formal-rehearsal-20260812-media-kind/cutover-readiness-report.json`

下一步只有在收藏管理者明確授權正式切換後，才依 runbook 建立新的 timestamped cutover directory、final `doujin-v2.db` 與正式 readiness report。

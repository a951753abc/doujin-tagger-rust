# 2026-08-12 正式切換前路徑與 UI 驗收

## 結論

批次 19 的切換前驗收通過。過程先修正 legacy 遷移把圖片資料夾誤標成 ZIP 的缺口，再由正式 `doujin.db` 的靜態副本建立新 v2 演練 catalog。正式來源沒有被 Rust HTTP server 開啟或修改。

這次結果仍不是正式切換授權；它證明目前的遷移產物、實體收藏路徑、既有設定與 localhost UI 可以進入切換 runbook 階段。

## 遷移缺口與修正

第一次路徑盤點發現 13,566 筆 current paths 中，有 11,763 筆是 `.zip` 一般檔案，另有 1,803 筆是實體圖片資料夾。舊版資料同時支援這兩種型態，但 migration import 原先將所有收藏硬編碼為 `media_kind=zip`。

修正後的規則是：legacy filepath 以不分大小寫的 `.zip` 副檔名判定為 `zip`，其餘保留為 `image_folder`。Migration report 與 acceptance gate 會分別比對兩種筆數；整合測試也加入圖片資料夾匯入案例。

## 路徑盤點

盤點程式以 SQLite `mode=ro&immutable=1`、read-only flags 與 `PRAGMA query_only=ON` 開啟唯一演練產物，並在前後比對 catalog BLAKE3。所有 library roots 與 current paths 均通過：

| Root | Current paths | ZIP | 圖片資料夾 |
|---|---:|---:|---:|
| `I:\同人誌` | 12,875 | 11,131 | 1,744 |
| `I:\商業誌` | 296 | 237 | 59 |
| `H:\` | 395 | 395 | 0 |
| 合計 | 13,566 | 11,763 | 1,803 |

缺少、無法讀取、媒體種類不符、symlink、root 外路徑及無 root 路徑均為 0。完整機器可讀結果保存在演練產物旁的 `path-audit-report.json`。

## 舊設定重套

Migration report 只保存允許重套的三個 legacy setting；UI 驗收在演練 catalog 的一次性工作副本上重新保存：

- 閱讀器：`C:\Program Files\Honeyview\Honeyview.exe`，檔案存在且為絕對路徑。
- 縮圖尺寸：`480x640`。
- WebP 品質：`100`。
- 沒有環境變數 override。

唯一演練產物沒有交給 server；設定寫入、縮圖 state 與 cache 都只發生在工作副本，驗收後一併移除。

## localhost UI／API 驗收

- Server 只監聽 `127.0.0.1:53187`；非 localhost Host 得到 HTTP 421。
- Health 回應正常，UI 顯示 13,566 筆有效收藏。
- 搜尋可得到唯一預期結果，詳細資料與 metadata endpoint 正常。
- 統計頁顯示同人誌 12,104、CG 1,126、商業誌 336，合計與 catalog 一致。
- 工作台可載入空的批次選取與同名候選狀態。
- ZIP 與圖片資料夾各抽查一筆縮圖，最終皆回傳 HTTP 200、`image/webp` 與非空內容。
- Browser console 的 warning／error 為 0。
- 沒有執行重新掃描、檔案開啟、閱讀器開啟、metadata 修改、外部搜尋、搬移或刪除。

第一次停止工作副本 server 時，背景 worker 曾回報一筆舊尺寸縮圖結果無法寫回：設定變更已把該 state 重新排為 pending，但舊工作仍在完成中。Application service 現在會辨識並丟棄這種 stale result，保留新 fingerprint 的 pending state；新增的 regression test 通過，重新建置後的短程 server 驗收 stderr 為空。

機器可讀摘要保存在 `pre-cutover-acceptance.json`。

## 保留產物與下一道閘門

保留目錄已隨 Rust 專案移至 `L:\doujin-tagger-rust\target\formal-rehearsal-20260812-media-kind`：

- `doujin-v2-rehearsal.db`
- `migration-report.json`
- `acceptance-gate.json`
- `path-audit-report.json`
- `pre-cutover-acceptance.json`
- `cutover-readiness-report.json`

v2 catalog SHA-256 為 `F7F8A0F3F31DEC5A1BB6AC3F8580DFCFE3340EE0613235EFA4EE00B7DBD55A90`。正式切換／rollback runbook 與只讀 preflight 已在批次 20 驗證；在收藏管理者另行明確回答 Go 前，仍不替換 `doujin.db`、不切換正式入口。

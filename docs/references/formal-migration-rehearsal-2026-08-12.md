# 2026-08-12 正式 catalog 的 Rust v2 遷移演練

## 結論

DEC-037 的「catalog 資料轉換驗收閘門」通過。正式 `doujin.db` 沒有被原地升級，也沒有交給 Rust HTTP server；演練只讀取 SHA-256 完全相同的靜態副本，建立一個全新的 v2 catalog。

這份結論不代表已授權或完成正式切換。實體路徑、圖片資料夾媒體種類、閱讀器／縮圖設定及 localhost UI 已在後續批次 19 驗收通過；正式切換與 rollback runbook 仍屬下一道閘門。

## 安全前置條件

- 正式 catalog：`L:\doujin-tagger\doujin.db`
- 正式 catalog 大小：12,103,680 bytes
- SHA-256：`2E0733C6E3700D6410335242C30738DCCB2EAAE848441F65B211EDB06D592385`
- 複製前、靜態副本、runner 完成後的正式來源 SHA-256 完全一致。
- 正式 catalog 旁沒有 `-wal`、`-shm` 或 journal。
- 靜態複製期間持有只允許其他 reader 的來源檔案鎖；寫入者無法在 hash 與複製之間插入變更。
- 沒有執行中的 `doujin-http`，也沒有 command line 指向本專案或 `doujin.db` 的舊版 Python 程序。
- Runner 回報來源以 `mode=ro&immutable=1`、`SQLITE_OPEN_READ_ONLY` 與 `PRAGMA query_only=ON` 開啟。
- Runner 讀取前後 BLAKE3 均為 `8dba28bfa552d3f48b766089505d0197f2d7eb40ad48c8fd5dce1d797513829d`。

## 匯入數量

| 項目 | 舊 catalog | v2 catalog |
|---|---:|---:|
| Library roots | 3 | 3 |
| 收藏 | 13,566 | 13,566 |
| ZIP 收藏 | 11,763 | 11,763 |
| 圖片資料夾 | 1,803 | 1,803 |
| Current locations | — | 13,566 |
| Metadata assertions | — | 87,116 |
| Metadata selections | — | 87,116 |
| Effective metadata rows | — | 13,566 |
| Tags | 0 | 0 |
| Tag links | 0 | 0 |

Tags 與 tag links 的零筆結果與來源一致，因此 migration gate 視為通過；這不是 runner 遺漏既有 tag 的判定。

## 空值保存

| Effective field | 舊 catalog 空值 | v2 catalog 空值 |
|---|---:|---:|
| authors | 2,897 | 2,897 |
| circle | 631 | 631 |
| classification | 0 | 0 |
| event | 3,295 | 3,295 |
| is_dl | 0 | 0 |
| parody | 1,023 | 1,023 |
| title | 0 | 0 |

所有欄位逐項一致。`folder` 另有 395 筆空值，但它不是 v2 effective metadata 欄位，也不作為收藏身分或場次的替代來源。

## 分類 mapping

| 舊值 | v2 上層分類 | v2 子分類 | 筆數 |
|---|---|---|---:|
| `CG` | CG | — | 1,126 |
| `同人誌` | 同人誌 | — | 12,104 |
| `商業誌` | 商業誌 | — | 4 |
| `官能小説` | 商業誌 | 官能小説 | 2 |
| `成年コミック` | 商業誌 | 成年コミック | 330 |

這符合 DEC-004：商業誌子分類保留，但統一歸屬「商業誌」。

## 驗證結果

- Migration status：`completed`
- Runner exit code：0
- Path conflicts：0
- Blocking issues：0
- `PRAGMA integrity_check`：`ok`
- Foreign-key violations：0
- Count mismatches：0
- Tag name mismatches：0
- Tag link mismatches：0
- Metadata 均勻抽樣：要求 100 筆、檢查 100 筆、差異 0
- Acceptance checks：全部通過

## 產物

修正圖片資料夾媒體種類後，取代用的演練產物已隨 Rust 專案移至 `L:\doujin-tagger-rust\target\formal-rehearsal-20260812-media-kind`：

- `doujin-v2-rehearsal.db`
- `migration-report.json`
- `acceptance-gate.json`

本次 v2 演練 catalog 大小為 40,374,272 bytes，SHA-256 為 `F7F8A0F3F31DEC5A1BB6AC3F8580DFCFE3340EE0613235EFA4EE00B7DBD55A90`。驗收通過後，腳本已移除一次性的 `legacy-source-copy.db` 與空的 target WAL／SHM。v2 演練 catalog 不會被自動設為正式 catalog。

## 正式切換狀態

1. 路徑、設定與 localhost UI 驗收已完成，詳見 `pre-cutover-path-and-ui-acceptance-2026-08-12.md`。
2. 正式切換與 rollback runbook、只讀 preflight 已完成非切換演練，詳見 `formal-cutover-and-rollback-runbook.md` 與 `cutover-readiness-evaluation-2026-08-12.md`。
3. 尚未取得或執行正式 Go 授權；`doujin.db` 與正式入口維持原狀。

# Parser 黃金語料審閱指南

## 目的

[`parser-corpus-v1.json`](../../tests/fixtures/parser-corpus-v1.json) 是 Python 現況與 Rust v2 共用的 parser 驗收資料。它不直接複製現有資料庫欄位，因為現有欄位可能已包含舊 parser 的誤判；每個 `expected` 都必須依 BDD 決策重新確認。

Parser Corpus v1 已於 2026-08-12 完成兩輪審閱，31 筆案例全部為 `accepted`，並成為 Rust parser 必須通過的黃金測試。後續新增案例先標記為 `draft`；完成審閱後再納入下一版 accepted corpus。

## 案例結構

每筆案例包含：

| 欄位 | 意義 |
|---|---|
| `id` | 穩定案例編號，用於審閱與測試失敗訊息 |
| `review_status` | `draft`、`accepted` 或 `rejected` |
| `origin` | 來自現有收藏或 BDD 設計案例；不保存實際檔案路徑 |
| `decisions` | 對應的 DEC 決策編號 |
| `tags` | 案例涵蓋的結構或風險 |
| `input.filename` | parser 的原始輸入 |
| `input.parody_evidence` | 本案例提供給 parser 的原作證據；空陣列代表沒有證據 |
| `expected.classification` | 上層分類、子分類與原始分類標記 |
| `expected.event` | 場次；無可靠值時為 `null` |
| `expected.leading_bracket_raw` | 未破壞的 leading bracket 原文 |
| `expected.circle` | 社團；無可靠值時為 `null` |
| `expected.authors` | 作者區段原文與拆分後清單 |
| `expected.title` | 去除已辨識結構後的標題 |
| `expected.parody` | 原作 raw、canonical 與採用證據；無可靠證據時為 `null` |
| `expected.identifiers` | 從檔名明確取得的外部識別碼 |
| `expected.other_info` | 有意義但不能可靠分類的原文及原因 |
| `expected.ignored_segments` | 版本、語言、日期等不屬於主要 metadata 的標記 |
| `expected.is_dl` | 是否明確標記為 DL／Digital 版本 |
| `expected.parse_status` | `complete`、`partial` 或 `title_only` |
| `expected.next_action` | `none` 或 `external_metadata` |

`parody_evidence` 是必要的測試前置條件。例如尾端 `(角色名稱)` 沒有證據時必須進入 `other_info`；相同位置若有 confirmed dictionary 或 alias 證據，才可成為原作。

## 第一批覆蓋範圍

第一批共 24 筆案例，涵蓋：

- 完整同人誌、任意場次、同人誌與 CG 分類前綴
- 成年コミック、官能小説、一般コミック
- `、`、`,` 與不自動拆分的 `&`、`＆`、`×`、`／`
- 巢狀作者括號、破損括號、非尾端作者括號
- 無原作證據的尾端括號與已確認 alias
- 修正版、別スキャン、全形括號、日期、RJ 編號與 DL 標記

審閱與現況對照：

- [批次 01：基本結構與分類](review-batch-01-standard-and-classification.md)
- [批次 02：作者拆分與模糊分隔符](review-batch-02-author-separators.md)
- [批次 03：巢狀與破損括號](review-batch-03-nested-and-malformed.md)
- [批次 04：尾端括號與版本標記](review-batch-04-trailing-parentheses-and-markers.md)
- [批次 05：識別碼與日期前綴](review-batch-05-identifiers-and-date-prefixes.md)
- [現有 Python parser 對照基線](current-parser-baseline.md)
- [完整收藏 shadow comparison](shadow-comparison-v1.md)
- [批次 06：影子比對與正規化邊界](review-batch-06-shadow-normalization.md)

## Shadow comparison

`tools/shadow_compare.py` 以 SQLite `mode=ro&immutable=1` 開啟既有資料庫，只讀取 `id` 與 `filename`，再批次比較 Python 與 Rust parser。它不更新收藏、metadata 或 parser 版本。

```powershell
cargo build -p doujin-parser
python tools/shadow_compare.py --output docs/parser-corpus/shadow-comparison-v1.md
```

報告會在執行前後檢查資料庫 size 與 mtime；若不同，報告會明確標記為未通過唯讀檢查。

## 審閱方式

審閱時只需針對案例 ID 回覆：

```text
parser-v2-case-001：接受
parser-v2-case-009：作者應改為……
parser-v2-case-015：整筆移除
```

也可以依 `tags` 分批審閱。建議順序是：

1. `standard` 與 `classification`
2. `author-splitting`
3. `nested` 與 `malformed`
4. `trailing-parentheses` 與 `alias`
5. `marker` 與 `identifier`

## 對應需求

- [Rust v2 的檔名解析與 metadata 來源優先序](../bdd/proposed/01-parser-and-metadata-priority.feature)
- [Rust v2 的社團與作者解析](../bdd/proposed/02-circle-and-author-parsing.feature)
- [BDD 領域決策紀錄](../bdd/decisions.md)

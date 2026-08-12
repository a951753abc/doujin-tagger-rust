# 現有功能 BDD 基線

## 目的

這一批 feature files 從目前可觀察的產品行為反推需求，作為 Rust v2 開始前的共同基線。它們描述「系統目前怎麼做」，不表示每個行為都應保留。

已確認的需求決策記錄於 [領域決策紀錄](decisions.md)；與現況不同的 Rust v2 行為放在 [`proposed/`](proposed/) 下，不回頭改寫現況基線。

本輪範圍包含：

- Web UI 與 HTTP API
- 收藏來源設定與重新掃描
- 檔名解析與分類
- 搜尋、瀏覽、metadata、tags 與批次操作
- 檔案開啟、閱讀、搬移與刪除
- Web metadata 建議、重複偵測與合併
- 縮圖、統計與基本安全邊界

本輪不包含 `cleanup*.py`、`fix_*.py`、`merge_parody.py` 等一次性修復腳本；這些腳本將來可作為 parser corpus 與 migration 情境的來源。

## 標籤

| 標籤 | 意義 |
|---|---|
| `@current` | 可由目前程式碼確認的既有行為 |
| `@needs-confirmation` | 現況存在，但是否應成為 Rust v2 需求需要使用者裁決 |
| `@destructive` | 會搬移、刪除或批次覆寫資料 |
| `@external` | 依賴外部網站、外部程式或作業系統 |
| `@security` | 涉及路徑、來源、輸入 allowlist 或請求邊界 |
| `@ui-local` | 僅保存在目前瀏覽器，不是伺服器端資料 |
| `@proposed` | 已提出的 Rust v2 行為，尚未全部成為最終 acceptance baseline |
| `@dec-*` | 此 scenario 追溯到已記錄的領域決策 |

每個 scenario 另有領域編號，例如 `@scan-001`。編號用於討論，不代表執行順序。

## 審閱方式

請對 scenario 做以下其中一種判定：

- 接受：此行為應保留為 Rust v2 acceptance criterion。
- 修改：情境成立，但 Given／When／Then 或資料規則需要調整。
- 移除：這只是舊實作細節，不再是需求。
- 新增：缺少重要情境、例外或反例。

建議先審閱 [待確認決策](review-questions.md)，再逐一檢查 feature files：

| 順序 | Feature | Scenarios | 主要現況來源 |
|---|---|---:|---|
| 1 | [收藏來源與掃描](01-library-scan.feature) | 11 | `scan.py`、`models.py`、`app.py` |
| 2 | [檔名解析與分類](02-filename-parsing.feature) | 13 | `parser.py`、`scan.py` |
| 3 | [搜尋與瀏覽](03-search-and-browse.feature) | 11 | `models.py`、`app.py`、`templates/index.html` |
| 4 | [Metadata、tags 與批次編輯](04-metadata-and-tags.feature) | 10 | `models.py`、`templates/index.html` |
| 5 | [檔案操作與閱讀](05-file-actions.feature) | 14 | `app.py`、`templates/index.html` |
| 6 | [外部補標、重複偵測與合併](06-enrichment-and-duplicates.feature) | 11 | `web_enrich.py`、`normalize.py` |
| 7 | [設定、縮圖與統計](07-settings-thumbnails-and-stats.feature) | 12 | `config.py`、`thumbs.py`、`models.py` |
| 8 | [安全與輸入邊界](08-security-boundaries.feature) | 6 | `app.py`、`models.py` |

第一版合計 88 個 scenarios。

目前另有 14 個 Rust v2 proposed features：

- [Rust v2 的檔名解析與 metadata 來源優先序](proposed/01-parser-and-metadata-priority.feature)
- [Rust v2 的社團與作者解析](proposed/02-circle-and-author-parsing.feature)
- [Rust v2 的掃描與檔案生命週期](proposed/03-scan-and-file-lifecycle.feature)
- [Rust v2 的 metadata confidence 與 canonical 合併](proposed/04-metadata-confidence-and-canonical.feature)
- [Rust v2 的本機 UI、縮圖與安全邊界](proposed/05-local-ui-thumbnails-and-security.feature)
- [Rust v2 的收藏儲存與可追溯資料](proposed/06-storage-and-audit.feature)
- [Rust v2 的收藏列表、搜尋與詳細資料](proposed/07-library-query.feature)
- [Rust v2 的手動 metadata 與 tags 寫入](proposed/08-manual-metadata-and-tags.feature)
- [Rust v2 的 metadata 候選與來源歷史](proposed/09-metadata-history.feature)
- [Rust v2 的 metadata assertion 人工裁決](proposed/10-metadata-assertion-decisions.feature)
- [Rust v2 的外部 metadata 搜尋工作](proposed/11-external-metadata-search-jobs.feature)
- [Rust v2 的 DLsite RJ 優先與書名搜尋 provider](proposed/12-dlsite-exact-rj-provider.feature)
- [Rust v2 的 tombstone 身分合併](proposed/13-tombstone-consolidation.feature)
- [Rust v2 的 E-Hentai／ExHentai gallery provider 與標籤映射](proposed/14-ehentai-gallery-provider.feature)

Proposed features 目前合計 145 個 scenarios；連同現況基線共 233 個 scenarios。

- [DEC-003：社團與作者](workshops/dec-003-circle-author-example-mapping.md)
- [批次 07：Rust v2 儲存模型](workshops/storage-v2-example-mapping.md)
- [批次 10：Tombstone 身分合併](workshops/tombstone-consolidation-example-mapping.md)

Parser 驗收語料：

- [Parser 黃金語料審閱指南](../parser-corpus/README.md)
- [第一批 parser corpus](../../tests/fixtures/parser-corpus-v1.json)

## 撰寫約定

- 使用標準英文 Gherkin keywords，敘述使用正體中文，方便未來直接接上 Cucumber 相容工具。
- BDD 只描述可觀察行為；SQLite、Flask、Axum 等實作選擇不寫進 scenario。
- 大量檔名案例未來放入獨立 parser corpus；feature files 只保留能說明規則或歧義的代表案例。
- 尚未存在但希望 Rust v2 新增的行為，不混入 `@current`；待現況基線確認後另建 `@proposed` feature set。

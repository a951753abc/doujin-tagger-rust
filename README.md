# Rust v2

本專案已與既有 Python 版本分離。Rust v2 位於 `L:\doujin-tagger-rust`；舊 Python 程式與 legacy catalog 保留在 `L:\doujin-tagger`，只在 migration rehearsal 或 shadow comparison 時以唯讀方式參照。

目前 Rust workspace 包含：

- `doujin-app`：application use cases；把 scanner、單一 writer repository 與檔案操作 service 組成可供未來 HTTP adapter 呼叫的同步邊界。
- `doujin-parser`：獨立 parser library 與同名 CLI。CLI 從標準輸入讀取單筆 `ParseInput` 或 JSON 陣列，輸出完整 `ParseResult`；不會讀寫收藏資料庫。
- `doujin-scanner`：新收藏掃描 library。它遞迴發現 ZIP、跳過既有完整路徑、排除系統／應用目錄、安全正規化新檔名，並產生待入庫資料。
- `doujin-storage`：SQLite v2 schema 與單一 writer repository，保存 metadata 來源、選擇、canonical mapping、位置歷史、tags 與檔案操作 journal。
- `doujin-thumbnails`：從 ZIP 或圖片資料夾安全選取自然排序第一張圖片，套用資源限制後產生 WebP cache。
- `doujin-files`：安全開啟／閱讀、搬移、軟刪除／永久刪除與 pending operation recovery；測試只使用臨時檔案、fake launcher 及 fake trash backend。
- `doujin-provider-dlsite`：RJ 優先、唯一完全相符書名 fallback 的 DLsite provider；成功匹配但沒有活動 option 時可提供 `DL` 場次候選，欄位解析、HTTP 錯誤分類與保守限速獨立於 application core。
- `doujin-http`：只允許 loopback listener 的 Axum HTTP adapter，內嵌無建置步驟的 Rust Library UI，並在獨立 blocking thread 執行 external search worker。
- `doujin-migrate`：依 DEC-037 將舊 catalog 的副本唯讀匯入全新 v2 catalog，並輸出 JSON 驗證報告。

## 執行

在 repository 根目錄使用 PowerShell：

```powershell
@'
{
  "filename": "[社團] 作品名稱 (ポケモン).zip",
  "parody_evidence": [
    {
      "raw": "ポケモン",
      "kind": "confirmed_alias",
      "canonical": "ポケットモンスター"
    }
  ]
}
'@ | cargo run --quiet -p doujin-parser
```

輸出：

```json
{
  "classification": {
    "top_level": "同人誌",
    "subcategory": null,
    "raw_marker": null
  },
  "event": null,
  "leading_bracket_raw": "社團",
  "circle": "社團",
  "authors": {
    "raw": null,
    "values": []
  },
  "title": "作品名稱",
  "parody": {
    "raw": "ポケモン",
    "canonical": "ポケットモンスター",
    "evidence": "confirmed_alias"
  },
  "identifiers": [],
  "other_info": [],
  "ignored_segments": [],
  "is_dl": false,
  "parse_status": "complete",
  "next_action": "none"
}
```

沒有原作證據時，`parody_evidence` 傳入空陣列；不確定的尾端括號會進入 `other_info`，不會自動成為原作。

大量檔名可以傳入 JSON 陣列；輸出會維持相同順序並回傳 `ParseResult` 陣列，供 shadow comparison 等批次工具使用。

Parser 對合法 percent-encoded 檔名只解碼一次後再做結構解析。新收藏掃描流程會呼叫 library 的 `normalize_new_collection_zip`：它只在解碼後能完整解析出場次、分類或創作者結構，且目標是安全、無衝突的同目錄 ZIP 檔名時才實際重新命名。衝突、不安全名稱、無法解析或檔案系統錯誤都不會覆寫目標；掃描器仍以原路徑產生待入庫資料，並附上正規化警告。

`doujin-scanner::scan_new_collections` 接受掃描來源與既有路徑集合，回傳 `PendingCollection`、逐項問題與摘要。它刻意不直接寫入索引，讓下一個 SQLite repository 切片能在單一 transaction 中決定如何提交。

`doujin-app::ApplicationService::run_scan` 會先建立 persistent scan run，再取得既有 current paths、執行 scanner，並逐筆呼叫 repository 入庫。單筆 constraint failure 不會回滾同批已成功收藏；整次結果標為 `partial`，問題與 JSON 摘要寫入 `scan_issues`／`scan_runs`。同一路徑再次掃描會跳過且不重新解析，database 同時只允許一筆 running scan。

完整掃描若確認 root 仍可讀、舊路徑已消失，且已有一筆或多筆實際存在的同檔名收藏，會將舊收藏轉為 tombstone 並建立待人工裁決關聯。舊 metadata／tags 不會複製到候選，候選也不會沿用舊 ID。整個 root 不存在或任一目錄／entry 讀取不完整時，不會在該 root 執行消失位置 reconciliation。沒有同名候選的消失收藏不屬於 DEC-008，此切片不推定刪除政策。圖片資料夾掃描仍未接入。

Application service 也統一轉呼叫安全 move、軟刪除／永久刪除與 pending file-operation recovery；HTTP、CLI 或桌面 adapter 不應自行繞過它操作檔案。高階歸檔操作只接收 collection IDs 與 archive root ID，目的地由 service 依 effective event 與既有 ZIP 檔名建立，不接受呼叫端提供完整來源或目的路徑。

## Localhost HTTP adapter

`doujin-http` 使用 Axum 0.8.9 與 Tokio 1.53.1。執行檔只接受 v2 catalog 路徑與可選 port，listen address 固定為 `127.0.0.1`；library 的 bind API 也會拒絕 `0.0.0.0`、區域網路或其他非 loopback 位址：

```powershell
cargo run --quiet -p doujin-http -- `
  .\doujin-v2.db 5000
```

啟動後以瀏覽器開啟 `http://127.0.0.1:5000/`。首頁、CSS 與 JavaScript 都編譯進 Rust 執行檔，不需要 Python template server、Node.js、CDN 或額外 frontend build。介面包含 Library 搜尋／組合篩選、列表／對比模式、分頁、收藏詳細資料、開啟／閱讀、tag、手動 metadata、外部資料搜尋、縮圖重建、依資料夾來源批次建立縮圖快取、統計、設定、來源管理與重新掃描。收藏詳情可漸進展開七個 metadata 欄位的 selection、assertions、confidence 與外部搜尋紀錄，並直接採用或拒絕可裁決 assertion。人工裁決工作台只保留目前頁面的選取，可批次加入 tag、覆寫原作／種類、搬移或刪除收藏，並提供同名候選裁決、合併預檢與逐欄衝突選擇。

列表／對比模式及最近開啟清單使用瀏覽器 `localStorage`。最近開啟只在 server 成功交給外部程式後更新，同一收藏移到最前方且最多保留 20 筆；不會寫入 SQLite，也不會在不同瀏覽器間同步。

舊 `doujin.db` 是未版本化的 Python catalog，不能直接交給此 server；必須先依 migration rehearsal／正式切換流程建立 v2 catalog。

第一批 API：

| Method | Path | 行為 |
|---|---|---|
| `GET` | `/` | 回傳 Rust 執行檔內嵌的本機 Library UI |
| `GET` | `/assets/app.css` | 回傳無外部字型或 CDN 依賴的 responsive 樣式 |
| `GET` | `/assets/app.js` | 回傳連接同源 API 的無框架 UI controller |
| `GET` | `/api/health` | 回報 adapter 存活與 API version |
| `GET` | `/api/settings` | 回傳有效閱讀器與縮圖設定，以及目前由環境變數鎖定的欄位 |
| `PUT` | `/api/settings` | 驗證並持久化閱讀器、縮圖尺寸與品質；設定變更會重排既有縮圖 |
| `GET` | `/api/stats` | 回傳 active 收藏總數、tagged 數、分類與常用 metadata 排行 |
| `GET` | `/api/collections` | 分頁列出 active 收藏；支援 `q`、`page`、`per_page` |
| `GET` | `/api/collections/{id}` | 回傳目前路徑、來源、effective metadata、tags 與時間戳記 |
| `POST` | `/api/collections/{id}/open` | 交由作業系統目前為 ZIP 設定的預設程式開啟收藏 |
| `POST` | `/api/collections/{id}/read` | 使用 application 啟動時設定的閱讀器開啟收藏 |
| `GET` | `/api/collections/{id}/thumbnail` | 回傳 WebP cache；尚未完成時排程工作並回傳不可快取的透明 placeholder，以及前端自動追蹤所需的 status／error／next-retry headers |
| `POST` | `/api/collections/{id}/thumbnail/rebuild` | 使單筆縮圖失效並重新排程，不修改收藏來源 |
| `POST` | `/api/thumbnails/rebuild` | 使全部 active 收藏縮圖失效、重新排程並回報數量 |
| `POST` | `/api/thumbnail-cache-jobs` | 依 `root_ids` 快照 active 收藏範圍，保留有效快取並優先補齊缺少或過期縮圖 |
| `GET` | `/api/thumbnail-cache-jobs/current` | 回傳最近一批快取工作的百分比、各狀態數量與預估剩餘秒數 |
| `GET` | `/api/collections/{id}/metadata` | 回傳各欄位 selection、assertions 與 external search results |
| `PUT` | `/api/collections/{id}/metadata/{field}` | 建立手動 metadata assertion 並回傳更新後收藏 |
| `DELETE` | `/api/collections/{id}/metadata/{field}` | 清除手動候選並重新套用來源優先序 |
| `PATCH` | `/api/collections/{id}/metadata/{field}/assertions/{assertion_id}` | 以 `select` 或 `reject` 人工裁決既有 assertion |
| `POST` | `/api/collections/{id}/external-search-jobs` | 建立或取得該 active 收藏目前的外部搜尋工作 |
| `GET` | `/api/external-search-jobs/{job_id}` | 查詢持久化外部搜尋工作狀態與結果 summary |
| `GET` | `/api/tombstone-candidates` | 列出 tombstone 與同名候選關聯、路徑及裁決狀態 |
| `PATCH` | `/api/tombstone-candidates/{tombstone_id}/{candidate_id}` | 以 `confirmed` 或 `rejected` 記錄人工裁決 |
| `GET` | `/api/tombstone-candidates/{tombstone_id}/{candidate_id}/preflight` | 列出 consolidation blockers 與逐欄手動值衝突 |
| `POST` | `/api/tombstone-candidates/{tombstone_id}/{candidate_id}/consolidate` | 以明確 conflict resolutions 執行可重試的身分合併 transaction |
| `POST` | `/api/collections/{id}/tags` | 以 `{"name":"..."}` 冪等加入單一 tag |
| `DELETE` | `/api/collections/{id}/tags` | 以 `{"name":"..."}` 移除 tag 並清理孤兒 tag |
| `GET` | `/api/library-roots` | 列出全部 library roots，包含停用項目 |
| `POST` | `/api/library-roots` | 以絕對且存在的資料夾路徑註冊來源；同一路徑會更新並重新啟用 |
| `DELETE` | `/api/library-roots/{id}` | 停用來源但保留設定與既有收藏資料 |
| `POST` | `/api/file-actions/move` | 將指定下載區收藏批次搬到啟用中的歸檔區，逐筆回報結果 |
| `POST` | `/api/file-actions/delete` | 以明確的 `soft` 或 `permanent` 模式批次刪除收藏 |
| `POST` | `/api/scans` | 只掃描 catalog 中已啟用的 library roots；request 不接受任意路徑 |
| `GET` | `/api/scans/{id}` | 回傳 persistent scan summary、狀態與 issues |

收藏列表預設每頁 50 筆，`per_page` 會限制在 1 到 200，無效頁碼回到第一頁。`q` 會搜尋檔名、標題、社團、作者與原作；輸入先轉成安全的 FTS terms，雙引號不會直接進入 `MATCH` 語法。列表固定依 collection ID 反向排序，未知排序參數會被忽略。

`GET /api/collections` 的 allowlisted filters：

| Parameter | 語意 |
|---|---|
| `event`、`circle`、`author`、`parody` | exact effective metadata；大小寫不敏感 |
| `classification`、`subcategory` | exact top-level classification 或子分類 |
| `source` | `archive` 或 `downloads` |
| `tag` | 可重複；結果必須具有全部指定 tags |
| `missing` | 可重複；支援 `title`、`event`、`circle`、`authors`、`parody`、`classification` |

不同 filters 之間採 AND。單值 filter 重複、空白值、未知 `source`／`missing` 會回傳 JSON 400；未支援的 query parameters 不會拼接進 SQL。

手動 metadata 的 request body 是 `{"value": ...}`。Allowlisted fields 與 value 型別如下：

| Field | Value |
|---|---|
| `title`、`event`、`circle` | 非空白 string |
| `authors` | 非空白 strings array |
| `parody` | string，或 `{"raw":"...","canonical":"..."}` |
| `classification` | string，或 `{"top_level":"...","subcategory":"..."}` |
| `is_dl` | boolean |

空白值不是「清除」；清除 manual assertion 必須使用 DELETE。清除後 repository 依 `manual > external > filename > inference` 重新選擇 effective value。Metadata 與 tags endpoints 只接受 active 收藏，且回傳更新後的 collection detail。

Metadata history 固定回傳七個欄位。每個欄位包含：

- `selection`：目前 assertion ID、`priority|manual|migration` 選擇方式與時間。
- `assertions`：typed JSON value、`manual|legacy|external|filename|inference` source、status、parser run、來源參照、confidence、理由、建立時間與 selected flag。
- `external_search_results`：外部搜尋 value、來源參照、confidence、`search_only|suggestion|auto_applied` disposition、可選 assertion ID 與時間。

低於候選門檻的 `search_only` 結果只出現在 external search results，不會偽裝成可選 assertion。此 endpoint 只允許讀取 active 收藏。

Assertion 裁決的 request body 是 `{"decision":"select"}` 或 `{"decision":"reject"}`。Assertion ID 必須屬於 URL 指定的 active 收藏與欄位；選取後 selection 標記為 `manual`，拒絕後 assertion 保留為 `rejected`。若拒絕目前 selection，repository 依既定來源優先序回復下一筆 accepted assertion；拒絕未選中的候選不改變 effective value。`rejected` 或 `obsolete` assertion 不可再次選取，重複拒絕已拒絕 assertion 則是無變更的成功操作。

建立外部搜尋工作的 request body 是 `{"fields":["title","circle"]}`，欄位採 metadata allowlist 並正規化為固定順序。同一收藏只能有一筆 pending／running 工作；重複要求回傳既有工作與 `created: false`。工作保存 `pending|running|succeeded|partial|failed` 狀態、嘗試次數、JSON summary、typed error 與 next retry time。

Application core 透過 `ExternalMetadataProvider` trait 接收 provider response，不依賴特定網站。Production binary 綁定 `doujin-provider-dlsite`：只在最新 parser result 恰有一個不同的 typed RJ 時查詢單一商品，不會由檔名重跑 parser，也不會以標題或社團模糊猜測。各欄位候選獨立保存，因此部分欄位失敗不會回滾已成功欄位。`network`、`rate_limited` 與 `provider_unavailable` 依錯誤種類及嘗試次數採指數退避，最長一天；`invalid_response`、`no_match`、`unsupported` 不安排定時重試。

HTTP server 啟動後每秒檢查一筆到期工作，再透過條件更新逐筆領取。取件與寫回共用單一 SQLite writer；DLsite 的限速等待及 blocking HTTP request 在 application mutex 外執行，不會長時間阻塞 API。Provider 對同一 host 保持單一 in-flight request 與至少 10 秒間隔。HTTP server 啟動時會將前一個程序留下的 running 工作回復為立即可執行的 pending，保留 attempts 並記錄 `worker_interrupted`。

Thumbnail worker 也只在領取與寫回時持有 application mutex；ZIP 解壓、圖片解碼、縮放與 WebP 編碼都在鎖外執行。首次要求會以 source／settings fingerprint 建立 persistent state；相同 pending／running 工作不會重複排程。來源 I/O、cache I/O 與 worker interruption 採最長一小時的指數退避，損壞 archive、無支援圖片、解碼錯誤與資源限制則等待來源變更或手動重建。Schema v5 只保存狀態、cache 路徑與 retry metadata，WebP 內容仍留在檔案系統。

Server 啟動後不會自動遍歷全庫預熱縮圖；worker 只處理畫面可見縮圖要求、手動批次、手動重建或其他已明確排入的工作。

設定頁的批次快取工具只接受已啟用的 library root ID。啟動時會固定這一批的 collection ID 範圍，把缺少或過期的工作提升為批次優先序（高於一般排程、低於畫面可見縮圖），並保留已有效的 WebP；同時間只允許一批。進度將 ready 與永久失敗都視為已處理，ETA 則只使用本批開始後新完成的數量估算，避免既有快取扭曲速度。

Library 對目前畫面使用的 collection ID 共用縮圖 tracker。收到 `pending`／`running` 會自動追蹤到 `ready` 並在不重新整理頁面的情況下替換封面；暫時性失敗依 `X-Thumbnail-Next-Retry-At` 恢復，永久性失敗停止自動要求。換頁或詳細資料改綁時會取消不再使用的 tracker，避免過時結果覆寫新收藏。

Tombstone candidate 裁決的 request body 是 `{"decision":"confirmed"}` 或 `{"decision":"rejected"}`。這個 endpoint 只記錄明確判斷並保留雙方身分；`confirmed` 不會自動執行 consolidation。

Consolidation preflight 要求指定 candidate 已 confirmed、同組其他 candidates 全部 rejected，而且雙方沒有 pending／running background jobs。不同手動選擇會逐欄回傳 assertion、來源與 typed JSON value。執行 request 使用 `{"resolutions":[{"field":"title","choice":"candidate"}]}`；每個衝突欄位都必須明確選擇 `tombstone` 或 `candidate`。

Schema v4 以 audit tables 保存 consolidation 與每筆轉入 record 的原 collection ID。成功後由 tombstone ID 恢復為 active survivor，接管 candidate current location；tags 採聯集，parser runs、metadata assertions、external search results、位置與檔案操作歷史均保留，effective metadata／FTS 在同一 transaction 重建。實體 ZIP 不搬移。Merged candidate 查詢回 HTTP 410、`collection_merged` 與 `merged_into_collection_id`；相同 consolidation request 重送會回傳既有結果。

檔案搬移 request 使用 `{"collection_ids":[1,2],"archive_root_id":3}`。每筆收藏只能由 downloads root 搬到指定的 active archive root；service 將空白場次放入「未分類」，替換 Windows 不允許的字元並避開保留名稱，且不覆寫同名 ZIP。HTTP request 沒有來源路徑或目的路徑欄位，額外欄位會被拒絕。

開啟與閱讀 endpoints 只接受 URL 中的 collection ID。兩者都會重新驗證收藏是 active、current path 位於啟用中的已註冊來源、實體檔案存在，且目標是一般 ZIP 而非 symlink。`open` 使用作業系統預設 handler；`read` 只使用 application 已設定的絕對閱讀器路徑，HTTP request 不能指定任意 executable。閱讀器可由 `PUT /api/settings` 持久化；未設定或路徑不是現存的一般檔案時不啟動外部程式。

```powershell
$env:DOUJIN_READER_PATH = 'C:\Program Files\Honeyview\Honeyview.exe'
cargo run --quiet -p doujin-http -- `
  .\doujin-v2.db 5000
```

Thumbnail 預設為 `300x400`、WebP 品質 80，cache 目錄是 catalog 旁的 `<catalog filename>.thumbnails`。可在啟動前覆寫；`DOUJIN_THUMB_DIR` 必須是絕對路徑：

```powershell
$env:DOUJIN_THUMB_DIR = 'D:\doujin-cache\thumbnails'
$env:DOUJIN_THUMB_SIZE = '360x480'
$env:DOUJIN_THUMB_QUALITY = '85'
```

啟動設定依 `環境變數 > SQLite application_settings > config.json > 預設值` 合併。`config.json` 預設從目前工作目錄讀取，也可用 `DOUJIN_CONFIG_PATH` 指定；其中的相對 `viewer_path`／`thumb_dir` 以設定檔所在目錄解析。支援的檔案欄位是 `viewer_path`、`thumb_dir`、`thumb_size`、`thumb_quality`。環境變數覆寫中的欄位會出現在 `GET /api/settings` 的 `environment_overrides`，執行期間不會被 PUT 越過。

Schema v6 以 typed singleton row 保存 reader path、thumbnail width／height／quality。設定寫入與舊 fingerprint 縮圖重排程在同一 SQLite transaction 完成；無效尺寸、品質、相對 reader path 或未知 JSON 欄位不會部分寫入。

`GET /api/stats` 只統計具有 current location 的 active 收藏。分類包含「未分類」bucket；作者由 `authors_json` 逐人計數，原作、作者與社團各取前 20，場次取前 30，並以 count 反向、名稱正向穩定排序。

刪除 request 使用 `{"collection_ids":[1,2],"mode":"soft"}` 或 `mode: "permanent"`。兩個 endpoints 都拒絕空白、重複或非正整數 collection IDs，並以 `succeeded`、`failed`、`pending_recovery` 統計及逐筆 item 回報；單筆失敗不回滾其他成功項目。所有破壞性操作會再次驗證 current path 位於啟用中的已註冊來源內，相似字首不算子路徑。Production server 啟動時會先核對 pending file operations：能確定已套用或未套用者完成 reconciliation，狀態不明確者保持 pending recovery。

所有請求的 HTTP Host 必須是 `localhost` 或實際 loopback IP，以阻止 DNS rebinding；POST／PUT／PATCH／DELETE 如果帶有 Origin 或 Referer，其 host 也必須符合相同規則。相似字首如 `localhost.evil.example` 會被拒絕，沒有來源標頭的本機 CLI 寫入仍可使用。所有 domain、404 與 405 錯誤都使用 `{"error":{"code":"...","message":"..."}}` JSON 格式。

UI document 另外送出 `default-src 'none'` 的 Content Security Policy，只允許同源 script、style、image 與 API connection，並加上 `nosniff`、`no-referrer` 與 `frame-ancestors 'none'`。介面使用語意化 landmarks、可見 focus、鍵盤 `/`、`J`、`K`、`?`、44px 觸控目標與 `prefers-reduced-motion`；窄螢幕重新排列為單欄且不產生水平溢位。

## 唯讀 migration 演練

先停止 Python 版本，確認 `doujin.db-wal`／`doujin.db-shm` 不存在，再複製舊 catalog。只把副本交給 runner，target 必須是尚不存在的新檔：

```powershell
Copy-Item ..\doujin-tagger\doujin.db .\doujin-rehearsal-source.db
cargo run --quiet -p doujin-migrate -- `
  .\doujin-rehearsal-source.db .\doujin-v2-rehearsal.db
```

Runner 以 `mode=ro&immutable=1`、`SQLITE_OPEN_READ_ONLY` 與 `query_only` 開啟來源；來源旁若已有 WAL／SHM，或 target／target WAL／SHM 任一檔案已存在，就會在寫入前拒絕執行。匯入以單一 transaction 提交，stdout JSON 報告包含收藏、位置、metadata、tags、空值、路徑衝突、均勻 metadata 抽樣、foreign-key check、integrity check，以及來源檔匯入前後的 BLAKE3。

這是演練工具，不會切換 Python 版本，也不應直接以唯一的正式 `doujin.db` 作為第一次 migration 來源。

正式資料的可重複演練可使用 repository 根目錄下的安全包裝腳本。輸出目錄必須尚不存在；腳本會先拒絕帶有 WAL／SHM／journal 的來源，取得不允許其他程序寫入的來源唯讀鎖，在同一鎖內核對 SHA-256 並複製，再讓 runner 只讀取靜態副本：

```powershell
.\tools\run_migration_rehearsal.ps1 `
  -SourceCatalog ..\doujin-tagger\doujin.db `
  -OutputDirectory .\target\formal-rehearsal-YYYYMMDD
```

成功時輸出目錄保留 `doujin-v2-rehearsal.db`、`migration-report.json` 與 `acceptance-gate.json`；靜態舊 catalog 副本及空的 target WAL／SHM 會自動移除。可用 `-TargetFileName doujin-v2.db` 指定單一 `.db` leaf name，或用 `-KeepSourceCopy` 明確要求保留副本。任何 hash、runner status、path conflict、blocking issue、integrity、foreign-key、數量、tag、metadata 抽樣或空值比較失敗都使驗收閘門失敗；既有輸出目錄不會被覆寫。

正式切換前再以 `tools/test_cutover_readiness.ps1` 核對來源／candidate SHA-256、reports、即時 current path audit、legacy 設定、程序停止狀態及正式 port。完整 Go／No-Go 與 rollback 流程見 [`docs/references/formal-cutover-and-rollback-runbook.md`](docs/references/formal-cutover-and-rollback-runbook.md)。Preflight 回報 `ready` 只代表技術條件成立，不會自行啟動 server 或授權切換。

## 驗證

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

黃金語料位於 [`tests/fixtures/parser-corpus-v1.json`](tests/fixtures/parser-corpus-v1.json)。

## 與既有收藏唯讀比較

先建置 CLI，再從 repository 根目錄執行：

```powershell
cargo build -p doujin-parser
python tools/shadow_compare.py --output docs/parser-corpus/shadow-comparison-v1.md
```

比較工具以 SQLite 唯讀 immutable 模式讀取相鄰 Python 專案的 `doujin.db`，不會回寫既有收藏；詳細結果與待確認案例位於 [`docs/parser-corpus`](docs/parser-corpus/README.md)。

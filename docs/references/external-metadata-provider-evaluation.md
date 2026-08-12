# 外部 metadata provider 選型

最後查核：2026-08-12

## 結論

Rust provider 採用 **DLsite RJ 優先、書名唯一完全匹配 fallback**：

- 只在最新 parser 證據恰好包含一個不同的 RJ 識別碼時送出請求。
- 使用 `https://www.dlsite.com/maniax/api/=/product.json?workno={RJ}` 的單次結構化回應。
- 第一階段只產生可由回應直接證明的 title、circle、authors、event，以及明確標記為 original work 的 parody 候選。
- 不以一般 genres 猜原作，不以 `work_type` 或網站分區猜本專案的 classification，也不因商品存在於 DLsite 就推斷本機檔案的 `is_dl`。
- 沒有 RJ 但有辨識書名時可查詢搜尋頁；只有正規化後唯一完全相符的標題才繼續查產品，多筆同名不得依排名選擇。
- 書名匹配不具有可靠識別碼資格，所得候選只能待確認或留作搜尋紀錄，不得自動套用。
- 舊版 Google 結果頁 scraping 不移植。

這個順序最符合已確認的 DEC-014／DEC-015：精確產品識別碼能形成可解釋的高信心證據，模糊字串搜尋則不能單獨取得自動套用資格。

## 舊版行為與風險

目前 `web_enrich.py` 混合了三種不同責任：

1. 以 RJ 或社團＋標題搜尋 DLsite，並解析 HTML selector。
2. 抓取 Google 搜尋結果頁，將第一個括號內容推測為原作。
3. 依 CG 關鍵字或同社團既有資料做本地推斷。

主要問題不是 Python 本身，而是證據界線不清：

- DLsite genre 經過排除清單後，剩下的第一個值可能被誤當原作。
- Google 結果頁 DOM 不是 metadata contract，且括號內容沒有固定領域語意。
- 模糊搜尋直接檢查前兩筆，容易把「搜尋排名」誤當「實體匹配」。
- 網路來源與本地推斷共用同一層，使 confidence 理由不容易追查。

Rust v2 應把這三者拆成 exact provider、未來的 fuzzy provider，以及 inference engine。

## GitHub 實作比較

| 專案 | 可借鏡內容 | 不直接採用的原因 | 授權 |
|---|---|---|---|
| [dlsite-rs](https://github.com/ozonezone/dlsite-rs) | Rust client；同時研究 HTML、AJAX 與 `/api/=/product.json?workno=...`；crate 提供精確產品 lookup 與搜尋 | README 明示仍為 WIP，且 DLsite JSON 規格可能造成 breaking changes；完整 response model 遠大於本專案需求，現有 async API 也不符合目前同步 worker 邊界 | [MIT](https://github.com/ozonezone/dlsite-rs/blob/master/Cargo.toml) |
| [dlsite-async](https://github.com/bhrevol/dlsite-async) | 維護活躍；先取 JSON 核心資料，再解析商品頁補 circle、author、event、genre；欄位與錯誤測試可作行為參考 | Python library，不作為 Rust runtime dependency；其 HTML 補資料路徑比單次 product JSON request 多一次請求 | [MIT](https://github.com/bhrevol/dlsite-async/blob/main/LICENSE) |
| [Playnite DLsite metadata provider](https://github.com/erri120/Playnite.Extensions) | 支援 RJ／URL 直接匹配、locale、搜尋結果數量與背景模式，可參考設定與 UX | 背景模式會直接採用搜尋第一筆，與本專案「無可靠識別碼不得自動套用」衝突；GPL 程式碼只作行為研究 | GPL-3.0 |
| [dlsite-manager](https://github.com/AcrylicShrimp/dlsite-manager) | Rust／Tauri、SQLite、背景 jobs、audit log 與 DLsite domain crate 分層 | 主要範圍是登入、同步已購作品與下載；第一個公開 metadata provider 不需要帳號、憑證或 DLsite Play v3 | MIT |

採用界線：可以參考公開 request shape、response 欄位與測試策略；本專案自行撰寫最小 adapter，不複製第三方 parser 或完整資料模型。

## 已驗證的介面

2026-08-12 以公開樣本 `RJ294126` 做唯讀驗證：

```text
GET https://www.dlsite.com/maniax/api/=/product.json?workno=RJ294126
```

回應為單一元素陣列，包含：

- `workno`／`product_id`
- `work_name`
- `maker_id`／`maker_name`
- `site_id`
- `author`／`authors`（作品類型不同時可能是 null）
- `creaters.created_by` 等具角色 creators
- `work_options`
- `genres`

這個 endpoint 不是已找到的官方穩定 public API。`dlsite-rs` 也明確警告 JSON 規格可能改變，因此 adapter 必須：

- 只 decode 需要的最小欄位並忽略未知欄位。
- 逐欄位驗證型別，單一 optional 欄位壞掉時仍允許其他欄位部分成功。
- 要求回傳 `workno` 或 `product_id` 與 request RJ 完全一致。
- 把商品頁 URL，而不是內部 API URL，保存為 `source_reference`。
- 以 fixture 測試 schema drift、空陣列、ID 不一致與欄位部分損壞。

DLsite 目前的 [robots.txt](https://www.dlsite.com/robots.txt) 對一般 user agent 宣告 `Crawl-delay: 10`；[llms.txt](https://www.dlsite.com/llms.txt) 另列出每秒 2 次、每分鐘 120 次與 retry 指引。第一版採較保守的 host-wide 最短 10 秒間隔、單一 in-flight request，並沿用既有 background job backoff，避免密集重試。

## 第一階段欄位映射

| Rust 欄位 | DLsite 證據 | 決策 |
|---|---|---|
| `title` | `work_name` | 接受；非空且 exact RJ response ID 相同才建立候選 |
| `circle` | `maker_name` | 接受；保存 `maker_id` 於理由或 provider evidence，不把 publisher／brand 混入 circle |
| `authors` | `author`／`authors` 的 `author_name`；沒有時才考慮 `creaters.created_by.name` | 接受，但必須去空白、去重並保留順序；voice、illustration、scenario roles 不得混入 authors |
| `event` | `work_options` 中明確表示活動的 structured option | 接受；一般 option 不得當成場次 |
| `parody` | 明確的 `ORW`／Original Work option | 只建立 canonical `オリジナル` 候選；一般 genres 不得猜原作 |
| `classification` | `site_id`、`work_category`、`work_type` | 第一階段不映射；它們和本專案「同人誌／CG／商業誌子分類」不是一對一關係 |
| `is_dl` | 商品存在於 DLsite | 不映射；遠端銷售形式不能單獨證明本機 ZIP 的來源或版本 |

provider 只回傳工作 request 中要求的欄位。欄位不存在不是錯誤；欄位存在但型別或語意不合法時，才回傳該欄位的 typed issue。

## Confidence 與錯誤分類

精確 lookup 的必要條件：

- parser evidence 中只有一個不同的 RJ。
- response `workno`／`product_id` 與該 RJ 完全相同。
- 候選值通過欄位型別與非空驗證。

符合條件的候選設定 `reliable_identifier_exact_match = true`，總信心度至少 0.95；理由應明列 RJ、provider 欄位與 exact match。是否能套用仍由 repository 檢查手動值衝突與來源優先序，不由 provider 越權決定。

錯誤分類：

| 狀況 | `ExternalSearchErrorKind` | 重試 |
|---|---|---|
| timeout、DNS、連線中斷 | `network` | 依現有 backoff |
| HTTP 429 | `rate_limited` | 依現有 rate-limit backoff |
| HTTP 5xx | `provider_unavailable` | 依現有 backoff |
| 空陣列、404、找不到相同 ID | `no_match` | 否 |
| response 根結構無法解讀 | `invalid_response` | 否；保留摘要供修 adapter |
| 沒有 RJ 且沒有辨識書名，或 RJ 有歧義 | `unsupported` | 否；不送 HTTP request |
| 書名搜尋零筆或多筆完全相符 | `no_match` | 否；不依搜尋排名選擇 |

## 批次 08 實作狀態

批次 08 已完成第一條 production exact lookup 資料流：

1. Storage 從最新 `parser_runs.result_json` 解碼 typed identifiers，application 將它們放入 `ExternalSearchRequest`。
2. DLsite adapter 不重跑 filename parser，也不直接查 SQLite。
3. `doujin-provider-dlsite` 使用 blocking HTTP client；production worker 在獨立 thread 執行 request，且不在網路等待期間持有 application mutex。
4. 一般 test suite 只使用注入的 fake transport 與固定 JSON fixture，不連線至正式 DLsite。
5. Production provider 採單一 in-flight request 與至少 10 秒 host-wide 間隔；HTTP server 每秒檢查一筆到期工作。

2026-08-13 依 DEC-040 加入 title fallback：沒有 RJ 時以辨識書名搜尋；DLsite 對含副標題分隔符的完整長標題可能回傳無關結果，因此查詢字串可縮短為分隔符前至少四字的穩定核心，但結果仍只接受唯一的完整書名正規化完全相符作品，再以其 RJ 取得產品 JSON。此路徑的 confidence 不標記可靠識別碼完全匹配，因此只能產生 suggestion 或 search-only。

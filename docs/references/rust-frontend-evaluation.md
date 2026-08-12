# Rust v2 本機前端實作參考

本文件記錄批次 15 將既有 Library 核心流程接到 Rust localhost server 的方式，並追溯 `browse-001`～`browse-003`、`file-005`、DEC-017 與 `recent-v2-001`。

## 交付邊界

前端是 `doujin-http` crate 內的靜態 HTML、CSS 與 JavaScript，由 `include_str!` 編譯進執行檔：

- `GET /`：Library document
- `GET /assets/app.css`：responsive、暖紙張編目室樣式
- `GET /assets/app.js`：同源 API controller

不覆寫既有 Python `templates/`／`static/`，也不依賴 Node.js、frontend framework、網路字型或 CDN。Rust 版與 Python 版因此可在遷移驗證期間並存。

## 已接上的使用流程

- Library 分頁、全文搜尋與來源、種類、缺少欄位、場次、社團、作者、原作、子分類、tag 組合篩選。
- 列表／對比模式會保存在目前瀏覽器，再次開啟沿用上次選擇。
- 收藏詳細資料、縮圖、系統開啟、指定閱讀器、tag 新增／移除、手動 metadata 寫入／清除、外部搜尋排程與縮圖重建。
- 詳細資料中的場次、社團、作者、原作與種類可直接加入篩選；統計排行也可返回對應 Library 結果。
- 收藏統計、閱讀器／縮圖設定、來源登記／停用與重新掃描。
- 載入、空結果、API 失敗、部分掃描成功、環境覆寫與背景排程均以文字呈現，不只依賴顏色。

破壞性批次搬移／刪除、tombstone 候選裁決與 consolidation UI 尚未接入本批；底層 API 與安全驗證已存在，應在後續人工裁決工作台中集中呈現，避免將高風險操作塞進一般瀏覽明細。

## 最近開啟與 DEC-017

最近開啟使用 `doujin-library.recent.v1` localStorage key：

1. `open` 或 `read` API 成功回覆後才寫入。
2. 同一 collection ID 去重並移到最前方。
3. 最多保存 20 筆，內容只有 ID、顯示標題、檔名、動作與本機時間。
4. 不提供 server endpoint，也不進 SQLite；不同瀏覽器或裝置不會共享。
5. 使用者可單獨清除這份瀏覽器紀錄，不影響收藏或檔案。

## 視覺與無障礙方向

依 `.impeccable.md` 採「沉著、編輯感、俐落」的暖紙張私人圖書館方向。桌面使用固定導覽、結果與詳細資料編目桌；手機重新排為單欄。所有字型使用本機 fallback，不以封面色彩改寫工具本身的視覺秩序。

- `zh-Hant` document language、skip link、landmarks、具名 form controls 與 native dialog。
- 16px 基準字級、可見 `:focus-visible`、WCAG AA 方向的墨色／朱紅對比。
- 互動目標至少 44px；只使用 opacity／transform 短動效，並尊重 reduced motion。
- 以 `textContent` 與 DOM node 建立收藏內容，不把 metadata 拼成 HTML。
- CSP 禁止外部與 inline script，並限制同源 API、style 與 image。

## 驗證

HTTP integration test 透過真實 loopback socket 驗證首頁與 assets 的 MIME、cache policy、CSP、`nosniff`、無外部 URL、responsive／reduced-motion 基線，以及未知 asset 維持 structured 404。HTML 使用 `no-store`，CSS／JavaScript 使用批次版本 query 與 `no-cache` 重新驗證，避免換新版執行檔後仍沿用上一版 UI。

本機瀏覽器以臨時 catalog 與六筆臨時 ZIP 驗證：

- 桌面 Library 載入、搜尋「午後」、再以「商業誌」交叉篩選。
- tag 寫入、metadata editor、統計與設定／來源頁。
- 390px 寬窄螢幕重排；document `scrollWidth` 等於 `clientWidth`，沒有水平溢位。
- 瀏覽器 console 沒有 error 或 warning。

測試資料庫位於 Rust build target 下；正式 `doujin.db` 未被 server 或測試開啟。

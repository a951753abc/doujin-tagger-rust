# Metadata 證據與 assertion 裁決面板實作參考

本文件記錄批次 17 如何把 `metadata-v2-020`～`metadata-v2-030` 與 `external-search-v2-001`～`external-search-v2-010` 的既有 HTTP API 接到 Library 收藏詳情。介面不建立第二套 metadata 規則；effective value、selection 與 assertion 狀態仍完全由 Rust application／SQLite 決定。

## 漸進揭露

收藏詳情預設只顯示 effective metadata。使用者展開「證據與裁決」後才請求 `GET /api/collections/{id}/metadata`，避免瀏覽與鍵盤切換收藏時替每筆資料額外載入完整歷史。

面板固定依下列順序呈現七個欄位：

1. title
2. event
3. circle
4. authors
5. parody
6. classification
7. is_dl

每個欄位摘要顯示目前值、assertion 來源、selection 方式與候選數。具有 `candidate` 的欄位會自動展開；使用者手動展開的欄位在同一收藏重新渲染後保持展開。

## Assertion 證據

每筆 assertion 顯示：

- typed value 與 assertion ID
- `manual`、`legacy`、`external`、`filename`、`inference` 來源
- `candidate`、`accepted`、`rejected`、`obsolete` 狀態
- selected 標記、建立時間、parser run 與來源參照
- 原始理由與 confidence total
- 來源可靠度、識別碼匹配、字串相似度、規則確定度及 confidence 理由

External search results 另列於欄位底部。`suggestion`／`auto_applied` 會指出對應 assertion ID；`search_only` 明示只供追查，沒有可執行的採用按鈕。

## 人工裁決

「採用這個值」呼叫 assertion decision endpoint 的 `select`，並重新載入收藏摘要，使 effective value 與 Library 畫面同步。

拒絕是不可重新選取的 metadata 決定，因此不直接送出。第一次操作會在原 assertion 列中展開警告；只有按下「確認拒絕候選」或「拒絕並改用下一順位」才送出 `reject`。成功後：

- assertion 原值、來源、confidence、理由與時間繼續保留。
- 拒絕未選中候選不改變 effective value。
- 拒絕目前 selection 時由 repository 依 `手動修改 > 外部 metadata > 檔名解析 > 推斷結果` 回退。
- `rejected`／`obsolete` 不再提供採用或拒絕操作。

## 外部搜尋工作狀態

排入搜尋後，證據面板會自動展開並顯示 job ID、欄位、狀態、嘗試次數、結果計數、逐欄 issues、typed error 與下次重試時間。`pending`／`running` 在面板展開期間輪詢；若有未來的 `next_retry_at`，輪詢最長降至每 60 秒一次。進入 `succeeded`、`partial` 或 `failed` 後停止輪詢，並重新載入收藏摘要與 metadata history。

工作本身與結果只保存在 SQLite。瀏覽器 `localStorage` 僅記錄每筆收藏最近一次已知 job ID，最多 200 筆，供重新整理頁面後呼叫持久化 job endpoint；不複製狀態或搜尋結果，也不影響 worker。

## 驗證

隔離 catalog 的瀏覽器驗證包含：

- 同一欄位並列 manual 與 filename assertions。
- 採用 filename assertion 後 effective title 立即更新。
- 拒絕目前 filename assertion 前顯示行內二次確認；確認後 evidence 保留並回退 manual title。
- 缺少 typed RJ 的外部工作明確顯示 `failed`／`unsupported`，不偽裝成零結果。
- 390px viewport 無水平 overflow，七欄摘要、裁決與 job 狀態仍可到達。
- 瀏覽器 console 無錯誤。

所有互動測試只使用 Rust 專案 `target` 下的一次性 catalog 與 ZIP，正式 `doujin.db` 不會交給 Rust server。

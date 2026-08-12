# Rust v2 縮圖 pipeline 實作參考

本文件記錄批次 13 對 `thumbnail-001`～`thumbnail-005`、DEC-018、DEC-021 與 `thumbnail-v2-001`～`thumbnail-v2-005` 的落地方式。

## 儲存邊界

- SQLite schema v5 的 `thumbnail_states` 是工作與 retry 狀態的權威來源；每筆 active 收藏最多一列。
- WebP 是可以重建的衍生內容，保存在 catalog 外的檔案系統 cache。SQLite 只保存 cache 路徑、source／settings fingerprint、狀態、錯誤與產出尺寸。
- Production 預設 cache 是 catalog 旁的 `<catalog filename>.thumbnails`，避免不同 catalog 在同一目錄共用 collection ID 命名空間。
- 手動重建只刪除由設定推導的 `{collection_id}.webp`，不接受 HTTP 呼叫端提供任意 cache 或收藏路徑，也不修改 ZIP。

## 來源選擇與資源界線

- Generator 接受 ZIP 或圖片資料夾，支援 JPEG、PNG、WebP、GIF、BMP。
- 候選先排除 directory entry、symlink 與 `__MACOSX`，再依相對路徑自然排序；例如 `page2.png` 早於 `page10.png`。
- ZIP entry 解壓後上限 100 MiB；圖片寬高各上限 20,000 像素，decoder allocation 上限 256 MiB。輸出尺寸設定限制為 1～4096 像素，WebP quality 為 1～100。
- 圖片以 Lanczos3 保持比例縮放到設定邊界內，先寫同目錄唯一 `.part`、flush 與 sync，再 publish 成 `{collection_id}.webp`。失敗的暫存檔會清理。

## 排程與失敗分類

首次 GET 若 cache 尚未 ready，repository 建立 `pending` state；相同收藏的後續要求讀回既有 state，不建立第二個工作。HTTP 立即回傳 `202`、`image/webp`、`Cache-Control: no-store` 與透明 placeholder。Ready cache 回傳 `200` 與 `Cache-Control: private, max-age=86400`。

HTTP response 另以 `X-Thumbnail-Status` 回報 `pending|running|ready|failed`。失敗時附上 `X-Thumbnail-Error-Kind`；暫時性失敗另附 `X-Thumbnail-Next-Retry-At`，讓前端可以遵守同一份 retry state，而不是自行猜測錯誤種類。

| Error kind | 分類 | 行為 |
|---|---|---|
| `source_io`、`worker_interrupted` | 暫時性 | 30 秒起算指數退避 |
| `cache_io` | 暫時性 | 60 秒起算指數退避 |
| `invalid_archive`、`no_supported_image`、`image_decode` | 永久性 | 不定時重試 |
| `resource_limit`、`unsupported` | 永久性 | 不定時重試 |

退避會隨已開始的 attempts 增加，最長一小時；到期前不能領取。永久性錯誤只有 source fingerprint 改變或收藏管理者呼叫 rebuild 才會回到 `pending`。這是 DEC-018 對舊 `thumbnail-004`「所有失敗都不自動重試」的細分結果。

## 設定變更與 crash recovery

啟動時先把遺留的 `running` state 改回可立即領取的 `pending`，記錄 `worker_interrupted` 且不額外增加 attempts。接著只檢查已存在縮圖 state 的收藏；settings fingerprint 改變時自動重新排程，不因服務啟動就為所有尚未要求過的收藏預先建圖。

Background worker 在 application mutex 內只領取與寫回 state；ZIP 讀取、圖片解碼、縮放與 WebP 編碼在鎖外完成，避免慢圖片阻塞 localhost API。

## 前端自動更新

Library 以 collection ID 去重可見縮圖的追蹤工作。`pending`／`running` 採有上限且加入 jitter 的輪詢；`ready` 後使用 cache-busted URL 同步更新清單與詳細資料並停止追蹤。暫時性 `failed` 等到 `X-Thumbnail-Next-Retry-At` 才恢復要求，永久性 `failed` 停止自動要求並保留詳細資料的手動重建入口。

換頁、重新搜尋、離開 view 或詳細資料改綁其他收藏時，對應元素會解除舊 tracker；已取消或過時的 response 不得寫回新畫面。這補足 DEC-039 與 `thumbnail-v2-006`～`thumbnail-v2-009`。

## HTTP surface

| Method | Path | 結果 |
|---|---|---|
| `GET` | `/api/collections/{id}/thumbnail` | Ready WebP 或透明 placeholder；首次要求同時排程 |
| `POST` | `/api/collections/{id}/thumbnail/rebuild` | 刪除單筆 cache、清除失敗狀態並排回 `pending` |
| `POST` | `/api/thumbnails/rebuild` | 對全部 active 收藏執行相同行為並回報 `rebuilt` 數量 |

測試只建立暫存 SQLite、ZIP／圖片資料夾與 cache；不開啟或修改正式 catalog 與收藏檔案。

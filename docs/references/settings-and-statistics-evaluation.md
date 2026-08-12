# Rust v2 設定與收藏統計實作參考

本文件記錄批次 14 對 `settings-002`、`settings-003`、DEC-021 與 `stats-001` 的實作方式。掃描來源的新增、停用、重新啟用與驗證已由既有 `library_roots` API 完成，不重複建立第二套 `scan_roots` JSON 設定。

## 設定資料模型

Schema v6 使用只有 `singleton = 1` 的 STRICT `application_settings` table：

- `reader_path` 可為空，但非空值必須由 application 驗證為絕對路徑。
- `thumbnail_width`、`thumbnail_height` 各限制為 1～4096。
- `thumbnail_quality` 限制為 1～100。
- 整組設定以一個 upsert transaction 保存；沒有任意 key/value extension point。

`PUT /api/settings` 沿用現有 UI 可直接使用的 `viewer_path`、`thumb_size`、`thumb_quality` 欄位，但 request 採 `deny_unknown_fields`。尺寸只接受 `WIDTHxHEIGHT`；`300*400`、零品質、超出範圍、相對 reader path 或多餘欄位都在 transaction 前回傳 JSON 400。

## 啟動優先序

有效設定逐欄依下列順序選擇：

1. `DOUJIN_READER_PATH`、`DOUJIN_THUMB_SIZE`、`DOUJIN_THUMB_QUALITY`、`DOUJIN_THUMB_DIR`
2. SQLite `application_settings` 中由使用者保存的值
3. `config.json` 的 `viewer_path`、`thumb_size`、`thumb_quality`、`thumb_dir`
4. 預設 thumbnail `300x400`、quality 80，以及 catalog 旁的 `<catalog filename>.thumbnails`

`config.json` 預設位於目前工作目錄；`DOUJIN_CONFIG_PATH` 可指定另一個檔案。檔案內相對 reader/cache path 以 config 所在目錄解析。環境變數存在時，設定 API 仍保存使用者值供未來沒有 override 的啟動使用，但目前程序維持環境值，並在 response 的 `environment_overrides` 明示來源。

## 縮圖設定變更

Application 先驗證要求值與套用環境 override 後的有效值，再把 user settings 與 thumbnail state 重排程放進同一 SQLite transaction。所有 `settings_fingerprint` 不同的既有 state 會：

- 改回 `pending`
- 清除 typed error、失敗時間、next retry 與輸出尺寸
- attempts 歸零
- 保留來源 ZIP 與舊 WebP，直到 worker 成功 publish 新 cache

因此符合 DEC-021，又不會在新縮圖成功前破壞收藏來源。

## 統計 read model

`GET /api/stats` 只計算具有 current location 的 active 收藏；tombstone、soft-deleted 與 merged identity 不會混入 Library Dashboard。

- `total`：active Library 收藏數
- `tagged`：至少有一個 tag 的 active 收藏數
- `categories`：effective top-level classification；空值歸入「未分類」
- `top_parody`、`top_author`、`top_circle`：前 20 名
- `top_event`：前 30 名

作者直接展開 `effective_metadata.authors_json`，所以 `甲、乙` 會分別增加甲與乙的使用次數。排行先依 count 反向，再依名稱不分 ASCII 大小寫排序，使相同資料得到穩定輸出。

所有設定、migration、統計與 HTTP 測試只使用 in-memory 或暫存 SQLite 與暫存收藏檔案，不開啟正式 catalog。

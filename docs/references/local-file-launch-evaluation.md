# Rust v2 本機收藏開啟方式

本文件記錄批次 12 對 DEC-020、`file-001`～`file-004`、`file-v2-007`、`boundary-001` 與 `boundary-002` 的實作選擇。

## Adapter 邊界

- `POST /api/collections/{id}/open` 使用作業系統目前為 ZIP 設定的預設 handler。
- `POST /api/collections/{id}/read` 使用 application 啟動時已設定的閱讀器。
- HTTP request 只提供 collection ID，不接受收藏路徑、閱讀器路徑、command 或 arguments。
- 最近開啟清單依 DEC-017 保持為瀏覽器本機狀態；server 成功回覆只表示已將要求交給 launcher，不保存跨瀏覽器紀錄。

## 路徑與程序安全

1. Repository 先確認收藏是 active，具有 current location，且 root 仍啟用。
2. Current path 的正規化 component 必須位於已註冊 root 下；canonical parent 也不能透過 symlink 逃出 root。
3. 實體收藏必須存在、是一般 ZIP 檔案且不能是 symlink。
4. 指定閱讀器必須是已設定的絕對路徑、存在、是一般檔案且不能是 symlink。
5. Launcher 失敗會轉為結構化 API error，不會把未成功交付的要求回報為 launched。

Production 的系統預設 handler 使用 `open 5.4.1`，Windows 啟用 `shellexecute-on-windows`，不啟用該 crate 明確標示為不安全相容用途的 `insecure` feature。指定閱讀器也由同一 adapter 以獨立 application 與收藏 path 呼叫，不經 shell 字串拼接。

## 設定過渡

在完整 settings API 批次完成前，production 由 `DOUJIN_READER_PATH` 注入可選閱讀器路徑。這個過渡只改變設定來源，不改變 application service 與 HTTP 的安全邊界；後續持久化設定不得讓單次 read request 直接指定 executable。

## 測試隔離

HTTP 與 application 測試注入 recording launcher，只記錄預期的 action、reader 與 collection path。測試不呼叫作業系統 handler、不啟動真實閱讀器，也不開啟正式收藏。

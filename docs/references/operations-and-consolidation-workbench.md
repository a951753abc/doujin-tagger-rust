# 人工裁決與高風險操作工作台實作參考

本文件記錄批次 16 如何把 `batch-001`～`batch-003`、檔案操作 v2 與 tombstone consolidation BDD 接到 localhost UI。工作台只呼叫既有 application／HTTP 邊界，不在瀏覽器自行修改 SQLite 或搬動檔案。

## 目前頁面選取

- 每筆 Library 結果提供可鍵盤操作的核取方塊；「全選本頁」與「反選本頁」只作用於畫面目前載入的結果。
- 搜尋、篩選、換頁或重新載入清單時會清空選取，避免把已離開畫面的收藏帶入高風險操作。
- 選取列與工作台都顯示筆數；批次操作失敗時只保留失敗項目，方便修正後重試。

## 批次 metadata 與 tag

批次加入 tag 逐筆呼叫冪等 endpoint。已具有同名 tag 的收藏列為「未變更」，不是失敗。批次 metadata 目前 allowlist 只有 `parody` 與 `classification`，並逐筆建立 manual assertion，因此仍遵守：

`手動修改 > 外部 metadata > 檔名解析 > 推斷結果`

同批允許部分成功；UI 分別列出成功、未變更與失敗，不以單一 toast 隱藏個別結果。

## 搬移與刪除

搬移對話框先列出每筆來源與目的典藏庫，再送出 collection IDs 與 archive root ID。後端仍重新驗證來源必須是 downloads、目的 root 已啟用、路徑位於註冊來源內且不覆寫同名 ZIP。

刪除對話框預設使用軟刪除，文字明示實體 ZIP 先送到作業系統資源回收桶並保留 catalog tombstone。永久刪除必須改選模式並完整輸入 `永久刪除 N 筆`；按鈕在文字不一致時保持 disabled。兩種模式都逐筆回報 `succeeded`、`failed` 與 `pending_recovery`。

## 同名候選與身分合併

候選卡片的 `confirmed`／`rejected` 只記錄人工判斷，不會在 PATCH 後立即合併。已確認候選要再執行 preflight；畫面會：

- 顯示 background job 等 blockers。
- 對每個手動值衝突並列 tombstone／candidate 值、來源與 assertion ID。
- 要求每個衝突欄位明確二選一。
- 要求完整輸入 `合併 tombstone_id <- candidate_id` 才能提交。

成功後 UI 明示存活 ID 與 merged candidate ID。Consolidation transaction 由較早的 tombstone ID 存活並接管 candidate current location；ZIP 不搬移，metadata assertions、tags、parser runs、外部結果與檔案操作歷史仍由 schema v4 的 audit model 保留。

## 瀏覽器驗證

隔離 catalog 的互動驗證包含：

- 重複批次 tag 由成功轉為未變更。
- downloads 與 archive 混合搬移得到一筆成功、一筆失敗，失敗項目留在選取清單。
- 永久刪除在確認文字完成前不可提交，完成後 ZIP 與 active collection 消失。
- consolidation title 衝突選擇 candidate 後，tombstone ID 存活、merged candidate 查詢回 410，實體 ZIP 仍存在原位置。
- 390px viewport 沒有水平 overflow，選取、搬移、刪除與身分裁決操作仍可到達，console 無錯誤。

所有驗證都使用 Rust 專案 `target` 下的暫存 catalog 與 ZIP；正式 `doujin.db` 不會交給 Rust server 或測試程序。

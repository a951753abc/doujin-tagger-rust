# BDD 審閱批次 10：Tombstone 身分合併 Example Mapping

> 審閱狀態：已接受（2026-08-12）

本批次只定義人工確認同名候選後的資料語意，不先決定 SQLite table／column 或 HTTP route。建議把「確認關聯」與「真正合併身分」拆成兩個動作，避免一次按鍵在尚未看見衝突時大量改寫 catalog。

## consolidation-case-001：確認關聯不立即合併

假設：

```text
舊收藏 #10：tombstone，最後位置 D:\old\作品.zip
新候選 #25：active，目前位置 E:\new\作品.zip
```

收藏管理者將 `#10 → #25` 標記為 `confirmed` 時，建議行為：

- 只保存「兩者已由人工確認為同一收藏」及裁決時間。
- `#10` 暫時仍是 tombstone，`#25` 暫時仍是 active。
- 不移動或重新命名實體 ZIP。
- 不立即搬移 metadata、tags、parser history 或外部搜尋紀錄。
- 真正改變身分前，系統應先提供 consolidation preflight。

這讓 `confirmed` 可以先完成審閱，不把一次關聯裁決等同於不可逆的大型 transaction。

## consolidation-case-002：舊收藏 ID 成為 survivor

當收藏管理者明確執行 consolidation，且 preflight 沒有未解衝突時，建議：

- 較早存在的 tombstone ID `#10` 成為 survivor，恢復為 active。
- `E:\new\作品.zip` 成為 `#10` 的 current location；舊位置繼續以 missing history 保存。
- 候選 `#25` 不再出現在有效 Library，也不再擁有 current location。
- `#25` 不立刻硬刪除；保存為 merged audit record，指向 survivor `#10`。
- 實體 ZIP 保持在 `E:\new\作品.zip`，consolidation 只修改 catalog。

保留舊 ID 的理由是：它已承載較早的 metadata、tags、人工裁決與外部參照。Merged record 則讓日後仍能解釋 `#25` 為何消失。

## consolidation-case-003：無衝突資料完整併入

若雙方資料沒有互斥的人工選擇，建議：

- Tags 採聯集；重複 tag 只保留一個關聯。
- 雙方 parser runs、metadata assertions、external search results、位置歷史與裁決證據全部保留。
- 每筆轉入資料保存原 collection ID 或 consolidation audit reference，不能偽裝成 survivor 原生資料。
- Effective metadata 仍依「手動 > 外部 > 檔名 > 推斷」重建。
- Tombstone 原有的已選值預設保持；候選的不同非手動值保存為未選候選，不因 consolidation 自動覆寫。
- FTS 與 effective projection 在同一 transaction 內重建。

## consolidation-case-004：不同手動值必須先逐欄裁決

例如：

```text
#10 手動標題：作品正式名稱
#25 手動標題：作品名稱 修正版
```

建議行為：

- Preflight 應列出 `title` 衝突及雙方 assertion／來源／時間。
- 只要仍有不同的手動選擇，consolidation 不得開始，catalog 保持原狀。
- 收藏管理者必須逐欄選擇 tombstone 值、candidate 值，或取消 consolidation。
- 失去選取的手動 assertion 仍保存，不刪除也不改寫來源。
- 非手動候選不同不阻擋身分合併；它們保存為未選候選，之後仍可在 metadata history 裁決。

## consolidation-case-005：多個同名候選必須全部裁決

假設 tombstone `#10` 同時關聯 `#25`、`#26`：

- 系統不得只因 `#25` 已 confirmed 就自動處理 `#26`。
- 執行 consolidation 前，必須恰有一筆 confirmed，且其他同組候選都已明確 rejected。
- 只要仍有 pending 候選或多筆 confirmed，preflight 應阻止 consolidation 並列出未解項目。
- Rejected 候選保持獨立 active 收藏，且拒絕紀錄永久保留，避免重複詢問。

這避免系統把另一份可能的搬移、複本或真正重複收藏擅自排除。

## consolidation-case-006：Transaction 必須可重試且全部 rollback

Consolidation 應在單一 repository writer transaction 中完成：

- survivor 狀態與 current location
- candidate merged audit state
- metadata／parser／external evidence 歸屬或 audit reference
- tags 聯集
- candidate links 與 consolidation record
- effective metadata 與 FTS projection

其中任何 constraint、磁碟或 database 錯誤都必須全部 rollback；實體 ZIP 不受影響。相同 consolidation request 重送時應回傳既有成功結果，不得再次複製 assertions、tags 或歷史紀錄。

## consolidation-case-007：合併後仍可完整追查

完成後：

- Library 與搜尋結果只出現 survivor `#10` 一次。
- 收藏詳細資料顯示目前位置 `E:\new\作品.zip`。
- Audit history 可看見舊 missing path、candidate ID、確認人為裁決、consolidation 時間及欄位衝突決策。
- 直接查詢 merged candidate `#25` 時，不回傳一般 active collection；應指出它已合併至 `#10`。
- 不提供自動拆分／復原 consolidation；若未來需要復原，必須另立 BDD 與可逆 transaction 設計。

## 本批次刻意不處理

- 消失收藏完全沒有同名候選時，應 tombstone、刪除或等待多久。
- Confirmed candidate 的實體 ZIP 搬移或重新命名。
- 自動判斷內容相同的 digest／hash 規則。
- Consolidation 的復原流程。

## 回覆格式

全部同意時可以直接回覆：

```text
批次 10 接受
```

若要修改，可只列案例，例如：

```text
consolidation-case-002：改由目前 active candidate ID 存活。
consolidation-case-004：有手動衝突時仍先合併，預設保留 tombstone 值。
```

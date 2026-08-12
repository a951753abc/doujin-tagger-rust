# BDD 審閱批次 07：Rust v2 儲存模型 Example Mapping

> 審閱狀態：已接受（2026-08-12）

本批次確認可觀察的資料語意；SQLite table／column 名稱會在接受後才寫成 DDL。建議基於 [SQLite 儲存引擎評估](../../references/sqlite-storage-evaluation.md) 與 DEC-001～DEC-029。八個案例已由收藏管理者全數接受，並記錄為 DEC-030～DEC-037。

## storage-v2-case-001：Database 是 catalog，不是內容容器

假設收藏為：

```text
D:\library\作品.zip
```

建議行為：

- ZIP 保留在檔案系統，不複製進 SQLite BLOB。
- SQLite 保存收藏身分、目前位置、metadata、tags、來源、檔案 digest 與處理狀態。
- 縮圖放在可清除並重建的 cache；SQLite 只記錄其狀態與 cache key。
- Parser／外部 metadata 的完整證據可保存為 JSON，但常用搜尋欄位必須可以直接索引。

## storage-v2-case-002：收藏身分與實體位置分開

假設系統主動將下載區收藏搬到歸檔區：

```text
D:\downloads\作品.zip
→ E:\archive\C106\作品.zip
```

建議行為：

- 經系統授權且成功的搬移沿用同一個收藏 ID、metadata 與 tags。
- 位置紀錄保存舊路徑、新路徑、操作時間與結果。
- 掃描只看到「舊路徑消失＋同名新路徑」時，不得沿用收藏 ID；仍依 DEC-008／009 建立新收藏與待裁決關聯。
- 因此「系統知道自己剛完成的 move」與「scanner 猜測可能搬移」是不同事件。

## storage-v2-case-003：所有 metadata 值都是有來源的候選

假設標題同時具有：

```text
手動修改：作品正式名稱
外部 metadata：作品名称
檔名解析：作品_名称
```

建議行為：

- 三個值與各自來源都保存，不因選出有效值而刪除其他候選。
- 系統另保存「目前選中哪一筆候選」，本例預設選手動值。
- 每個欄位獨立選擇，外部結果可以只改善場次而不改標題。
- 手動清空依 DEC-022 移除手動候選的選中狀態，再重新選擇較低優先候選；不建立最高優先的空字串。
- 被拒絕或失效的候選保留狀態與理由，但不進入目前有效值。

## storage-v2-case-004：Canonical 名稱不是直接改寫所有歷史值

假設原作名稱包含：

```text
檔名 raw：ポケモン
已確認官方名稱：ポケットモンスター
```

建議行為：

- 保存 raw 名稱、canonical entity、採用的 alias mapping 與證據。
- 修改 canonical 只改變目前顯示／搜尋投影，不覆寫 parser run 的歷史 raw payload。
- 使用者拒絕兩個名稱合併時，保存對稱的 merge exclusion，避免交換順序後再次提出相同建議。
- 刪除 canonical entity 前必須先處理仍引用它的 metadata 與 alias，不允許懸空引用。

## storage-v2-case-005：Tombstone 是保留資料的收藏狀態

假設收藏路徑消失，scanner 找到一筆或多筆同名候選：

- 舊收藏狀態改為 tombstone，不再出現在有效 Library。
- 舊 metadata、tags、最後位置與 parser／人工修改紀錄仍保存。
- 每一筆新候選是獨立收藏，不共用舊收藏的有效 metadata。
- 候選關聯保存「為何被連結、何時發現、是否已裁決」。
- 人工確認同一收藏後才可合併身分；拒絕後保存裁決，避免重複詢問。

## storage-v2-case-006：有效 metadata 與全文搜尋是可重建投影

建議行為：

- Metadata 候選、選擇紀錄與人工裁決是權威資料。
- 另外維護一份扁平的 effective metadata，供列表、篩選、排序與 API 快速讀取。
- FTS 索引只索引 effective title、circle、authors、parody 等目前有效文字。
- 投影或 FTS 損壞時，可以從權威資料重建，不改變使用者選擇。
- Tags 與人工裁決不是投影，不得在重建搜尋索引時清除。

## storage-v2-case-007：單一 writer 與 transaction 邊界

建議行為：

- Scanner 先產生待入庫資料，再由 repository writer 於 transaction 中提交。
- 同一收藏的建立、metadata 候選、有效值投影、FTS 與操作紀錄必須一起成功或一起 rollback。
- UI 人工修改與背景工作都交給同一個 writer queue，不讓多個元件各自持有長時間寫入 transaction。
- 批次中某筆資料違反 constraint 時，結果必須指出失敗項目；是否整批 rollback 依該操作既有 BDD 決定。
- Database busy、磁碟空間不足或 migration failure 必須回報，不得當成成功略過。

## storage-v2-case-008：以新資料庫進行可回復遷移

建議行為：

- 不直接把目前 `doujin.db` 原地改造成 v2 schema。
- Migration 先建立新的 `doujin-v2.db`，唯讀匯入舊資料並產生驗證報告。
- 驗證至少比較收藏數、tags 關聯數、空值分布、路徑衝突與抽樣 metadata。
- 使用者確認前，Python 版本與舊資料庫仍可正常使用。
- 切換成功後仍保留舊資料庫備份；rollback 不需要反向轉換 v2 資料。
- 真實遷移前先以資料庫副本完整演練，不對唯一的正式資料執行第一次 migration。

## 接受後的預定資料群組

這不是最終 DDL，只表示每項資料的責任範圍：

| 資料群組 | 可能包含 |
|---|---|
| 收藏與位置 | collections、locations、library roots、file operations |
| Metadata | assertions、current selections、effective projection、parser runs |
| Canonical | entities、name variants、merge exclusions |
| Tags | tags、collection-tag relations |
| 生命週期 | tombstones、candidate links、delete／move records |
| 背景工作 | scan runs、issues、external searches、thumbnail jobs |
| 搜尋 | FTS projection |
| Schema | migrations／user version |

## 回覆格式

全部同意時可以直接回覆：

```text
批次 07 接受
```

若有修改：

```text
storage-v2-case-002：系統主動搬移後也建立新的收藏 ID，原因是……
storage-v2-case-008：希望直接原地升級，原因是……
```

@proposed @rust-v2
Feature: Rust v2 的收藏儲存與可追溯資料
  收藏管理者希望收藏身分、metadata 與檔案位置可以安全保存並追溯，
  同時能在索引或遷移失敗時完整回復。

  @dec-030 @storage-v2-001
  Scenario: 內容檔案與 catalog 分開保存
    Given 檔案系統中存在一筆 ZIP 收藏
    When 系統將收藏加入 Library
    Then ZIP 應留在原本的檔案系統位置
    And catalog 應保存收藏身分、位置、metadata 與處理狀態
    And 可重建的縮圖不應成為收藏內容的唯一副本

  @dec-031 @storage-v2-002
  Scenario: 系統完成搬移後沿用收藏身分
    Given 一筆下載區收藏已具有 metadata 與 tags
    When 系統成功將檔案搬到已設定的歸檔區
    Then 新位置應沿用原本的收藏 ID
    And 原本的 metadata 與 tags 應保持不變
    And 位置歷史應保存搬移前後路徑與結果

  @dec-031 @dec-034 @storage-v2-003
  Scenario: Scanner 不以同名候選猜測收藏身分
    Given 一筆既有收藏的路徑已消失
    And scanner 在其他位置找到同名 ZIP
    When 系統處理掃描結果
    Then 舊收藏應成為保留資料的 tombstone
    And 同名 ZIP 應建立為獨立收藏
    And 未經人工裁決不得沿用舊收藏 ID 或 metadata

  @dec-032 @storage-v2-004
  Scenario: 同一欄位保存多個來源候選
    Given 標題具有手動、外部 metadata 與檔名解析三個不同候選
    When 系統依來源優先序選出目前標題
    Then 每個候選及其來源都應保留
    And 目前選擇應指向手動候選
    And 未選中的候選不應因此被刪除

  @dec-022 @dec-032 @storage-v2-005
  Scenario: 清除手動候選後重新選擇有效值
    Given 某欄位目前選中手動候選
    And 該欄位仍有外部 metadata 候選
    When 收藏管理者清除手動值
    Then 系統不應建立手動空白候選
    And 外部 metadata 候選應依優先序成為目前有效值

  @dec-005 @dec-016 @dec-033 @storage-v2-006
  Scenario: 修改 canonical 不改寫歷史 raw 值
    Given parser 歷史保存原作 raw 值「ポケモン」
    And 該值目前對應 canonical「ポケットモンスター」
    When 收藏管理者修改 canonical mapping
    Then 目前顯示與搜尋可以反映新的 canonical
    But parser 歷史中的 raw 值仍應為「ポケモン」

  @dec-035 @storage-v2-007
  Scenario: 從權威資料重建有效值與搜尋索引
    Given metadata 候選、目前選擇、tags 與人工裁決仍完整
    And 有效 metadata 或搜尋索引需要重建
    When 系統執行投影重建
    Then 列表與搜尋應重新反映目前選擇的 metadata
    And tags 與人工裁決不應被清除或改變

  @dec-036 @storage-v2-008
  Scenario: 同一收藏的入庫資料以 transaction 提交
    Given scanner 已產生一筆待入庫收藏
    When repository 提交收藏、位置、parser 證據、metadata 候選與讀取投影
    Then 這些資料應一起成功
    And 成功後搜尋索引應能找到目前有效值

  @dec-036 @storage-v2-009
  Scenario: Constraint failure 回復整筆收藏入庫
    Given repository 正在提交一筆待入庫收藏
    When 其中一項資料違反 storage constraint
    Then 該收藏的所有本次入庫資料都應 rollback
    And 系統應明確回報失敗而不是當成成功略過

  @dec-037 @storage-v2-010
  Scenario: 以新 catalog 演練可回復遷移
    Given 現有版本仍使用舊 catalog
    When 系統演練 Rust v2 migration
    Then 系統應建立新的 v2 catalog 並唯讀匯入舊資料
    And 系統應產生收藏數、tags、空值與路徑衝突的驗證結果
    And 舊 catalog 應保持可用且未被原地改寫

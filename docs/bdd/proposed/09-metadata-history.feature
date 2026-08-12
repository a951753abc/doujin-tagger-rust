@proposed
Feature: Rust v2 的 metadata 候選與來源歷史
  收藏管理者希望理解 effective value 為何生效，並檢視尚未裁決或僅供追查的外部結果。

  @metadata-v2-020
  Scenario: 以固定欄位順序讀取 metadata 歷史
    Given 一筆 active 收藏具有部分 metadata assertions
    When Library 查詢該收藏的 metadata 歷史
    Then response 應依 title、event、circle、authors、parody、classification、is_dl 回傳七個欄位
    And 沒有 assertions 的欄位也應以空清單呈現

  @dec-007 @metadata-v2-021
  Scenario: 顯示目前 selection 與所有來源候選
    Given 同一欄位具有 manual、external、filename 或 inference assertions
    When Library 查詢該欄位歷史
    Then 每筆 assertion 應顯示 value、source、status、理由與建立時間
    And 欄位應顯示目前 selected assertion ID、選擇方式與時間
    And 只有目前 selected assertion 應標記為 selected

  @dec-014 @metadata-v2-022
  Scenario: 顯示外部候選的完整 confidence 證據
    Given 一筆 external assertion 具有 confidence 紀錄
    When Library 查詢該 assertion
    Then response 應包含 0 到 1 的 confidence total
    And response 應包含來源可靠度、識別碼匹配、字串相似度、規則確定度與理由
    And response 應保留外部來源參照

  @dec-015 @metadata-v2-023
  Scenario: 區分可套用候選與低信心搜尋紀錄
    Given 外部搜尋同時產生 suggestion 與低信心 search-only 結果
    When Library 查詢 metadata 歷史
    Then suggestion 可以同時指向一筆 metadata assertion
    And search-only 結果不應具有 assertion ID
    And 兩者的 value、來源參照、confidence、disposition 與時間都應保留

  @security @metadata-v2-024
  Scenario: 只讀取 active 收藏的 metadata 歷史
    Given collection ID 不存在或收藏是 tombstone 或 soft-deleted
    When Library 查詢 metadata 歷史
    Then API 應回傳找不到收藏
    And 不應洩漏該收藏的 assertions 或外部搜尋紀錄

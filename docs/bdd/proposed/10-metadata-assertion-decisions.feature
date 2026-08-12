@proposed
Feature: Rust v2 的 metadata assertion 人工裁決
  收藏管理者希望選擇或拒絕既有候選，同時保留原始資料與可追溯證據。

  @dec-007 @metadata-v2-025
  Scenario: 選擇既有候選成為 effective value
    Given 一筆 active 收藏的欄位具有 candidate 或 accepted assertion
    When 收藏管理者選擇該 assertion
    Then 系統應將 candidate 標記為 accepted
    And 該 assertion 應成為人工選擇的 effective value
    And 後續自動結果不得覆寫該 selection

  @metadata-v2-026
  Scenario: 拒絕目前選中的 assertion
    Given 某欄位目前由一筆可裁決 assertion 生效
    When 收藏管理者拒絕該 assertion
    Then 系統應保留該 assertion 並將狀態標記為 rejected
    And 系統應依來源優先序選擇下一筆 accepted assertion
    And 沒有其他 accepted assertion 時欄位應保持未設定

  @metadata-v2-027
  Scenario: 拒絕未選中的 assertion 不改變 effective value
    Given 某欄位具有目前 selection 與另一筆未選中的候選
    When 收藏管理者拒絕未選中的候選
    Then 目前 selection 與 effective value 應保持不變
    And 被拒絕候選應保留在 metadata 歷史

  @dec-014 @metadata-v2-028
  Scenario: 拒絕候選時保留原始證據
    Given 一筆候選具有來源參照、confidence 與原始理由
    When 收藏管理者拒絕該候選
    Then 來源參照、confidence、原始理由與建立時間都不應被改寫
    And metadata 歷史應顯示該候選為 rejected

  @security @metadata-v2-029
  Scenario: Assertion 必須屬於 URL 指定的收藏與欄位
    Given assertion ID 屬於另一筆收藏或另一個 metadata 欄位
    When 呼叫端要求選擇或拒絕該 assertion
    Then API 應拒絕裁決
    And 兩筆收藏的 selection、effective value 與 assertion 狀態都不應改變

  @security @metadata-v2-030
  Scenario: 只裁決 active 收藏中仍可用的 assertion
    Given 收藏不存在、不是 active，或 assertion 已是 rejected 或 obsolete
    When 呼叫端要求選擇該 assertion
    Then API 應回傳結構化錯誤
    And 不應修改 metadata
    And 重複拒絕同一筆 rejected assertion 可以視為無變更的成功操作

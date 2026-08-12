@proposed
Feature: Rust v2 的手動 metadata 與 tags 寫入
  收藏管理者希望修正單筆收藏資料，並保留自動來源候選與可追溯優先序。

  @dec-007 @metadata-v2-016
  Scenario: 寫入單一 allowlisted metadata 欄位
    Given 一筆 active 收藏具有檔名解析或其他來源候選
    When 收藏管理者以符合欄位型別的值寫入 title、event、circle、authors、parody、classification 或 is_dl
    Then 系統應建立新的 manual assertion
    And manual assertion 應成為該欄位的 effective value
    And 既有較低優先候選應保留供追溯

  @security @metadata-v2-017
  Scenario: 拒絕非 metadata 欄位與錯誤型別
    Given 呼叫端指定 path、source、時間戳記、未知欄位或不符合欄位型別的值
    When 呼叫端要求寫入手動 metadata
    Then 系統不應修改收藏
    And API 應回傳結構化輸入錯誤

  @dec-022 @metadata-v2-018
  Scenario: 清除手動值後重新套用來源優先序
    Given 某欄位目前由 manual assertion 生效
    When 收藏管理者清除該手動值
    Then 系統應將 manual assertion 標記為不再生效
    And 最高優先的剩餘候選應成為 effective value
    And 沒有剩餘候選時欄位應保持未設定

  @security @metadata-v2-019
  Scenario: 只修改 active 收藏
    Given collection ID 不存在或收藏已是 tombstone 或 soft-deleted
    When 呼叫端要求修改 metadata 或 tags
    Then 系統不應寫入該收藏
    And API 應回傳找不到收藏

  @tag-v2-001
  Scenario: 冪等新增單一 tag
    Given 收藏管理者輸入非空白 tag 名稱
    When 收藏管理者將 tag 加入 active 收藏一次或多次
    Then 系統應建立或重用同名 tag
    And 收藏與 tag 之間最多只能有一筆關聯

  @tag-v2-002
  Scenario: 移除 tag 並清理孤兒資料
    Given 一個 tag 目前與一筆或多筆收藏關聯
    When 收藏管理者從指定收藏移除該 tag
    Then 系統應只移除指定收藏的 tag 關聯
    And tag 沒有任何剩餘關聯時應刪除該 tag
    And 重複移除應是無變更的成功操作

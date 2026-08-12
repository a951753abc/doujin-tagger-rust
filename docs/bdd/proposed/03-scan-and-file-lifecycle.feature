@proposed @rust-v2
Feature: Rust v2 的掃描與檔案生命週期
  收藏管理者希望掃描、搬移與刪除不會因同名或批次錯誤而誤套資料，
  並能明確選擇可還原或永久的刪除方式。

  @scan-v2-002
  Scenario: 沒有設定掃描來源
    Given Rust scanner 沒有收到任何掃描來源
    When 系統執行新收藏掃描
    Then 系統不應產生待入庫收藏
    And 結果應回報尚未設定掃描來源

  @scan-v2-003
  Scenario: 跳過遺失來源並繼續其他來源
    Given Rust scanner 收到一個不存在的來源與一個存在的來源
    And 存在來源中有一筆新 ZIP 收藏
    When 系統執行新收藏掃描
    Then 遺失來源應記錄為掃描問題
    And 新 ZIP 仍應成為待入庫收藏

  @dec-006 @scan-v2-004
  Scenario: 遞迴發現新 ZIP 並跳過既有路徑
    Given 掃描來源與子資料夾中具有 ZIP 收藏
    And 收藏索引已包含其中一個完整路徑
    When 系統執行新收藏掃描
    Then scanner 應遞迴發現 ZIP
    And 既有路徑不應重新解析或重新命名
    And 只有新路徑應產生待入庫資料
    And 待入庫資料應記錄來源類型、所在資料夾與 parser 版本

  @security @scan-v2-005
  Scenario: 不進入排除目錄或目錄 symlink
    Given 掃描樹中包含應用程式、版本控制、套件、系統目錄或目錄 symlink
    When 系統執行新收藏掃描
    Then scanner 不應進入那些目錄
    And 來源外的檔案不應成為待入庫收藏

  @dec-023 @scan-v2-006
  Scenario: 新收藏改名成功後使用新路徑
    Given 新 ZIP 的 percent-encoded 檔名可安全解碼並解析結構
    When Rust scanner 處理該收藏
    Then scanner 應在實體改名成功後產生待入庫資料
    And 待入庫路徑應使用解碼後的實體檔名
    And 結果應保存原路徑與新路徑的正規化紀錄

  @dec-023 @scan-v2-007
  Scenario: 新收藏改名衝突時保留原路徑
    Given 新 ZIP 的解碼後檔名與同目錄既有檔案衝突
    When Rust scanner 處理該收藏
    Then scanner 不應覆寫既有檔案
    And 原始 ZIP 應保持原名
    And 待入庫資料應使用原路徑並附上正規化警告

  @dec-008 @dec-009 @scan-v2-001
  Scenario Outline: 同名候選只建立待人工裁決的關聯
    Given 一筆既有收藏的原始路徑已不存在
    And 掃描發現 <candidate_count> 同檔名候選收藏
    When 系統完成重新掃描
    Then 舊收藏不應再出現在有效 Library
    And 舊收藏應以 tombstone 保留原 metadata 與 tags
    And 每一筆同名候選都應保持為獨立收藏
    And 系統不應自動將舊 metadata 或 tags 套用到任一候選
    And 系統應將這組關聯標記為待人工裁決

    Examples:
      | candidate_count |
      | 一筆            |
      | 多筆            |

  @dec-010 @security @destructive @file-v2-001
  Scenario: 後端拒絕搬移非下載區收藏
    Given 收藏位於下載區以外的掃描來源
    And 搬移目的地是已設定的歸檔區
    When 呼叫端要求後端搬移該收藏
    Then 後端不應搬移實際檔案
    And 收藏索引應保持原狀
    And 結果應回報來源不是下載區

  @dec-010 @security @destructive @file-v2-002
  Scenario: 後端允許從下載區搬到已設定的歸檔區
    Given 收藏位於下載區
    And 搬移目的地是已設定的歸檔區
    When 呼叫端要求後端搬移該收藏
    Then 後端可以執行搬移
    And 成功後收藏索引應指向新的歸檔位置

  @dec-011 @destructive @file-v2-003
  Scenario: 批次搬移允許部分成功
    Given 一批下載區收藏中只有部分項目可以安全搬移
    When 系統完成批次搬移
    Then 可搬移項目應完成搬移並更新各自的索引
    And 不可搬移項目的檔案與索引應保持原狀
    And 結果應逐筆標示成功或失敗及失敗原因

  @dec-012 @destructive @file-v2-004
  Scenario: 刪除前明確選擇刪除模式
    Given 收藏管理者要求刪除一筆可刪除的收藏
    When 系統顯示刪除確認
    Then 收藏管理者應能選擇軟刪除或永久刪除
    And 未明確選擇並確認前不應刪除檔案或修改索引

  @dec-012 @external @destructive @file-v2-005
  Scenario: 軟刪除收藏到作業系統資源回收桶
    Given 收藏管理者已選擇並確認軟刪除
    When 系統執行刪除
    Then 實際檔案應移到作業系統資源回收桶
    And 收藏不應再出現在有效 Library
    And 系統應保留可供還原的 tombstone、metadata 與 tags

  @dec-012 @destructive @file-v2-006
  Scenario: 永久刪除收藏
    Given 收藏管理者已選擇並確認永久刪除
    When 系統執行刪除
    Then 實際檔案應被永久刪除且不經作業系統資源回收桶
    And 收藏索引及其 tag 關聯應被永久刪除
    And 系統不應建立可供還原的 tombstone

  @dec-043 @destructive @file-v2-007
  Scenario Outline: 沒有有效場次時一律使用未分類歸檔資料夾
    Given 歸檔區為 "I:\同人誌"
    And 下載區收藏沒有有效場次
    And 該收藏的 `is_dl` 為 <is_dl>
    When 系統執行歸檔搬移
    Then 收藏應搬到 "I:\同人誌\未分類"

    Examples:
      | is_dl |
      | true  |
      | false |

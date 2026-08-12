Feature: 本機應用的安全與輸入邊界
  收藏管理者希望所有會操作檔案或資料的要求都限制在明確允許的範圍。

  @current @security @boundary-001
  Scenario: 拒絕操作掃描來源以外的收藏路徑
    Given 收藏索引中的檔案路徑不位於任何目前設定的掃描來源內
    When 呼叫端要求開啟、閱讀或刪除該收藏
    Then 系統應拒絕操作
    And 系統不應存取或修改該路徑

  @current @security @boundary-002
  Scenario: 相似字首不代表位於允許路徑內
    Given 允許來源為 "D:/library"
    And 目標路徑為 "D:/library-other/item.zip"
    When 系統驗證目標路徑
    Then 系統不應將目標視為允許來源的子路徑

  @current @security @boundary-003
  Scenario: 拒絕來自其他網站的寫入要求
    Given POST、PUT 或 DELETE 要求帶有 Origin 或 Referer
    And 該標頭文字不包含 "localhost" 或 "127.0.0.1"
    When 系統收到要求
    Then 系統應拒絕寫入

  @current @needs-confirmation @security @boundary-004
  Scenario: 沒有 Origin 與 Referer 的寫入要求仍被接受
    Given POST、PUT 或 DELETE 要求沒有 Origin 與 Referer
    When 系統收到要求
    Then 系統不應只因缺少來源標頭而拒絕寫入

  @current @security @boundary-005
  Scenario: 只允許支援的重複合併欄位
    Given 呼叫端要求對場次、社團、作者與原作以外的欄位偵測或合併
    When 系統處理要求
    Then 系統不應讀取或修改不支援欄位

  @current @security @boundary-006
  Scenario: 只保存允許的 Web 設定
    Given 呼叫端提交閱讀器、掃描來源、縮圖設定以外的設定鍵
    When 系統保存 Web 設定
    Then 系統應忽略未允許的設定鍵

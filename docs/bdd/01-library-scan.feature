Feature: 收藏來源與重新掃描
  收藏管理者希望系統能從已設定的資料夾建立收藏索引，
  並在檔案新增、移動或消失後維持索引一致。

  @current @scan-001
  Scenario: 尚未設定任何掃描來源
    Given 系統沒有任何掃描來源
    When 收藏管理者要求重新掃描
    Then 系統不應新增或移除收藏
    And 系統應回報沒有可掃描的來源

  @current @scan-002
  Scenario: 某個掃描來源不存在
    Given 已設定多個掃描來源
    And 其中一個來源路徑不存在
    When 收藏管理者要求重新掃描
    Then 系統應跳過不存在的來源
    And 系統應繼續掃描其他存在的來源

  @current @scan-003
  Scenario: 從掃描來源遞迴發現 ZIP 收藏
    Given 一個標記為「歸檔區」或「下載區」的掃描來源
    And 來源的子資料夾中存在 ZIP 檔案
    When 系統掃描該來源
    Then 每個新發現的 ZIP 檔案應成為一筆收藏
    And 收藏應保留其完整路徑、所在資料夾與來源類型

  @current @scan-004
  Scenario: 將圖片資料夾視為一筆收藏
    Given 某個資料夾含有支援的圖片檔案
    And 該資料夾不含 ZIP 檔案
    When 系統掃描該資料夾
    Then 該資料夾本身應成為一筆收藏
    And 系統不應再把其子資料夾各自當成另一筆收藏

  @current @scan-005
  Scenario: 排除應用程式與系統資料夾
    Given 掃描樹中包含應用程式、版本控制、套件或系統回收資料夾
    When 系統遞迴掃描來源
    Then 系統不應進入已知應排除的資料夾

  @current @needs-confirmation @scan-006
  Scenario: 相同路徑的收藏已經存在
    Given 收藏索引中已有某個檔案路徑
    And 新版 parser 對該檔名可能產生不同結果
    When 系統再次掃描相同路徑
    Then 系統應跳過該收藏
    And 現有 metadata 不應被重新解析或更新

  @current @scan-007
  Scenario: 同一時間只允許一個重新掃描工作
    Given 一次重新掃描尚未完成
    When 收藏管理者再次要求重新掃描
    Then 系統應拒絕第二個掃描工作
    And 系統應回報掃描正在執行

  @current @scan-008
  Scenario: 已消失收藏有唯一的同名新位置
    Given 一筆既有收藏的原始路徑已不存在
    And 索引中只有一筆同檔名收藏指向目前存在的新位置
    When 系統重新掃描
    Then 原收藏的 metadata 與 tags 應遷移到新位置的收藏
    And 原收藏索引應被移除
    And 系統應將此次變更計為遷移

  @current @needs-confirmation @scan-009
  Scenario: 已消失收藏有多個同名新位置
    Given 一筆既有收藏的原始路徑已不存在
    And 索引中有多筆同檔名收藏指向目前存在的位置
    When 系統重新掃描
    Then 系統不應把舊 metadata 自動套用到任一候選收藏
    And 原收藏索引應被移除
    And 系統應回報一次有歧義的遷移

  @current @scan-010
  Scenario: 已消失收藏沒有替代位置
    Given 一筆既有收藏的路徑已不存在
    And 沒有可辨識的替代位置
    When 系統重新掃描
    Then 該收藏及其 tag 關聯應從索引移除
    And 實際不存在的檔案不需要再次刪除

  @current @scan-011
  Scenario: 掃描完成後回報摘要
    Given 系統完成所有可用來源的掃描
    When 掃描結果回傳給收藏管理者
    Then 摘要應包含發現、加入、跳過、移除與遷移的數量
    And 摘要應包含解析完整與僅有標題的數量
    And 摘要應包含花費時間


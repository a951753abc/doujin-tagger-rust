Feature: 開啟、閱讀、搬移與刪除收藏檔案
  收藏管理者希望從 Library 操作實際檔案，同時避免越界、覆寫或誤刪。

  @current @external @file-001
  Scenario: 使用作業系統預設程式開啟收藏
    Given 收藏檔案存在於已設定的掃描來源內
    When 收藏管理者要求開啟檔案
    Then 系統應交由目前作業系統的預設程式開啟

  @current @external @file-002
  Scenario: 使用指定閱讀器閱讀收藏
    Given 收藏檔案存在於已設定的掃描來源內
    And 已設定一個存在的閱讀器程式
    When 收藏管理者要求閱讀收藏
    Then 系統應以該閱讀器開啟收藏檔案

  @current @external @file-003
  Scenario: 指定閱讀器不存在
    Given 閱讀器路徑為空白或指向不存在的程式
    When 收藏管理者要求閱讀收藏
    Then 系統不應啟動外部程式
    And 系統應回報閱讀器不存在

  @current @file-004
  Scenario: 索引中的收藏檔案已不存在
    Given 收藏仍存在於索引
    And 實際檔案已不存在
    When 收藏管理者要求開啟或閱讀收藏
    Then 系統不應啟動外部程式
    And 系統應回報檔案不存在

  @current @ui-local @file-005
  Scenario: 成功閱讀後加入最近開啟
    Given 收藏成功交給指定閱讀器
    When Library 更新最近開啟清單
    Then 該收藏應移到清單最前方
    And 同一收藏不應重複出現
    And 清單最多保留 20 筆

  @current @destructive @file-006
  Scenario: 刪除 ZIP 收藏
    Given 收藏管理者已在 UI 確認無法復原的刪除操作
    And 目標是已設定掃描來源內的 ZIP 檔案
    When 系統執行刪除
    Then 實際 ZIP 檔案應被刪除
    And 收藏索引及其 tag 關聯應被刪除

  @current @destructive @file-007
  Scenario: 拒絕刪除非 ZIP 收藏
    Given 目標收藏是圖片資料夾或其他非 ZIP 路徑
    When 呼叫端要求刪除收藏
    Then 系統不應刪除實際路徑
    And 系統應回報只允許刪除 ZIP 檔案

  @current @destructive @file-008
  Scenario: 將下載區收藏搬到唯一歸檔區
    Given 收藏管理者選取下載區收藏
    And 系統只設定一個歸檔區
    And 收藏管理者確認搬移
    When 系統執行批次搬移
    Then 收藏應搬到該歸檔區的場次子資料夾
    And 收藏索引的路徑、資料夾與來源應同步更新

  @current @destructive @file-009
  Scenario: 從多個歸檔區選擇搬移目的地
    Given 系統設定多個歸檔區
    When 收藏管理者要求批次搬移
    Then 系統應要求收藏管理者選擇其中一個目的地
    And 無效或取消的選擇不應搬移任何檔案

  @current @security @destructive @file-010
  Scenario: 搬移目的地必須是已設定歸檔區
    Given 呼叫端指定未列在設定中的目的地
    When 系統收到批次搬移要求
    Then 系統不應搬移任何檔案
    And 系統應回報目的地不被允許

  @current @destructive @file-011
  Scenario: 依場次建立安全的目的資料夾名稱
    Given 收藏的場次包含 Windows 不允許的字元、保留名稱或空白值
    When 系統建立歸檔目的地
    Then 系統應替換不安全字元並避免保留名稱
    And 空白場次應使用「未分類」
    And 最終目的地不得逃出指定歸檔區

  @current @destructive @file-012
  Scenario: 搬移目的地已存在同名檔案
    Given 歸檔目的地已存在同名檔案
    When 系統執行批次搬移
    Then 系統不應覆寫既有檔案
    And 該筆收藏應留在原位置
    And 批次結果應列出該筆錯誤

  @current @needs-confirmation @destructive @file-013
  Scenario: 後端未限制只有下載區收藏可以搬移
    Given 目標目的地是有效歸檔區
    And 呼叫端提交的收藏可能來自下載區以外的掃描來源
    When 系統執行批次搬移
    Then 只要來源路徑位於任一允許掃描來源內，系統仍會嘗試搬移

  @current @destructive @file-014
  Scenario: 批次搬移部分成功
    Given 選取的收藏中只有部分項目可以安全搬移
    When 系統完成批次搬移
    Then 可搬移項目應完成搬移
    And 不可搬移項目應保持原狀
    And 結果應分別回報成功數與逐筆錯誤

  @current @external @file-015
  Scenario: 閱讀有子資料夾的圖片資料夾時選擇子資料夾
    Given 圖片資料夾收藏含有至少一個含圖片的子資料夾
    When 收藏管理者要求以閱讀器閱讀
    Then 系統應列出整個資料夾與各子資料夾供選擇
    And 只有位於該收藏資料夾內、且不是 symlink 的子資料夾可以被開啟
    And 系統預設開啟仍直接開啟收藏資料夾本身


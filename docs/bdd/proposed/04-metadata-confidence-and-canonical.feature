@proposed @rust-v2
Feature: Rust v2 的 metadata confidence 與 canonical 合併
  收藏管理者希望外部建議依可解釋的證據處理，
  並能控制自動覆寫、正式名稱與錯誤合併建議。

  @dec-013 @external @metadata-v2-004
  Scenario Outline: 所有欄位統一依來源優先序選擇有效值
    Given 收藏的 <field> 目前有效值來自檔名解析
    And 外部 metadata 對 <field> 提出不同值
    When 系統重新決定 <field> 的有效值
    Then 外部 metadata 值可以成為有效值
    And 原檔名解析值及其來源應繼續保留

    Examples:
      | field |
      | 場次  |
      | 社團  |
      | 作者  |
      | 原作  |

  @dec-013 @external @metadata-v2-005
  Scenario: 外部 metadata 不得自動覆寫手動值
    Given 收藏管理者已手動修改一個 metadata 欄位
    And 外部 metadata 對該欄位提出不同值
    When 系統處理外部建議
    Then 手動值應保持為有效值
    And 外部值只能成為待確認候選

  @dec-013 @external @metadata-v2-006
  Scenario: 使用者明確裁決以外部候選取代手動值
    Given 一個 metadata 欄位具有手動值與不同的外部候選
    When 收藏管理者明確選擇以該外部候選取代目前值
    Then 該外部候選應成為有效值
    And 系統應記錄這次選擇是手動裁決
    And 外部來源與先前的手動值都應保持可追溯

  @dec-014 @external @metadata-v2-007
  Scenario: Confidence 顯示綜合分數的組成與理由
    Given 系統產生一筆外部 metadata 候選
    When 系統顯示該候選的 confidence
    Then 應顯示 0 到 1 之間的總信心度
    And 應保留來源可靠度、識別碼匹配、字串相似度與規則確定度
    And 應提供人類可讀的判斷理由

  @dec-014 @external @metadata-v2-008
  Scenario: 可靠識別碼完全匹配優先於單純的高字串相似度
    Given 一筆候選具有可靠產品來源的完全相同識別碼
    And 另一筆候選只有更高的標題字串相似度
    When 系統排列兩筆候選
    Then 識別碼完全匹配的候選應排在前面
    And 兩筆候選的分項分數與理由都應保持可檢視

  @dec-015 @external @metadata-v2-009
  Scenario: 可靠識別碼完全匹配且沒有衝突時自動套用
    Given 外部候選的總信心度至少為 0.95
    And 候選具有可靠識別碼完全匹配
    And 候選不會覆寫任何手動值
    When 系統處理該候選
    Then 系統可以自動套用已驗證的欄位
    And 每個套用值都應保留外部來源與判斷證據

  @dec-015 @external @metadata-v2-010
  Scenario: 高總分但沒有可靠識別碼時仍需人工確認
    Given 外部候選的總信心度至少為 0.95
    And 候選沒有可靠識別碼完全匹配
    When 系統處理該候選
    Then 系統不應自動套用候選值
    And 候選應顯示為待人工確認

  @dec-015 @external @metadata-v2-011
  Scenario: 中等信心候選只顯示為待確認建議
    Given 外部候選的總信心度至少為 0.75 且低於 0.95
    When 系統處理該候選
    Then 系統不應自動套用候選值
    And 候選應顯示為可由收藏管理者確認的建議

  @dec-015 @external @metadata-v2-012
  Scenario: 低信心候選只保留搜尋紀錄
    Given 外部候選的總信心度低於 0.75
    When 系統處理該候選
    Then 系統不應自動套用候選值
    And 系統不應將候選顯示為可直接套用的建議
    And 搜尋來源、分數與候選內容應保留供追查

  @dec-015 @external @metadata-v2-013
  Scenario: 外部結果只有部分欄位可靠時可以部分成功
    Given 外部結果只有部分欄位符合自動套用條件
    And 其他欄位信心不足或與手動值衝突
    When 系統處理該外部結果
    Then 系統只應自動套用符合條件的欄位
    And 其他欄位應保持原值並依各自狀態等待確認或只保留紀錄
    And 系統應逐欄位記錄處理結果與理由

  @dec-016 @duplicate @canonical-v2-001
  Scenario: 已確認的官方名稱優先成為 canonical
    Given 同一實體具有日文名稱與非日文名稱
    And 其中一個名稱已確認為官方名稱
    When 系統選擇 canonical 名稱
    Then 應採用已確認的官方名稱
    And 不應僅因另一個名稱是日文而優先採用它
    And 其他名稱應保留為可追溯的別名

  @dec-016 @duplicate @canonical-v2-002
  Scenario: 沒有官方名稱時只提出 canonical 推薦
    Given 一組名稱變體都沒有已確認的官方名稱
    When 系統依語言、使用次數與其他證據評估 canonical
    Then 系統可以提出一個推薦名稱及理由
    And 系統不應僅依推薦結果自動合併名稱

  @dec-016 @duplicate @canonical-v2-003
  Scenario: 不再建議使用者已拒絕的合併
    Given 收藏管理者已判定一組名稱不應合併
    When 系統再次執行名稱變體偵測
    Then 該組名稱不應再次顯示為合併建議
    And 系統應保留不得合併的排除規則
    And 收藏管理者主動移除排除規則後才可再次建議

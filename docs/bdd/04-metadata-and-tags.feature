Feature: 編輯 metadata、tags 與批次資料
  收藏管理者希望能修正自動解析結果，並將相同變更套用到多筆收藏。

  @current @metadata-001
  Scenario: 編輯單筆收藏的主要 metadata
    Given 收藏管理者開啟一筆收藏的編輯視窗
    When 收藏管理者修改場次、社團、作者、標題、原作或分類並儲存
    Then 系統應保存允許的欄位
    And Library 應顯示更新後的結果

  @current @metadata-002
  Scenario: 清空可選的 metadata 欄位
    Given 一筆收藏已有場次、社團、作者、標題或原作
    When 收藏管理者清空該欄位並儲存
    Then 系統應將該欄位保存為未設定

  @current @security @metadata-003
  Scenario: 忽略不允許修改的欄位
    Given 呼叫端提交檔案路徑、來源、建立時間或其他未允許欄位
    When 系統更新收藏
    Then 系統不應修改這些欄位

  @current @needs-confirmation @metadata-004
  Scenario: 手動修正與自動解析結果沒有來源區別
    Given 收藏管理者手動修正一個 metadata 欄位
    When 系統保存修正
    Then 系統只保存最後的欄位值
    And 系統不記錄該值來自使用者、parser 或外部建議

  @current @tag-001
  Scenario: 為收藏新增 tag
    Given 收藏管理者輸入非空白 tag 名稱
    When 收藏管理者將 tag 加到收藏
    Then 系統應建立或重用同名 tag
    And 收藏與 tag 應建立關聯

  @current @tag-002
  Scenario: 重複加入相同 tag
    Given 收藏已經具有某個 tag
    When 收藏管理者再次加入同名 tag
    Then 系統不應建立重複關聯

  @current @tag-003
  Scenario: 移除最後一個 tag 關聯
    Given 某個 tag 只被一筆收藏使用
    When 收藏管理者從該收藏移除 tag
    Then 收藏與 tag 的關聯應被移除
    And 不再被任何收藏使用的 tag 應一併移除

  @current @batch-001
  Scenario: 選取目前頁面的收藏
    Given Library 正在顯示一頁搜尋結果
    When 收藏管理者執行全選或反選
    Then 選取狀態只應作用於目前顯示的結果
    And 批次工具列應顯示已選筆數

  @current @batch-002
  Scenario: 批次加入 tag
    Given 收藏管理者選取多筆收藏
    When 收藏管理者批次加入一個非空白 tag
    Then 每筆尚未具有該 tag 的收藏應新增關聯
    And 系統應回報實際新增的關聯數

  @current @destructive @batch-003
  Scenario Outline: 批次覆寫支援的欄位
    Given 收藏管理者選取多筆收藏
    When 收藏管理者將 <field> 批次改為 <value>
    Then 所有選取收藏的 <field> 應改為 <value>
    And 系統應回報實際更新筆數

    Examples:
      | field | value    |
      | 原作  | 新原作   |
      | 分類  | 同人誌   |


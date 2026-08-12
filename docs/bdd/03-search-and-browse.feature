Feature: 搜尋與瀏覽收藏
  收藏管理者希望快速找出作品、切換顯示方式，並從結果繼續縮小範圍。

  @current @search-001
  Scenario: 瀏覽全部收藏
    Given 收藏索引中已有資料
    When 收藏管理者未指定搜尋字詞或篩選條件
    Then 系統應回傳全部收藏的第一頁
    And 每頁預設最多顯示 50 筆
    And 結果應包含總筆數與目前頁碼

  @current @search-002
  Scenario Outline: 全文搜尋主要 metadata
    Given 某筆收藏的 <field> 包含搜尋字詞
    When 收藏管理者執行全文搜尋
    Then 該收藏應出現在搜尋結果

    Examples:
      | field  |
      | 檔名   |
      | 標題   |
      | 社團   |
      | 作者   |
      | 原作   |

  @current @search-003
  Scenario: 搜尋字詞含有雙引號
    Given 收藏管理者輸入單獨或內嵌的雙引號
    When 系統建立全文搜尋條件
    Then 搜尋不應因語法錯誤而失敗
    And 無有效搜尋詞時應視為未指定全文條件

  @current @search-004
  Scenario: 組合多個 metadata 篩選
    Given 收藏管理者選定場次、社團、作者、原作、分類或來源中的多個條件
    When 系統搜尋收藏
    Then 結果必須同時符合所有已指定條件

  @current @search-005
  Scenario: 篩選未分類欄位
    Given 收藏管理者選擇某個 metadata 欄位的「未分類」條件
    When 系統搜尋收藏
    Then 結果只應包含該欄位為空白或未設定的收藏

  @current @search-006
  Scenario: 使用多個 tags 篩選
    Given 收藏管理者選擇兩個以上 tags
    When 系統搜尋收藏
    Then 結果只應包含同時具有全部所選 tags 的收藏

  @current @search-007
  Scenario: 限制分頁大小
    Given 呼叫端要求小於 1 或大於 200 的每頁筆數
    When 系統執行搜尋
    Then 系統應將每頁筆數限制在 1 到 200 之間
    And 無效頁碼應回到第一頁

  @current @security @search-008
  Scenario: 不支援的排序欄位不應進入查詢
    Given 呼叫端指定未允許的排序欄位
    When 系統執行搜尋
    Then 系統應改用預設排序欄位
    And 排序方向只能是遞增或遞減

  @current @ui-local @browse-001
  Scenario: 記住列表或對比顯示模式
    Given 收藏管理者切換 Library 的顯示模式
    When 同一瀏覽器再次開啟 Library
    Then 系統應沿用最近選擇的顯示模式

  @current @browse-002
  Scenario: 從收藏 metadata 繼續篩選
    Given 收藏管理者正在查看一筆收藏
    When 收藏管理者點選該收藏的場次、社團、作者或原作
    Then Library 應加入對應篩選條件
    And 搜尋結果應重新從第一頁顯示

  @current @browse-003
  Scenario: 從統計項目返回篩選結果
    Given 統計頁列出某個場次、社團、作者或原作
    When 收藏管理者點選該統計項目
    Then 系統應回到 Library
    And Library 應顯示符合該項目的收藏


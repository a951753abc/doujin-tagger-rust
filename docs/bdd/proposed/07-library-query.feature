@proposed
Feature: Rust v2 的收藏列表、搜尋與詳細資料
  收藏管理者希望透過 localhost API 瀏覽目前有效的收藏，並安全地縮小結果範圍。

  @browse-v2-001
  Scenario: 分頁瀏覽有效收藏
    Given catalog 同時具有有效、tombstone 與軟刪除收藏
    When 呼叫端沒有提供搜尋字詞與分頁參數
    Then API 應只回傳有效收藏的第一頁
    And 每頁預設最多回傳 50 筆
    And response 應包含目前頁碼、每頁筆數、總筆數與總頁數

  @search-v2-001
  Scenario Outline: 搜尋主要 metadata 與檔名
    Given 某筆有效收藏的 <field> 包含搜尋字詞
    When 呼叫端以該字詞搜尋收藏
    Then 該收藏應出現在搜尋結果

    Examples:
      | field  |
      | 檔名   |
      | 標題   |
      | 社團   |
      | 作者   |
      | 原作   |

  @security @search-v2-002
  Scenario: 全文搜尋不直接執行呼叫端提供的 FTS 語法
    Given 搜尋字詞含有單獨或內嵌的雙引號與控制字元
    When API 建立全文搜尋條件
    Then 查詢不應因 FTS 語法錯誤而失敗
    And 沒有有效搜尋詞時應視為沒有全文搜尋條件

  @search-v2-003
  Scenario: 正規化分頁範圍
    Given 呼叫端提供零、負數或超出上限的分頁參數
    When API 查詢收藏
    Then 無效頁碼應回到第一頁
    And 每頁筆數應限制在 1 到 200 之間

  @security @search-v2-004
  Scenario: 忽略尚未支援的排序輸入
    Given 呼叫端提供不支援的排序欄位或方向
    When API 查詢收藏
    Then 該輸入不應拼接到 SQL
    And 結果應使用預設的 collection ID 反向排序

  @browse-v2-002
  Scenario: 讀取單筆有效收藏詳細資料
    Given catalog 中具有一筆有效收藏
    When 呼叫端依 collection ID 讀取詳細資料
    Then API 應回傳目前路徑、檔名、來源、effective metadata、tags 與時間戳記
    And 不存在、tombstone 或軟刪除的 collection ID 應回傳找不到收藏

  @search-v2-005
  Scenario: 組合 metadata 與來源篩選
    Given 收藏管理者指定場次、社團、作者、原作、分類、子分類或來源中的多個條件
    When API 查詢收藏
    Then 結果必須同時符合所有已指定條件
    And 所有 filter values 應以 SQLite bind parameters 傳入

  @search-v2-006
  Scenario: 同時篩選多個未分類欄位
    Given 收藏管理者指定一個或多個 missing metadata 欄位
    When API 查詢收藏
    Then 結果必須在每個指定欄位都沒有有效值
    And 空作者清單應視為 authors 未分類

  @search-v2-007
  Scenario: 同時篩選多個 tags
    Given 收藏管理者指定兩個以上的 tag 名稱
    When API 查詢收藏
    Then 結果必須同時具有全部指定 tags
    And 只有部分 tags 相符的收藏不應出現在結果中

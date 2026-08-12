Feature: 外部補標、重複偵測與名稱合併
  收藏管理者希望取得 metadata 建議並整理名稱變體，
  但外部或推斷結果不應在未確認時破壞既有資料。

  @current @external @enrich-001
  Scenario: 依 RJ 編號優先查詢 DLsite
    Given 收藏檔名包含有效 RJ 編號
    When 收藏管理者要求 Web 搜尋建議
    Then 系統應優先使用 RJ 編號查詢產品資料
    And 成功取得的直接匹配應具有高信心度

  @current @external @enrich-002
  Scenario: 以社團與標題搜尋 DLsite
    Given 收藏沒有可用的 RJ 直接匹配
    And 收藏至少具有社團或標題
    When 收藏管理者要求 Web 搜尋建議
    Then 系統應以可用的社團與標題搜尋候選產品
    And 系統最多應進一步檢查前兩筆搜尋結果

  @current @external @enrich-003
  Scenario: 以模式與同社團資料產生本地建議
    Given 收藏符合 CG 關鍵字或同社團已有足夠的已知原作
    When 系統建立 metadata 建議
    Then 系統可以產生不需網路的原作建議
    And 建議應包含來源、信心度與可用的理由

  @current @external @enrich-004
  Scenario: 依信心度排列多個建議
    Given 系統從本地推斷、DLsite 或一般 Web 搜尋取得多個建議
    When 系統顯示建議
    Then 建議應由信心度高到低排列
    And 每筆建議應顯示可辨識的來源

  @current @external @enrich-005
  Scenario: 外部搜尋失敗時保留目前 metadata
    Given 外部網站逾時、拒絕要求或頁面無法解析
    When 收藏管理者要求 Web 搜尋建議
    Then 系統不應因搜尋失敗自動清空或修改目前 metadata
    And UI 應顯示沒有建議或搜尋失敗

  @current @needs-confirmation @enrich-006
  Scenario: UI 套用建議時原作可覆寫非空白值
    Given 編輯視窗中的原作已有值
    And 一筆建議包含不同原作
    When 收藏管理者在 UI 按下套用建議
    Then UI 應以建議原作取代目前原作
    And 其他已有值的主要欄位不應被建議覆寫

  @current @needs-confirmation @enrich-007
  Scenario: 建議 API 只補上空白欄位
    Given 收藏已有部分 metadata
    And 已選建議同時包含空白欄位與已有值欄位
    When 呼叫端要求直接套用建議
    Then 系統只應更新目前為空白的場次、社團、作者或原作
    And 系統不應更新已有值或標題

  @current @duplicate-001
  Scenario Outline: 對支援的欄位偵測名稱變體
    Given 多筆收藏在 <field> 中使用不同文字表示相同名稱
    When 收藏管理者要求偵測重複
    Then 系統應以 Unicode、假名、空白、標點與 romaji 正規化結果建立候選群組

    Examples:
      | field |
      | 場次  |
      | 社團  |
      | 作者  |
      | 原作  |

  @current @needs-confirmation @duplicate-002
  Scenario: 自動推薦群組的正式名稱
    Given 一個重複群組包含日文與非日文變體
    When 系統選擇預設正式名稱
    Then 系統應優先推薦日文變體
    And 同類型變體中應優先推薦使用次數較多者

  @current @destructive @duplicate-003
  Scenario: 合併名稱變體
    Given 收藏管理者選擇正式名稱
    And 群組中存在其他名稱變體
    When 收藏管理者執行合併
    Then 所有使用其他變體的收藏應改為正式名稱
    And 系統應回報更新筆數

  @current @duplicate-004
  Scenario: 跳過不應合併的候選群組
    Given 系統顯示一組可能重複的名稱
    When 收藏管理者判定它們不是同一名稱並選擇跳過
    Then 系統不應修改任何收藏


Feature: 設定、縮圖與收藏統計
  收藏管理者希望調整本機設定、快速瀏覽封面並了解收藏分布。

  @current @settings-001
  Scenario: 新增與停用掃描來源
    Given 收藏管理者位於設定頁
    When 收藏管理者新增掃描來源
    Then 系統應保存來源路徑及其「歸檔區」或「下載區」類型
    And 設定變更後應提示需要重新掃描

    When 收藏管理者停用該掃描來源
    Then 後續掃描不應再進入該來源
    And 系統應保留來源設定與既有收藏資料

  @current @settings-001a
  Scenario: 再次新增同一路徑會更新並重新啟用來源
    Given 一個掃描來源目前已停用
    When 收藏管理者以相同路徑再次新增掃描來源
    Then 系統應沿用原本的來源身分
    And 系統應更新來源類型與標籤
    And 後續掃描應再次進入該來源

  @current @settings-001b
  Scenario: 拒絕無效的掃描來源設定
    Given 收藏管理者輸入相對路徑、不存在的資料夾或不支援的來源類型
    When 收藏管理者儲存掃描來源
    Then 系統不應寫入該設定
    And 系統應回傳可辨識的輸入錯誤

  @current @settings-002
  Scenario: 設定值的來源優先順序
    Given 同一啟動設定同時存在環境變數、設定檔與預設值
    When 系統啟動並讀取設定
    Then 環境變數應優先於設定檔
    And 設定檔應優先於預設值

  @current @settings-003
  Scenario Outline: 驗證縮圖設定
    Given 收藏管理者輸入 <size> 與 <quality>
    When 收藏管理者儲存縮圖設定
    Then 系統應回傳 <result>

    Examples:
      | size      | quality | result             |
      | "300x400" | 80      | 儲存成功           |
      | "300*400" | 80      | 尺寸格式錯誤       |
      | "300x400" | 0       | 品質範圍錯誤       |
      | "300x400" | 101     | 品質範圍錯誤       |

  @current @needs-confirmation @settings-004
  Scenario: 修改縮圖設定不會立即重建既有縮圖
    Given 收藏已經具有縮圖快取
    When 收藏管理者修改縮圖尺寸或品質
    Then 新設定應套用於後續產生的縮圖
    And 既有縮圖應保持不變直到收藏管理者清除快取

  @current @thumbnail-001
  Scenario: 首次要求尚未產生的縮圖
    Given 收藏檔案存在但沒有縮圖快取
    When Library 要求該收藏的縮圖
    Then 系統應將縮圖生成工作放入背景佇列
    And 系統應立即回傳不可快取的透明 placeholder

  @current @thumbnail-002
  Scenario: 從 ZIP 或圖片資料夾產生縮圖
    Given 收藏是 ZIP 或圖片資料夾
    And 收藏內至少有一張支援的圖片
    When 背景工作產生縮圖
    Then 系統應依自然排序選擇第一張圖片
    And 圖片應縮放到設定尺寸內並保存為 WebP 快取

  @current @thumbnail-003
  Scenario: 避免重複排入相同縮圖工作
    Given 某筆收藏的縮圖工作正在執行
    When Library 再次要求同一筆縮圖
    Then 系統不應建立第二個相同背景工作

  @current @needs-confirmation @thumbnail-004
  Scenario: 失敗的縮圖不會自動重試
    Given 某筆收藏的縮圖生成曾經失敗並留下失敗標記
    When Library 再次要求同一筆縮圖
    Then 系統不應重新嘗試產生縮圖
    And 收藏管理者清除縮圖快取後才可以再次嘗試

  @current @destructive @thumbnail-005
  Scenario: 清除所有縮圖快取
    Given 收藏管理者已確認清除縮圖快取
    When 系統執行清除
    Then 所有 WebP 快取與失敗標記應被移除
    And 實際收藏檔案不應被修改
    And 系統應回報清除數量

  @current @thumbnail-006
  Scenario: 封面候選包含各子資料夾的第一張
    Given 收藏內的圖片分布在多個子資料夾
    When 系統列出候選封面
    Then 候選應包含自然排序前段的圖片
    And 也應包含每個第一層子資料夾的第一張圖片
    And 自動縮圖仍使用整體自然排序的第一張

  @current @stats-001
  Scenario: 顯示收藏統計摘要
    Given 收藏索引中已有資料
    When 收藏管理者開啟統計頁
    Then 系統應顯示收藏總數與至少具有一個 tag 的收藏數
    And 系統應顯示各分類數量
    And 系統應顯示原作、作者、社團與場次的常用項目

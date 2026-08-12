@proposed @rust-v2
Feature: Rust v2 的本機 UI、縮圖與安全邊界
  收藏管理者希望保留本機使用體驗，
  並讓縮圖失敗、設定變更與網路監聽具有明確且安全的行為。

  @dec-017 @ui-local @recent-v2-001
  Scenario: 最近開啟紀錄只屬於目前瀏覽器
    Given 收藏管理者在一個瀏覽器閱讀收藏並產生最近開啟紀錄
    When 收藏管理者以另一個瀏覽器或裝置開啟 Library
    Then 第二個瀏覽器不應自動取得第一個瀏覽器的最近開啟紀錄
    And 伺服器不應將該紀錄保存為跨瀏覽器共享狀態

  @dec-018 @thumbnail-v2-001
  Scenario: 暫時性縮圖錯誤安排延遲重試
    Given 縮圖生成因暫時性檔案鎖定、逾時或暫時無法讀取而失敗
    When 系統記錄這次失敗
    Then 系統應保存錯誤種類與失敗時間
    And 系統應安排一個未來的重試時間
    And 到達重試時間前不應重複執行相同工作

  @dec-018 @thumbnail-v2-002
  Scenario: 連續暫時性失敗增加等待時間
    Given 同一筆縮圖工作已因暫時性錯誤連續失敗
    When 系統安排下一次自動重試
    Then 下一次等待時間應比前一次更長
    And 系統應保存嘗試次數與下一次重試時間

  @dec-018 @thumbnail-v2-003
  Scenario: 永久性縮圖錯誤等待明確觸發才重試
    Given 縮圖生成因不支援格式或確定損壞而失敗
    When 系統將錯誤分類為永久性錯誤
    Then 系統不應安排定時自動重試
    And 收藏檔案變更或收藏管理者手動要求後才可再次嘗試

  @dec-019 @security @boundary-v2-001
  Scenario Outline: 服務只監聽 loopback 位址
    Given Rust v2 服務準備啟動
    When 系統監聽 <address>
    Then Library 應只能由同一台主機連線
    And 區域網路中的其他裝置不應能直接連線

    Examples:
      | address     |
      | "127.0.0.1" |
      | "::1"       |

  @dec-019 @security @boundary-v2-002
  Scenario: 拒絕設定非 loopback 監聽位址
    Given 啟動設定要求服務監聽 "0.0.0.0" 或區域網路介面位址
    When Rust v2 服務驗證監聽設定
    Then 服務不應以該位址啟動
    And 系統應回報只允許 localhost loopback

  @dec-020 @external @file-v2-007
  Scenario Outline: 保留兩種收藏開啟動作
    Given 收藏檔案存在於允許的掃描來源
    And 所需的外部程式設定有效
    When 收藏管理者選擇 <action>
    Then 系統應使用 <handler> 開啟收藏

    Examples:
      | action           | handler              |
      | 系統預設開啟     | 作業系統預設程式     |
      | 指定閱讀器閱讀   | 收藏管理者指定的閱讀器 |

  @dec-021 @thumbnail-v2-004
  Scenario: 縮圖設定變更後自動排程重建
    Given 既有收藏已具有依舊設定產生的縮圖
    When 收藏管理者儲存新的縮圖尺寸或品質
    Then 既有縮圖應標記為需要重建
    And 系統應自動排程以新設定重建縮圖
    And 實際收藏檔案不應被修改

  @dec-021 @thumbnail-v2-005
  Scenario: 收藏管理者手動要求重建縮圖
    Given Library 中存在縮圖快取或縮圖失敗紀錄
    When 收藏管理者手動要求重建縮圖
    Then 系統應使指定範圍的現有縮圖失效並重新排程
    And 永久性錯誤的失敗紀錄可以因此重新嘗試
    And 實際收藏檔案不應被修改

  @dec-039 @thumbnail-v2-006
  Scenario: 背景縮圖完成後自動替換 placeholder
    Given 可見收藏尚未具有縮圖快取
    When Library 首次要求縮圖並收到 pending 或 running placeholder
    Then Library 應自動追蹤同一收藏的縮圖狀態
    When 縮圖生成完成並可取得 ready WebP
    Then Library 應在不重新整理頁面的情況下顯示真正縮圖
    And Library 應停止追蹤該縮圖

  @dec-039 @thumbnail-v2-007
  Scenario: 同一收藏的可見縮圖共用追蹤工作
    Given 清單與詳細資料同時顯示同一筆尚未 ready 的收藏
    When 兩處都需要顯示該收藏的縮圖
    Then Library 應共用同一份縮圖追蹤工作
    And ready 後兩處都應顯示同一張真正縮圖

  @dec-039 @thumbnail-v2-008
  Scenario: 過時的縮圖結果不得覆寫新選取收藏
    Given 詳細資料正在等待收藏 A 的縮圖
    When 收藏管理者在縮圖完成前改為選取收藏 B
    And 收藏 A 的縮圖稍後完成
    Then 收藏 A 的結果不得寫入收藏 B 的詳細資料
    And 不再被畫面使用的追蹤工作應停止

  @dec-039 @thumbnail-v2-009
  Scenario Outline: 前端依縮圖失敗種類決定是否繼續追蹤
    Given Library 正在追蹤一筆縮圖
    When 後端回傳 <failure> 並提供 <next_retry>
    Then Library 應採取 <behavior>
    And 不得形成無限密集的重新要求

    Examples:
      | failure  | next_retry | behavior                         |
      | 暫時性失敗 | 未來時間   | 到達指定時間後自動繼續追蹤       |
      | 永久性失敗 | 未設定     | 停止自動追蹤並保留手動重建入口   |

  @dec-022 @metadata-v2-014
  Scenario: 清空手動值後由較低優先來源補回
    Given 某欄位目前具有手動值
    And 該欄位仍有外部 metadata、檔名解析或推斷候選
    When 收藏管理者清空該手動值
    Then 系統不應保存會阻擋其他來源的手動空白值
    And 最高優先的剩餘候選應成為有效值

  @dec-022 @metadata-v2-015
  Scenario: 清空手動值且沒有其他候選時保持空白
    Given 某欄位目前只有手動值
    And 該欄位沒有任何較低優先來源候選
    When 收藏管理者清空該手動值
    Then 欄位應保持空白
    And 系統不應建立額外的推斷值

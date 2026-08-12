@proposed @external
Feature: Rust v2 的外部 metadata 搜尋工作
  收藏管理者希望以可追蹤背景工作補足資料，並讓部分成功與重試行為可以理解。

  @external-search-v2-001
  Scenario: 為 active 收藏建立外部搜尋工作
    Given 一筆 active 收藏與至少一個 allowlisted metadata field
    When 收藏管理者要求外部搜尋
    Then 系統應建立 pending 工作並保存收藏、欄位與建立時間
    And API 應回傳工作 ID 與目前狀態

  @external-search-v2-002
  Scenario: 避免同一收藏重複執行外部搜尋
    Given 某筆收藏已有 pending 或 running 外部搜尋工作
    When Library 再次要求該收藏的外部搜尋
    Then 系統不應建立第二個 active 工作
    And API 應回傳既有工作並標示未新建

  @security @external-search-v2-003
  Scenario: 只接受 active 收藏與 allowlisted fields
    Given 收藏不存在、不是 active，或 request 包含未知欄位或空欄位清單
    When 呼叫端要求建立外部搜尋工作
    Then API 應回傳結構化錯誤
    And 系統不應建立背景工作

  @dec-015 @external-search-v2-004
  Scenario: 外部搜尋可以逐欄位部分成功
    Given provider 對部分欄位回傳有效候選並對其他欄位回報錯誤
    When 系統保存 provider response
    Then 有效候選應各自依 confidence 規則保存或套用
    And 失敗欄位不應回滾已成功欄位
    And 工作狀態應為 partial 並逐欄位保存 issue

  @dec-015 @external-search-v2-005
  Scenario: 搜尋結果依 confidence 分流
    Given provider 同時回傳高、中、低信心欄位結果
    When 系統處理搜尋結果
    Then 符合自動條件的結果可以 auto-apply
    And 中等信心結果應保存為 suggestion
    And 低信心結果應只保存為 search-only
    And 工作 summary 應分別計數

  @external-search-v2-006
  Scenario Outline: 暫時性錯誤依種類與嘗試次數安排重試
    Given running 外部搜尋工作第 <attempts> 次遇到 <error_kind>
    When 系統記錄失敗
    Then 工作應回到 pending
    And 系統應依錯誤種類與嘗試次數增加等待時間
    And 到達 next retry time 前不得再次執行

    Examples:
      | error_kind          | attempts |
      | network             | 1        |
      | rate_limited        | 1        |
      | provider_unavailable| 2        |

  @external-search-v2-007
  Scenario Outline: 永久性錯誤不安排定時重試
    Given running 外部搜尋工作遇到 <error_kind>
    When 系統記錄失敗
    Then 工作狀態應為 failed
    And next retry time 應保持未設定

    Examples:
      | error_kind      |
      | invalid_response|
      | no_match        |
      | unsupported     |

  @external-search-v2-008
  Scenario: 查詢持久化工作狀態
    Given 一筆外部搜尋工作處於 pending、running、succeeded、partial 或 failed
    When Library 依工作 ID 查詢
    Then API 應回傳欄位、嘗試次數、結果 summary、錯誤種類與下一次重試時間
    And 不存在或無效的工作 ID 應回傳結構化錯誤

  @external-search-v2-009
  Scenario: Worker 以有限批次處理所有已到期工作
    Given 佇列同時具有已到期、尚未到期與不同收藏的外部搜尋工作
    When worker 以指定 batch limit 領取工作
    Then worker 應只領取 batch limit 內已到期的 pending 工作
    And 每筆工作應獨立成為 succeeded、partial、待重試或 failed
    And 單筆 provider 結果不得阻止後續工作
    And worker 應回報各狀態數量與基礎設施 issues

  @external-search-v2-010
  Scenario: 程式重啟時復原中斷工作
    Given 前一個程序停止時仍有 running 外部搜尋工作
    When 新程序啟動並執行工作復原
    Then running 工作應回到立即可執行的 pending
    And 已累計的 attempts 應保持不變
    And error kind 應記錄為 worker_interrupted
    And 重複執行復原不應再次修改同一工作

  @dec-041 @external-search-v2-011
  Scenario: 重複搜尋到完全相同的外部候選時重用 assertion
    Given 同一收藏與欄位已有相同值、來源參照及 confidence 證據的外部 assertion
    When 後續外部搜尋工作再次回傳完全相同的候選
    Then 系統應重用既有 assertion 而不是建立重複候選
    And 新一次外部搜尋紀錄仍應保存並指向該 assertion
    And 既有的採用或拒絕裁決不得被重複搜尋推翻
    And metadata 證據畫面應只顯示一筆可裁決候選

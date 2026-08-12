@proposed @external
Feature: Rust v2 的 DLsite RJ 優先與書名搜尋 provider
  收藏管理者希望以產品識別碼取得可追溯的外部 metadata，
  並避免搜尋排名、一般 genre 或不完整 response 造成誤判。

  @dec-014 @dec-015 @dlsite-v2-001
  Scenario: 只有一個 RJ 識別碼時執行精確 lookup
    Given 最新 parser 證據恰好包含一個不同的 RJ 識別碼
    When DLsite provider 處理外部 metadata 工作
    Then provider 應只依該 RJ 查詢單一產品
    And response 的產品識別碼必須與 request RJ 完全相同
    And 候選應保存可開啟的 DLsite 商品頁作為來源參照

  @dec-040 @dlsite-v2-002
  Scenario: 沒有 RJ 但有辨識書名時搜尋唯一完全相符作品
    Given 最新 parser 證據沒有 RJ 識別碼
    And 收藏具有非空白的辨識書名
    When DLsite provider 處理外部 metadata 工作
    Then provider 應以該書名或分隔符前的穩定核心書名查詢 DLsite 搜尋頁
    And 只有搜尋結果恰好一筆與辨識書名正規化後完全相符時才應查詢該產品
    And 書名匹配候選不得標記為可靠識別碼完全匹配
    And 候選只能成為待確認建議或搜尋紀錄，不得自動套用

  @dec-040 @dlsite-v2-015
  Scenario: DLsite 書名搜尋路徑保留必要的尾斜線
    Given 收藏的辨識書名是「とある村の筆下ろし事情」
    And 收藏沒有可用的 typed RJ 識別碼
    When provider 建立 DLsite 書名搜尋 request
    Then URL 應以 percent-encoded 書名加上尾斜線結尾
    And DLsite 不應忽略關鍵字而回傳預設熱門作品
    And 唯一完全相符結果應取得 RJ160339

  @dlsite-v2-003
  Scenario: 多個不同 RJ 保持歧義而不自動選擇
    Given 最新 parser 證據包含兩個或以上不同的 RJ 識別碼
    When DLsite provider 處理外部 metadata 工作
    Then provider 不應送出 HTTP request
    And 所有 RJ 證據都應保持可追溯
    And 系統應回報識別碼有歧義而不得自動套用 metadata

  @dec-015 @dlsite-v2-004
  Scenario: 精確產品回應只映射有直接證據的欄位
    Given DLsite 回應包含 work name、maker name、authors、creators、work options 與 genres
    When provider 建立請求欄位的 metadata 候選
    Then work name 可以映射為標題
    And maker name 可以映射為社團
    And 明確的 author 或 created-by role 可以映射為作者清單
    And 明確的活動 option 可以映射為場次
    And voice、illustration 或 scenario role 不應混入作者

  @dec-002 @dlsite-v2-005
  Scenario: 只有明確 Original Work option 才建立原作候選
    Given DLsite 回應同時包含一般 genres 與 work options
    When provider 評估原作欄位
    Then 明確的 Original Work option 可以建立 canonical 為 "オリジナル" 的候選
    And provider 不應從其他 genre 猜測原作名稱
    And provider 不應因缺少 Original Work option 就推斷其他原作

  @dlsite-v2-006
  Scenario: 不以 DLsite 商品屬性推斷本機分類與 DL 狀態
    Given DLsite 回應包含 site、work category 與 work type
    When provider 評估 classification 與 is_dl
    Then provider 不應在尚無已確認映射規則時建立 classification 候選
    And provider 不應只因商品存在於 DLsite 就將本機收藏標記為 DL 版

  @dec-015 @dlsite-v2-007
  Scenario: 單一 optional 欄位損壞時仍可部分成功
    Given DLsite response 的產品識別碼、標題與社團有效
    And 作者欄位具有無法解讀的型別
    When provider 逐欄位解析 response
    Then 標題與社團候選仍應保存
    And 作者欄位應記錄 invalid-response issue
    And 外部搜尋工作應依既有規則完成為 partial

  @dlsite-v2-008
  Scenario Outline: HTTP 與 response 錯誤轉為 typed provider error
    Given DLsite exact request 遇到 <condition>
    When provider 分類失敗結果
    Then error kind 應為 <error_kind>
    And 是否重試應為 <retry>

    Examples:
      | condition                    | error_kind           | retry |
      | timeout 或連線中斷          | network              | 是    |
      | HTTP 429                     | rate_limited         | 是    |
      | HTTP 5xx                     | provider_unavailable | 是    |
      | 空陣列、HTTP 404 或 ID 不同  | no_match             | 否    |
      | 無法解讀 response 根結構     | invalid_response     | 否    |

  @dec-014 @dec-015 @dlsite-v2-009
  Scenario: 精確 RJ 候選保留高信心證據
    Given request RJ 與 DLsite response 產品識別碼完全相同
    And 一個候選欄位通過型別與內容驗證
    When provider 建立 confidence evidence
    Then reliable identifier exact match 應為 true
    And 總信心度應至少為 0.95
    And 理由應包含 RJ、provider 欄位與完全匹配說明
    And 是否套用仍應由手動衝突與 metadata 優先序決定

  @external @dlsite-v2-010
  Scenario: 多筆工作遵守 DLsite 公開的保守請求間隔
    Given worker 連續處理多筆需要 DLsite exact lookup 的工作
    When provider 對同一 host 送出 request
    Then 同一時間最多只能有一個 in-flight request
    And 相鄰 request 的開始時間預設至少間隔 10 秒
    And 暫時性錯誤不得形成密集重試迴圈

  @dec-040 @dlsite-v2-011
  Scenario: 沒有 RJ 也沒有辨識書名時不送出搜尋要求
    Given 最新 parser 證據沒有 RJ 識別碼
    And 收藏沒有非空白的辨識書名
    When DLsite provider 處理外部 metadata 工作
    Then provider 不應送出 HTTP request
    And 結果應標記為 unsupported

  @dec-040 @dlsite-v2-012
  Scenario: 多筆搜尋結果具有相同書名時保持歧義
    Given 最新 parser 證據沒有 RJ 識別碼
    And DLsite 搜尋結果有兩筆以上與辨識書名正規化後完全相符的作品
    When DLsite provider 處理外部 metadata 工作
    Then provider 不應依搜尋排名自動選擇產品
    And 結果應標記為 no_match 並說明有多筆相同書名

  @dec-043 @dlsite-v2-013
  Scenario: DLsite 成功匹配但沒有活動 option 時以 DL 補場次
    Given 收藏的場次目前空白
    And DLsite provider 已以唯一 RJ 或唯一完全相符書名確認商品
    And 商品 response 沒有明確的活動 option
    When provider 建立場次候選
    Then 場次候選應為 "DL"
    And 來源參照應為已確認的 DLsite 商品頁
    And 唯一 RJ 匹配可依 DEC-015 評估自動套用
    But 書名匹配仍只能成為待確認建議
    And provider 不應因此修改 `is_dl`

  @dec-043 @dlsite-v2-014
  Scenario: 空的 work_options 陣列代表商品沒有 option
    Given DLsite 已唯一匹配商品
    And product API 的 work_options 是空陣列
    When provider 建立場次與原作候選
    Then 空陣列不應被視為 invalid_response
    And 場次應建立 "DL" fallback 候選
    And 原作應保持空白而不得從一般 genre 推斷

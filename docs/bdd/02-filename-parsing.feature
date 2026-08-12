Feature: 從檔名解析收藏 metadata
  收藏管理者希望系統從常見同人誌與商業誌檔名取得 metadata，
  同時保留無法可靠判定的資訊。

  @current @parse-001
  Scenario: 解析完整的同人誌檔名
    Given 檔名為 "(C106) [20NT (ふけまち)] プラナちゃん催眠のお時間です (ブルーアーカイブ) [DL版].zip"
    When 系統解析檔名
    Then 場次應為 "C106"
    And 社團應為 "20NT"
    And 作者應為 "ふけまち"
    And 標題應為 "プラナちゃん催眠のお時間です"
    And 原作應為 "ブルーアーカイブ"
    And 應標記為 DL 版

  @current @parse-002
  Scenario: 解析沒有場次的同人誌檔名
    Given 檔名為 "[Hなほん。やさん。(あっきー)] 妊娠ライブ! (ラブライブ!) [DL版].zip"
    When 系統解析檔名
    Then 場次應保持未分類
    And 社團應為 "Hなほん。やさん。"
    And 作者應為 "あっきー"
    And 標題應為 "妊娠ライブ!"
    And 原作應為 "ラブライブ!"

  @current @parse-003
  Scenario: 方括號只有社團名稱
    Given 同人誌檔名以 "[社團名稱]" 開頭
    And 方括號內容不符合「社團 (作者)」格式
    When 系統解析檔名
    Then 方括號內容應成為社團
    And 作者應保持未分類

  @current @parse-004
  Scenario: 商業誌方括號代表作者
    Given 檔名以已知商業誌分類前綴開頭
    And 分類前綴後為 "[作者名稱]"
    When 系統解析檔名
    Then 方括號內容應成為作者
    And 社團應保持未分類
    And 系統應保留偵測到的商業誌分類

  @current @parse-005
  Scenario Outline: 忽略不屬於主要 metadata 的標記
    Given 檔名包含 <marker>
    When 系統解析檔名
    Then <marker> 不應成為場次、社團、作者、標題或原作

    Examples:
      | marker                |
      | "[DL版]"             |
      | "[Digital]"          |
      | "[Chinese]"          |
      | "[2014-04-30]"       |
      | "(別スキャン)"       |
      | "(修正版)"           |
      | "(Full HQ Scan)"     |
      | "[DLsite限定特典付き]" |

  @current @parse-006
  Scenario: 忽略日期與 RJ 編號前綴
    Given 作品資訊前有日期方括號或 RJ 編號方括號
    When 系統解析檔名
    Then 日期或 RJ 編號不應被當成社團
    And 系統應繼續解析後續的社團、作者與標題

  @current @parse-007
  Scenario: 支援全形與巢狀圓括號
    Given 標題尾端的原作使用全形圓括號或包含平衡的巢狀圓括號
    When 系統解析檔名
    Then 系統應從最尾端的完整括號區段取得原作候選
    And 括號以前的文字應成為標題

  @current @parse-008
  Scenario: 已知版本或媒體說明不應成為原作
    Given 檔名尾端括號內容為已知版本、語言、媒體格式或合集說明
    When 系統解析檔名
    Then 該括號內容不應成為原作
    And 系統應繼續檢查更前面的尾端括號

  @current @parse-009
  Scenario: 正規化已知原作別名
    Given 從檔名取得的原作存在於已知別名表
    When 系統完成解析
    Then 原作應轉換為設定的正式名稱

  @current @parse-010
  Scenario: 無法辨識結構時保留可搜尋標題
    Given 檔名不符合任何已知結構
    When 系統解析檔名
    Then 系統至少應產生非空白標題
    And 不能可靠判定的其他欄位應保持未分類

  @current @needs-confirmation @parse-011
  Scenario: 任意開頭圓括號被當成場次
    Given 非商業誌檔名以最長 50 字元的圓括號內容開頭
    And 該內容不一定是已知場次
    When 系統解析檔名
    Then 系統仍將該內容當成場次

  @current @needs-confirmation @parse-012
  Scenario: 合法的尾端圓括號預設被當成原作
    Given 檔名尾端有一個未命中排除規則的圓括號區段
    And 系統沒有外部證據證明該內容是原作
    When 系統解析檔名
    Then 系統仍將該內容當成原作

  @current @needs-confirmation @parse-013
  Scenario: 商業誌分類名稱在 parser 與 Library 不一致
    Given parser 偵測到 "成年コミック"、"官能小説" 或 "一般コミック"
    When 收藏顯示在只提供「商業誌」分類的 Library
    Then 該收藏可能無法以單一「商業誌」分類一致顯示與篩選


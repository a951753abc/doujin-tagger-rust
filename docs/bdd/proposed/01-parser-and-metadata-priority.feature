@proposed @rust-v2
Feature: Rust v2 的檔名解析與 metadata 來源優先序
  收藏管理者希望 parser 對不確定資訊採取可追溯且可修正的處理，
  並確保較可信的 metadata 來源不會被較低優先來源覆蓋。

  @dec-001 @parser-v2-001
  Scenario: 將一般開頭圓括號視為場次
    Given 新收藏的檔名以 "(某場次)" 開頭
    And 該內容不是已辨識的分類前綴
    When Rust v2 parser 解析檔名
    Then 場次應為 "某場次"
    And 該區段不應被當成標題、原作或其他資訊

  @dec-001 @dec-004 @parser-v2-002
  Scenario: 已辨識的商業誌分類前綴優先於場次
    Given 新收藏的檔名以 "(成年コミック)" 開頭
    When Rust v2 parser 解析檔名
    Then 上層分類應為 "商業誌"
    And 商業誌子分類應為 "成年コミック"
    And 場次應保持未分類

  @dec-002 @parser-v2-003
  Scenario: 無證據的尾端圓括號歸入其他資訊
    Given 新收藏的檔名為 "[社團] 作品名稱 (角色名稱).zip"
    And parser 沒有足夠證據判定 "角色名稱" 是原作
    When Rust v2 parser 解析檔名
    Then 原作應保持未分類
    And 標題應為 "作品名稱"
    And "角色名稱" 應保存為其他資訊
    And 解析結果應保留該判斷的來源區段

  @dec-002 @parser-v2-004
  Scenario: 有足夠證據時將尾端圓括號判定為原作
    Given 新收藏的檔名尾端包含原作候選
    And 原作字典、外部識別碼或其他已確認規則提供足夠證據
    When Rust v2 parser 解析檔名
    Then 該候選可以成為原作解析值
    And 解析結果應記錄採用它的證據

  @dec-004 @parser-v2-005
  Scenario Outline: 商業誌子分類同時歸入商業誌
    Given 新收藏具有商業誌子分類 <subcategory>
    When 系統建立收藏分類
    Then 上層分類應為 "商業誌"
    And 商業誌子分類應為 <subcategory>

    Examples:
      | subcategory     |
      | "成年コミック" |
      | "官能小説"     |
      | "一般コミック" |

  @dec-005 @parser-v2-006
  Scenario: 同時保存原作原始值與 canonical 正式名稱
    Given 新收藏的檔名原作區段為 "ポケモン"
    And canonical mapping 將 "ポケモン" 對應為 "ポケットモンスター"
    When Rust v2 parser 建立原作 metadata
    Then 原作的檔名解析值應保存為 "ポケモン"
    And 原作的 canonical 正式名稱應保存為 "ポケットモンスター"
    And 系統應能追溯正式名稱所使用的 mapping

  @dec-006 @parser-v2-007
  Scenario: 自動掃描只解析新收藏
    Given 收藏索引中不存在某個新發現的路徑
    When 系統掃描到該收藏
    Then 系統應以目前 parser 版本解析該收藏
    And 解析結果應記錄使用的 parser 版本

  @dec-006 @parser-v2-008
  Scenario: Parser 更新不自動改寫既有收藏
    Given 一筆收藏已由舊版 parser 建立 metadata
    And 系統已安裝新版 parser
    When 系統再次掃描相同收藏路徑
    Then 既有收藏不應自動重新解析
    And 目前有效 metadata 不應因 parser 更新而改變

  @dec-007 @metadata-v2-001
  Scenario Outline: 依來源優先序選擇有效 metadata
    Given 同一欄位同時具有 <higher_source> 與 <lower_source> 的不同值
    When 系統決定目前有效值
    Then 應採用 <higher_source> 的值
    And 應保留兩個候選值及其來源以供追溯

    Examples:
      | higher_source | lower_source |
      | 手動修改      | 外部 metadata |
      | 外部 metadata | 檔名解析     |
      | 檔名解析      | 推斷結果     |

  @dec-007 @metadata-v2-002
  Scenario: 低優先來源不得覆寫手動修改
    Given 收藏管理者已手動修改某個 metadata 欄位
    And 外部 metadata、檔名 parser 或推斷引擎提出不同值
    When 系統重新計算目前有效 metadata
    Then 手動修改值應保持有效
    And 其他值只能作為可檢視的候選

  @dec-007 @metadata-v2-003
  Scenario: 推斷結果只在沒有更高優先資料時生效
    Given 某個欄位沒有手動修改、外部 metadata 或檔名解析值
    And 推斷引擎提出一個值
    When 系統決定目前有效值
    Then 推斷結果可以成為目前有效值
    And 該值應明確標記為推斷結果

  @dec-023 @parser-v2-016 @filesystem
  Scenario: Percent decoding 成功解析後重新命名新收藏
    Given 新發現 ZIP 的檔名含有合法的 UTF-8 percent encoding
    And 解碼後可成功解析出場次、分類或創作者結構
    And 解碼後名稱是同目錄內安全且不存在的單一檔名
    When 系統處理這筆新收藏
    Then 系統應將實體 ZIP 重新命名為解碼後的檔名
    And 系統應在重新命名成功後才以新路徑建立收藏索引
    And 重新命名紀錄應保留原始檔名與新檔名

  @dec-023 @parser-v2-017 @filesystem
  Scenario Outline: 不安全或衝突的解碼名稱不得覆寫檔案
    Given 新發現 ZIP 的檔名可以 percent decode
    And 解碼後名稱 <condition>
    When 系統嘗試正規化新收藏檔名
    Then 原始 ZIP 應保持原名
    And 系統不得覆寫任何既有檔案
    And 結果應回報需要處理的原因

    Examples:
      | condition |
      | "包含路徑分隔符或 Windows 禁用字元" |
      | "是 Windows 保留名稱" |
      | "與同目錄既有檔名衝突" |
      | "無法成功解析收藏結構" |

  @dec-024 @parser-v2-018
  Scenario: 有後續收藏結構的來源前綴不當成社團
    Given 新收藏檔名以 "[firelee@2DJGAME]" 開頭
    And 後方仍有可辨識的場次與社團結構
    When Rust v2 parser 解析檔名
    Then 該前綴應保存為 kind 為 "source_marker" 的 ignored segment
    And 該前綴不應成為社團
    And parser 應繼續解析後方的場次、社團、作者與標題

  @dec-025 @parser-v2-019
  Scenario: 保存社團與標題中的底線
    Given 新收藏檔名為 "[Circle_Name] Work_Title.zip"
    When Rust v2 parser 解析檔名
    Then 社團應為 "Circle_Name"
    And 標題應為 "Work_Title"
    And parser 不應把底線改寫成空白

  @dec-026 @parser-v2-020
  Scenario: 已知標記使用完整內容匹配
    Given 新收藏檔名為 "[Digital Lover] D.L. action 56 [Dl版].zip"
    When Rust v2 parser 解析檔名
    Then 社團應為 "Digital Lover"
    And "[Dl版]" 應保存為 distribution marker
    And `is_dl` 應為 true

  @dec-027 @dec-028 @parser-v2-021
  Scenario: 未知尾端標記不阻止前方已知標記
    Given 新收藏檔名為 "[社團] 作品名稱 (原作候選) [DL版] [音声付き].zip"
    And parser 沒有足夠原作 evidence
    When Rust v2 parser 解析檔名
    Then "[DL版]" 應保存為 distribution marker
    And "[音声付き]" 應以 reason "unclassified_trailing_marker" 保存為 other info
    And "原作候選" 應依 DEC-002 保存為 other info
    And 標題應為 "作品名稱"

  @dec-028 @parser-v2-022
  Scenario: 未知尾端方括號不併入標題
    Given 新收藏檔名為 "[社團] 作品名稱 [無毒漢化組].zip"
    When Rust v2 parser 解析檔名
    Then 標題應為 "作品名稱"
    And "[無毒漢化組]" 應以 reason "unclassified_trailing_marker" 保存為 other info

  @dec-029 @parser-v2-023
  Scenario: 結構解析不改寫標題內的全形括號
    Given 新收藏檔名為 "★五月女レイナ編（セリフ、効果音付き）本編"
    When Rust v2 parser 解析檔名
    Then 標題應保持為 "★五月女レイナ編（セリフ、効果音付き）本編"

  @dec-043 @parser-v2-024
  Scenario: DL 版標記不能推斷 DLsite 場次
    Given 新收藏檔名沒有明確場次
    And 檔名具有可辨識的 DL distribution marker
    When Rust v2 parser 解析檔名
    Then `is_dl` 應為 true
    But 場次應保持未分類
    And parser 不應把數位版標記當成 DLsite 來源證據

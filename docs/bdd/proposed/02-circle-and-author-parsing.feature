@proposed @rust-v2 @dec-003
Feature: Rust v2 的社團與作者解析
  收藏管理者希望系統能解析主流社團與作者格式，
  並在符號或括號具有歧義時保留原文而不做不可逆猜測。

  @parser-v2-009
  Scenario: 拆分標準的社團與單一作者
    Given 新收藏的 leading bracket 為 "23.4ド (イチリ)"
    When Rust v2 parser 解析社團與作者
    Then 社團原始值應為 "23.4ド"
    And 作者清單應包含 "イチリ"
    And 完整 bracket 原始值應被保留

  @parser-v2-010
  Scenario Outline: 以可靠分隔符拆分多人作者
    Given 新收藏的 leading bracket 為 <bracket>
    When Rust v2 parser 解析社團與作者
    Then 社團原始值應為 <circle>
    And 作者清單應依序包含 <first_author> 與 <second_author>
    And 作者區段原始值應完整保留

    Examples:
      | bracket                              | circle         | first_author | second_author |
      | "NoFuture (すみすず、端音乱希)"     | "NoFuture"     | "すみすず"   | "端音乱希"    |
      | "Example (Author A, Author B)"      | "Example"      | "Author A"   | "Author B"    |

  @parser-v2-011
  Scenario Outline: 不以模糊符號拆分作者名稱
    Given 新收藏的作者區段為 <author_text>
    When Rust v2 parser 解析作者清單
    Then 作者清單應只包含完整的 <author_text>
    And parser 不應僅依 <separator> 將它拆成多位作者

    Examples:
      | author_text       | separator |
      | "黑男 & 杏二"     | "&"       |
      | "さくら小春＆小原トメ太" | "＆"      |
      | "3×3"            | "×"       |
      | "サテツ／彩社長" | "／"      |

  @parser-v2-012
  Scenario Outline: 單一 bracket 依上層分類決定角色
    Given 新收藏的上層分類為 <category>
    And leading bracket 只有 <value> 且沒有作者括號
    When Rust v2 parser 解析社團與作者
    Then <target_field> 的檔名解析值應為 <value>
    And 完整 bracket 原始值應被保留

    Examples:
      | category | value      | target_field |
      | "同人誌" | "無糖紅茶" | 社團         |
      | "CG"     | "K.Y.HIRO" | 社團         |
      | "商業誌" | "作者名稱" | 作者區段     |

  @parser-v2-013
  Scenario: 作者區段含有巢狀括號
    Given 新收藏的 leading bracket 為 "macdoll (士嬢マコ(・c_・ ))"
    When Rust v2 parser 解析社團與作者
    Then 社團原始值應為 "macdoll"
    And 作者清單應只包含 "士嬢マコ(・c_・ )"
    And parser 不應遞迴解讀作者名稱內的括號

  @external @parser-v2-014
  Scenario: 括號破損時先嘗試外部 metadata
    Given 新收藏的 leading bracket 括號不平衡或作者括號不在尾端
    When Rust v2 parser 無法可靠拆分社團與作者
    Then parser 不應自動填入社團或作者結構化欄位
    And 完整 bracket 應保存為其他資訊
    And 系統應記錄結構不明的原因
    And 系統應嘗試以外部 metadata 補足社團與作者

  @external @parser-v2-015
  Scenario: 外部 metadata 仍無法可靠補足破損檔名
    Given filename parser 無法可靠拆分社團與作者
    And 外部搜尋失敗或結果未達採用條件
    When 系統完成自動補足流程
    Then 收藏應標記為需要人工確認
    And 原始 bracket 與所有外部候選應保持可追溯


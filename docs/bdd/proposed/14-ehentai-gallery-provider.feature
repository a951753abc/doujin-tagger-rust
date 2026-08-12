@proposed @external @ehentai
Feature: Rust v2 的 E-Hentai／ExHentai gallery provider 與 namespace 標籤映射
  收藏管理者的檔案多數源自 ExHentai，
  希望以 gallery identity 或唯一書名取得原作、社團、作者與內容標籤，
  同時避免翻譯 gallery、搜尋排名與社群標註覆蓋人工資料。

  @dec-042 @ehentai-v2-001
  Scenario: 具有唯一 gid 與 token 時直接查詢 gdata
    Given 最新 parser 證據具有唯一的 E-Hentai 或 ExHentai gid/token
    When E-Hentai provider 處理外部資料工作
    Then provider 應直接以 gid/token 呼叫 gdata 而不搜尋書名
    And gdata 回傳的 gid 與 token 必須與 request 完全相同
    And metadata confidence 應標記為可靠識別碼完全匹配

  @dec-042 @ehentai-v2-002
  Scenario: 沒有 gallery identity 時以辨識書名核對唯一 gallery
    Given 收藏沒有 E-Hentai gallery identity
    And 收藏具有非空白的辨識書名
    When E-Hentai provider 搜尋 gallery
    Then provider 應從搜尋頁擷取最多 25 組 gid/token 並批次呼叫 gdata
    And 只有 title 或 title_jpn 去除社團前綴後唯一完全相符時才能選擇 gallery
    And 書名匹配不得標記為可靠識別碼完全匹配

  @dec-042 @ehentai-v2-003
  Scenario: 同名翻譯 gallery 不得取代原始 gallery
    Given 搜尋結果同時包含標題附有翻譯尾綴的 gallery 與原始 gallery
    And 只有原始 gallery 去除社團前綴後與辨識書名完全相同
    When provider 核對 gdata 的 title 與 title_jpn
    Then provider 應選擇原始 gallery
    And 翻譯 gallery 的 language 與內容標籤不得匯入該收藏

  @dec-042 @ehentai-v2-004
  Scenario: 多筆完全同名 gallery 保持歧義
    Given gdata 有兩筆以上 gallery 的正規化書名完全相同
    When provider 評估書名搜尋結果
    Then provider 不應依搜尋排名、上傳時間或評分自動選擇
    And 結果應為 no_match 並說明完全同名筆數

  @dec-042 @ehentai-v2-005
  Scenario: 專用 namespace 映射到結構化 metadata
    Given 選定 gallery 具有 group、artist 與 parody namespace
    When provider 建立請求欄位的 metadata 候選
    Then 唯一 group 可以映射為社團
    And artist 清單可以映射為作者
    And 唯一 parody 可以映射為原作
    And parody original 的 canonical 值應為「オリジナル」
    And group、artist 與 parody 不應重複加入一般 tags

  @dec-042 @ehentai-v2-006
  Scenario: 內容 namespace 保留為可搜尋 collection tags
    Given 選定 gallery 具有 character、female、male、mixed、other 或 language namespace
    When provider 映射 gallery tags
    Then 系統應保留「namespace:value」作為 collection tag
    And 空白、無 namespace 或未允許的暫時性標籤不得加入
    And 重複執行相同搜尋不得建立重複 collection-tag 關聯

  @dec-015 @dec-042 @ehentai-v2-007
  Scenario: 外部 tags 只在建議級以上信心度自動加入
    Given provider 回傳 E-Hentai tag candidate
    When 系統保存外部資料結果
    Then confidence 至少 0.75 的 tag 可以以 additive 方式加入收藏
    And tag 不得改寫任何人工 metadata 選擇
    And confidence 低於 0.75 或缺少來源參照的 tag 應拒絕保存
    And 工作 summary 應分別記錄收到與新加入的 tag 數量

  @dec-042 @ehentai-v2-008
  Scenario: ExHentai cookie 是可選的本機能力
    Given 本機環境提供有效的 DOUJIN_EXHENTAI_COOKIE
    When provider 建立 production client
    Then 書名搜尋應使用 ExHentai gallery catalog
    And gdata request 應攜帶相同 cookie
    And cookie 不得出現在 log、錯誤訊息或來源參照

  @dec-042 @ehentai-v2-009
  Scenario: E-Hentai 優先且 DLsite 只補缺少欄位
    Given E-Hentai 已回傳部分請求欄位與內容 tags
    When provider chain 評估尚未覆蓋的欄位
    Then DLsite 只應收到缺少欄位的 fallback request
    And 已由 E-Hentai 回傳的欄位不得再產生 DLsite 重複候選
    And E-Hentai 沒有場次而 DLsite 成功匹配商品時，DLsite 可以提供 "DL" 場次候選
    And DLsite 的 no_match 或 unsupported 不得使有效 E-Hentai 結果失敗

  @dec-042 @ehentai-v2-010
  Scenario: 兩個 provider 都失敗時保留完整診斷
    Given E-Hentai 書名搜尋回報 no_match
    And DLsite fallback 也回報失敗
    When provider chain 完成外部資料工作
    Then 工作錯誤訊息應同時包含 E-Hentai 與 DLsite 的失敗原因
    And 任一失敗可重試時工作應保留該可重試錯誤種類

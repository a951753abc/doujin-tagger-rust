@proposed @rust-v2 @dec-038
Feature: Rust v2 的 tombstone 身分合併
  收藏管理者希望在人工確認同名候選後安全合併收藏身分，
  同時保留 metadata、tags、位置與原始證據的完整稽核歷史。

  @consolidation-v2-001
  Scenario: 確認候選關聯不立即合併身分
    Given 一筆 tombstone 與一筆 active candidate 尚未確認為同一收藏
    When 收藏管理者將 candidate 標記為 confirmed
    Then 系統應只保存人工確認與裁決時間
    And 雙方收藏 ID、位置、metadata、tags 與實體檔案都不應立即改變
    And 系統應提供 consolidation preflight

  @consolidation-v2-002
  Scenario: 由較早的 tombstone ID 存活
    Given preflight 已允許合併 confirmed candidate
    When 收藏管理者明確執行 consolidation
    Then tombstone ID 應恢復為 active survivor
    And candidate 的目前位置應成為 survivor 的 current location
    And candidate ID 應成為指向 survivor 的 merged audit record
    And 實體檔案不應被搬移或重新命名

  @consolidation-v2-003
  Scenario: 無衝突資料完整併入並重建投影
    Given tombstone 與 candidate 沒有不同的手動選擇
    When 系統完成 consolidation
    Then tags 應採聯集且不得重複
    And parser runs、metadata assertions、外部結果與位置歷史都應保留
    And 每筆轉入證據應可追查原 collection ID 或 consolidation audit
    And tombstone 原本選中的值應優先保持
    And candidate 的不同非手動值應保存為未選候選
    And effective metadata 與全文搜尋應在同一 transaction 重建

  @consolidation-v2-004
  Scenario: 不同手動值必須先逐欄裁決
    Given tombstone 與 candidate 對同一欄位具有不同的手動選擇
    When 系統執行 consolidation preflight
    Then preflight 應列出雙方 assertion、來源與值
    And 未提供該欄位的 tombstone 或 candidate 選擇前不得開始 consolidation
    And 未被選中的 assertion 仍應保留原始來源與歷史

  @consolidation-v2-005
  Scenario: 多個同名候選必須全部裁決
    Given tombstone 同時具有多筆同名 candidate links
    When 收藏管理者準備執行 consolidation
    Then 必須恰有一筆 confirmed candidate
    And 其他 candidate links 都必須已明確 rejected
    And 仍有 pending 或多筆 confirmed 時 preflight 應阻止 consolidation

  @consolidation-v2-006
  Scenario: Consolidation 全部成功或全部 rollback
    Given consolidation 將同時變更身分、位置、證據、tags 與讀取投影
    When 任一 database constraint 或寫入步驟失敗
    Then 所有 catalog 變更都應 rollback
    And 實體檔案應保持不變
    And 相同成功要求重送時不應重複複製資料或歷史

  @consolidation-v2-007
  Scenario: 合併後保留查詢指向與完整稽核
    Given candidate 已成功合併至 survivor
    When 呼叫端查詢 Library、survivor 與 merged candidate
    Then Library 應只顯示 survivor 一次
    And survivor 應顯示 candidate 原本的目前位置
    And merged candidate 不應作為 active collection 回傳
    And merged candidate 查詢應指出 survivor collection ID
    And audit 應保存舊位置、candidate ID、人工裁決、衝突決策與合併時間
    And 系統不應提供未經獨立 BDD 定義的自動拆分

# Rust v2 儲存引擎評估：SQLite

最後查核：2026-08-12

## 結論

Rust v2 採 SQLite 作為本機 catalog 與 metadata 的權威儲存；ZIP、圖片資料夾與縮圖內容留在檔案系統。這項選擇是依目前已確認的產品邊界，而不是因為 Python 版本已使用 SQLite。

目前條件：

- 單一收藏管理者
- HTTP 服務只監聽 localhost
- 應用程式與資料庫位於同一台電腦
- 主要寫入者是同一個後端服務
- 約 13,566 筆收藏，資料量遠低於單檔資料庫限制
- 需要 relational constraints、transactions、tags、來源追溯與全文搜尋

SQLite 官方將 device-local、低 writer concurrency、低於 TB 級的資料列為合適情境；若資料與應用程式分隔於網路、多台電腦直接共用，或需要大量同時 writer，才建議 client/server database。

## 儲存邊界

| 資料 | 權威位置 | 原因 |
|---|---|---|
| 收藏身分、路徑狀態、metadata 候選、有效值、tags、tombstone、工作紀錄 | SQLite | 需要關聯、約束、transaction 與可追溯查詢 |
| ZIP 與圖片資料夾 | 檔案系統 | 是原始內容，不應複製成大型 database BLOB |
| 縮圖 | Cache 資料夾 | 可重建；SQLite 只記錄 digest、狀態、路徑與重試資訊 |
| Parser／外部來源的完整原始回應 | SQLite JSON TEXT | 保存證據；常用查詢欄位另存 relational projection |
| 標題、社團、作者、原作全文搜尋 | SQLite FTS5 | 與有效 metadata projection 同步，可重建 |
| 最近開啟 | 瀏覽器本機狀態 | 依 DEC-017，不進伺服器資料庫 |

## 實作護欄

1. 新 schema 使用 `STRICT` tables、foreign keys、CHECK 與 UNIQUE constraints。
2. Database 放在本機磁碟；不得讓多台電腦直接開啟網路共享上的同一個 SQLite 檔案。
3. 後端維持單一 writer queue；掃描、人工修改與背景工作以短 transaction 提交。
4. 使用 WAL 改善讀寫並行，但不把 WAL 誤解成多 writer database。
5. 設定 busy timeout，`SQLITE_BUSY` 必須成為可分類、可重試的錯誤。
6. 使用正式 schema migrations 與 `user_version`；啟動時不得默默忽略 migration failure。
7. JSON 只保存不固定或需要完整追溯的 payload；篩選、排序、唯一性與 foreign key 所需欄位必須 relational 化。
8. FTS、縮圖與 effective metadata projection 都視為可由權威資料重建的衍生資料。
9. 線上備份使用 SQLite backup API 或等價的一致性快照流程，不在寫入期間只複製單一 `.db` 而忽略 WAL。
10. v2 repository 只允許初始化空白 catalog 或開啟已辨識的 v2 schema；遇到非空白且未版本化的 database 時必須拒絕，避免誤把舊 `doujin.db` 原地升級。

## 第一個實作切片

2026-08-12 已採用 `rusqlite 0.40.2` 與 bundled SQLite：

- Bundled build 明確包含 FTS5 與 JSON1，不依賴使用者電腦另裝 SQLite runtime。
- 初始 migration 使用 `STRICT` tables、foreign keys、CHECK、partial UNIQUE indexes、`user_version = 1` 與 migration 紀錄。
- `CatalogRepository` 以單一 owned connection 提供 `&mut self` 寫入邊界；scanner 不直接寫 database。
- 檔案型 catalog 使用 WAL 與 5 秒 busy timeout；記憶體測試不啟用 WAL。
- 每筆 scanner 結果的收藏、位置、parser payload、metadata assertions、目前選擇、effective projection 與 FTS 在同一 transaction 中提交；批次的部分成功或整批 rollback 政策留給各操作的 BDD 決定。
- 初始測試只使用記憶體或臨時 database，未對現有 `doujin.db` 執行 migration 或寫入。

## Metadata repository 切片

同日完成的第二個切片將 DEC-007、DEC-013～015、DEC-022、DEC-032 與 DEC-035 接到 repository：

- Metadata 寫入使用與欄位相符的 typed value；空白文字不會被保存成最高優先的手動候選。
- 有效值依「手動修改 > 已接受的外部 metadata > 檔名解析 > 推斷結果」重選；待確認的外部建議不會自行進入有效值。
- 手動修改會保留舊候選歷史；清除手動值時將該 assertion 標為 obsolete，再從已接受候選回退，不建立空字串。
- 收藏管理者可明確選取既有候選；selection 會標記為手動裁決，後續自動結果不得覆寫。
- 人工選取或拒絕 assertion 時必須同時比對 collection ID、field 與 assertion ID；跨收藏或跨欄位裁決會在 transaction 內拒絕。
- 被拒絕的 assertion 只改為 `rejected`，原始 value、source reference、confidence、reason 與建立時間均保留；若它原本是 selection，才依來源優先序重選並重建 projection。
- 外部結果低於 0.75 時只進 `external_search_results`；0.75（含）至 0.95（不含），或沒有可靠識別碼完全匹配時，保存為待確認候選。
- 外部結果至少 0.95、具有可靠識別碼完全匹配且沒有手動衝突時，才會成為已接受候選並依優先序自動套用。
- Effective metadata 與 FTS 由 `metadata_selections` 指向的 assertions 重建；重建不刪除 assertions、人工裁決或其他權威資料。
- 每次候選／selection 變更和 projection 更新在同一 transaction 中完成。

## External metadata search job 切片

- Schema v3 為 `background_jobs` 加入 `partial` 與 `result_json`，並用 partial unique index 保證同一收藏最多一筆 pending／running 外部搜尋。
- Schema v5 新增一收藏一列的 `thumbnail_states`，保存來源與設定 fingerprint、狀態、cache 路徑、嘗試次數、typed error 與下一次重試時間；WebP bytes 不進 SQLite。
- Schema v6 新增 typed singleton `application_settings`，保存 reader path 與縮圖寬、高、品質；不再讓任意 key/value 混入 runtime 設定。
- v1、v2 catalog 依序 migration 到 v3；既有工作 ID、payload、狀態、錯誤、嘗試次數與時間均保留。
- Application 以 provider trait 隔離外部網站；provider 可以逐欄位回傳候選與 typed issues，不要求整批全成或全敗。
- 高、中、低 confidence 結果繼續共用 metadata repository 的 auto-applied、suggestion、search-only 規則；部分欄位失敗時已保存的成功結果不回滾。
- 暫時性錯誤依 network、rate limit、provider unavailable 使用不同基礎延遲並隨嘗試次數退避；永久性錯誤不設定 next retry time。
- HTTP API 建立及查詢工作；production server 以獨立 blocking thread 執行 DLsite exact-RJ provider，沒有單一 typed RJ 時不送 request。
- Application worker 以 due-time 與 batch limit 取得候選，再透過 `pending → running` 條件更新領取；未到 next retry time 的工作不會執行。
- 一輪 worker 可以同時回報 succeeded、partial、retry-scheduled 與 failed；已分類的 provider outcome 不會終止其他工作，非預期錯誤則記入 worker issues 並嘗試將已領取工作回復為 pending。
- HTTP server 開啟 catalog 後先把遺留的 running 外部搜尋標記為 `worker_interrupted` 並立即排回 pending；attempts 不遞增，重複復原是無變更操作。
- Production worker 每秒檢查一筆到期工作；取件與寫回短暫持有單一 application mutex，10 秒 host 限速與 blocking HTTP request 均在 mutex 外執行。Crash recovery 保留原有行為。

## Canonical repository 切片

DEC-005、DEC-016 與 DEC-033 的第一個 repository 切片遵守以下界線：

- Canonical entity 分為 event、circle、author 與 parody；正式名稱是否經官方確認以獨立欄位保存，不以語言自動決定優先權。
- Raw name 保留在原本的 metadata assertion 與 parser payload；`assertion_entities` 只記錄 assertion value 與 canonical entity 的映射及證據。
- Event、circle、parody 使用 value index 0；authors 依原清單索引逐項映射，因此可以只 canonicalize 部分作者而不改變順序。
- 建立 mapping 時會保存 name variant、原始來源與 mapping 證據；欄位種類、entity kind、value index 或 raw value 不一致時整個 transaction rollback。
- Effective metadata 與 FTS 顯示 canonical name；修改 canonical name 時重建所有受影響收藏，但不改寫 assertion JSON。
- 系統不會自動合併 entity。收藏管理者拒絕合併時，entity ID 先排序再保存，因此 `(A, B)` 與 `(B, A)` 是同一條排除規則。
- Canonical entity 仍被 name variant、assertion mapping 或 merge exclusion 引用時拒絕刪除；必須先明確處理所有引用，避免懸空資料。

## 收藏生命週期 repository 切片

DEC-008～011、DEC-031、DEC-034 與 DEC-036 的第一個狀態切片採用以下 transaction 邊界：

- 檔案服務已成功完成「下載區 → 已設定歸檔區」move 後，repository 才把舊位置標為 moved、建立新 current 位置並記錄 succeeded file operation；收藏 ID、metadata、tags 與 assertions 不變。
- Repository 不負責實際搬動正式收藏檔案；目前測試只在臨時資料夾模擬已完成 move。真正的跨磁碟安全搬移與失敗復原仍屬後續 file-operation service。
- 記錄 completed move 前驗證來源是下載區、目標 root 是啟用中的歸檔區、目標 ZIP 已存在、來源已消失、目標未被其他收藏索引，且解析後路徑確實位於歸檔區內。
- Scanner 確認 current path 已消失後，repository 將位置標為 missing、收藏改為 tombstone；effective metadata 與來源證據繼續保留，但有效 Library 搜尋只顯示 active 收藏。
- 新收藏入庫時若存在同檔名 tombstone，系統只建立 `same_filename` candidate link；每一筆新檔仍有獨立收藏 ID 與自己的 parser／metadata，不自動移轉舊資料。
- 人工裁決分別記錄 confirmed 或 rejected 與時間；confirmed 只代表已確認關聯，不會偷偷合併 ID。Schema v4 另以 consolidation audit 與 transfer origins 保存明確合併：preflight 先阻擋未裁決 candidates、active jobs 與未解手動衝突，再由 tombstone ID 在單一 transaction 接管 current location、tags 與全部證據並重建 projection。實體檔案不移動，失敗全部 rollback，重送保持冪等。

## File-operation service 切片

DEC-010～012 與 DEC-036 的第一個實體檔案切片由獨立 `doujin-files` crate 負責：

- 每筆操作先在 SQLite 建立 pending `file_operations`，再異動檔案；成功後以 transaction 更新位置／收藏狀態與 operation，確定未套用的失敗則記錄 failed。
- 若檔案已改變但 database 尚未確認，operation 保持 pending recovery；啟動後可依來源／目標是否存在重新完成，狀態不明確時不自行猜測。
- 同磁碟 ZIP move 優先使用 no-overwrite hard link 再移除來源；不能 hard link 時改用目標目錄中的唯一 `.partial` 檔。
- Copy fallback 完成後執行 `sync_all`，比較 byte count、檔案大小與 BLAKE3 digest；全部一致才用 hard link 發布最終檔名，因此不覆寫既有目標。
- 發布目標後若來源刪除失敗，先嘗試移除新目標回復原狀；若 rollback 也失敗則保留 pending recovery 並要求人工處理。
- 軟刪除 production backend 使用 `trash 5.2.6` 送往作業系統資源回收桶，收藏改為 soft-deleted 並保留 metadata；測試注入 fake backend，不污染實際資源回收桶。
- 永久刪除直接移除 ZIP，成功後刪除收藏、metadata 與 tag 關聯，但保留不再指向 collection 的 file-operation audit row。
- Batch 逐筆執行並分別回報 succeeded、failed 或 pending-recovery；單筆失敗不回滾已成功項目。
- 批次 11 的 HTTP adapter 只接受 collection IDs、archive root ID 與明確刪除模式，不接受任意來源／目的路徑；service 依 effective event 建立安全場次資料夾，空值使用「未分類」，並避開 Windows 非法字元與保留名稱。
- 破壞性操作開始前重新驗證 current location 屬於啟用中的已註冊來源；路徑 component 與 canonical parent 都必須在 root 內，相似字首或解析後越界會在建立 operation 前遭拒絕。
- Production server 啟動時先執行 pending file-operation reconciliation，再開始接受 HTTP 要求。這一切片對應 `file-v2-001`～`file-v2-006`、`boundary-001` 與 `boundary-002`。

## v2 migration rehearsal 切片

DEC-037 的唯讀遷移演練由 `doujin-migrate` crate 負責：

- 來源必須是靜止的舊 catalog 副本；以 SQLite URI `mode=ro&immutable=1`、`SQLITE_OPEN_READ_ONLY` 與 `query_only` 開啟，不執行舊 schema migration。
- 來源旁只要存在 `-wal` 或 `-shm` 就拒絕，避免 immutable 連線忽略尚未 checkpoint 的資料；target、target WAL 或 target SHM 任一已存在時也拒絕，不覆寫使用者資料。
- `settings.scan_roots` 轉成 v2 library roots；每個 legacy filepath 必須唯一、位於一個已設定 root 內，且收藏 source 必須與 root source 一致，否則在建立 target 前產生 blocked report。
- 舊 catalog 沒有保存 metadata provenance，因此舊有效值使用獨立 `legacy` source，selection 標成 `migration`。它不被誤稱為手動或外部資料，也不會被後續自動解析／外部搜尋靜默覆寫；新的明確手動修改仍可取代它。
- `成年コミック` 與 `官能小説` 依 DEC-004 轉成上層「商業誌」並保留子分類；同人誌、CG 與既有商業誌保持對應。
- Collections、current locations、metadata assertions／selections、effective projection、FTS、tags 與 tag links 在單一 transaction 中匯入；失敗時不留下部分 target。
- 驗證報告比較所有群組數量、舊欄位與 effective metadata 空值分布、正規化路徑衝突、完整 tag 集合／關聯、100 筆均勻 metadata 抽樣、foreign-key check、integrity check，以及來源副本匯入前後的 BLAKE3。

2026-08-12 已用正式 `doujin.db` 的副本完成一次完整演練，暫存 target 隨後刪除，未進行正式切換：

- Collections：13,566 → 13,566；current locations 與 effective metadata 各 13,566。
- Legacy metadata assertions／selections：87,116；library roots：3 → 3。
- Tags：0 → 0；tag links：0 → 0。
- 路徑衝突、blocking issues、count mismatches、抽樣 mismatches 與 foreign-key violations 都是 0；`integrity_check` 為 `ok`。
- 來源副本沒有被修改，也沒有新增 WAL／SHM。正式 `doujin.db` 前後 SHA-256 均為 `2E0733C6E3700D6410335242C30738DCCB2EAAE848441F65B211EDB06D592385`，且前後都沒有 sidecar。
- 這只證明目前資料可安全轉換，不代表已批准建立正式 `doujin-v2.db` 或切換應用程式。

## Application service 與 scan journal 切片

同日完成的 `doujin-app` 第一個 use-case 邊界落實 scan-001／002／003／005／006／007／011 與批次部分成功規則：

- Application service 擁有單一 `CatalogRepository`，先建立 running `scan_runs`，再查詢 current paths、呼叫 scanner 並逐筆 transaction 入庫。
- 同一路徑已存在時 scanner 直接跳過，不重新解析或改寫 metadata；repository 仍保留 duplicate guard，處理掃描快照後才出現的競態。
- Scanner issue 與單筆 ingest failure 都轉成 `scan_issues`；其他項目繼續處理，scan run 完成為 `partial`。沒有問題時為 `succeeded`，無掃描來源也會留下可觀察的 partial run，而不是無聲返回。
- JSON 摘要保存 roots、發現、pending、新增、跳過、入庫失敗、重新命名、解析狀態、tombstone、候選關聯與耗時。消失位置只在 root 完整可讀且存在同檔名候選時依 DEC-008 建立 tombstone；root 遺失或 traversal 不完整時不執行 reconciliation。沒有同名候選時不推定刪除政策。圖片資料夾尚未實作。
- `scan_runs_single_running` 以 schema migration `0002_scan_run_guard` 建立 partial unique index；既有 version 1 catalog 可升級到 version 2 且保留資料，不以修改舊 migration 冒充升級。
- Application service 同時作為 move、delete 與 pending recovery 的 facade，讓後續 localhost HTTP adapter 共用既有 file-operation journal 與部分成功語意。

## Localhost HTTP adapter 切片

第一個 `doujin-http` adapter 使用 Axum 0.8.9 與 Tokio 1.53.1，將 transport 限制保持在 application core 之外：

- Production binary 只建立 `127.0.0.1:port` listener；library bind 與 serve 邊界都再次驗證 `IpAddr::is_loopback`，明確拒絕 `0.0.0.0`、LAN interface 與其他非 loopback address。IPv4 `127.0.0.1` 與 IPv6 `::1` 都是允許的配置。
- `POST /api/scans` 沒有 root path request 欄位，只讀取 catalog 內啟用中的 `library_roots`，避免呼叫端把 scanner 指向任意本機路徑。
- 同步 scanner／SQLite 工作放進 blocking task，並對 application service 使用 non-blocking mutex acquisition；另一個 scan 正在執行時回傳 HTTP 409，不讓第二個要求無限等待。
- `GET /api/health` 不碰 catalog；`GET /api/scans/{id}` 只讀 persistent run 與 issues。Domain、404 與 405 錯誤共用結構化 JSON envelope。
- 所有 requests 先驗證 HTTP Host 只能是 `localhost` 或 loopback IP，阻止 DNS rebinding 網域讀取本機 API。Mutating methods 若帶 Origin／Referer，另把 URI authority 解析成確切 host；不使用容易被 `localhost.evil` 繞過的 substring comparison。缺少來源標頭仍允許本機 CLI 呼叫。
- Integration tests 使用 OS 配發的 `127.0.0.1:0` 真實 TCP listener 驗證 health、成功 scan、partial scan、409、404／405 與 cross-site rejection；沒有監聽正式 port 或操作正式 catalog。

Library root 設定延續相同的 application-service 邊界：

- `GET /api/library-roots` 回傳啟用與停用來源，保留設定歷史供 UI 呈現。
- `POST /api/library-roots` 只接受絕對路徑、實際存在的資料夾、`archive`／`downloads` 類型與非空白標籤；同一路徑使用既有 ID 更新並重新啟用。
- `DELETE /api/library-roots/{id}` 的 transport 語意是停用掃描來源，不刪除 `library_roots` row、位置歷史或收藏資料；停用來源不會出現在下一次 `active_scan_roots`。
- JSON decode、無效來源、無效 ID 與不存在的 root 都使用既有 JSON error envelope，不落回 Axum 的純文字 rejection。
- 真實 loopback socket 測試涵蓋註冊、列出、停用、掃描忽略、重新啟用，以及各種無效輸入。

唯讀收藏查詢也不讓 HTTP adapter 直接組合任意 SQL：

- `GET /api/collections` 只回傳具有 current location 的 active 收藏，預設每頁 50 筆，`per_page` 限制在 1 到 200，並固定以 collection ID 反向排序。
- `q` 先移除雙引號與控制字元，再將每個有效詞包成 FTS prefix term；metadata FTS 與經過 `%`／`_`／反斜線 escape 的檔名 `LIKE` 條件合併。沒有有效詞時退回未指定搜尋，不執行呼叫端提供的 FTS 語法。
- `GET /api/collections/{id}` 回傳目前位置、root、effective metadata、tags 與時間戳記；tombstone、soft-deleted 或不存在的 ID 對 Library 都視為 not found。
- Axum query decode failure、無效 collection ID 與 not found 共用 JSON error envelope。未知排序參數由固定 DTO 忽略，不會拼接進 SQL。
- 查詢 allowlist 另支援 event、circle、author、parody、classification、subcategory、source，以及可重複的 `tag`／`missing`。不同 filters 與多個 tags 全部採 AND；作者以 `json_each(authors_json)` exact match，空作者清單則符合 `missing=authors`。
- 動態 SQL 只拼接程式內建的欄位片段；所有 filter values 仍透過 SQLite bind parameters 傳入。單值 filter 重複、空白值、未知 source／missing 都在 transport 邊界回傳 JSON 400。

單筆 metadata／tags 寫入同樣經過 application service：

- `PUT /api/collections/{id}/metadata/{field}` 將 typed JSON value 轉成現有 `MetadataValue`，由 repository 建立 manual assertion、保留其他來源候選並重建 projection／FTS。
- `DELETE /api/collections/{id}/metadata/{field}` 不建立手動空白值；它淘汰 manual assertion，再依 manual、external、filename、inference 優先序選出剩餘候選。重複清除是冪等操作。
- Metadata field 只允許 title、event、circle、authors、parody、classification、is_dl；path、root、source 與時間戳記不能透過此 route 修改。所有欄位都有 transport type validation。
- `POST`／`DELETE /api/collections/{id}/tags` 使用 JSON tag name；新增關聯與重複移除皆冪等，最後一個關聯移除時刪除孤兒 tag row。
- Application service 在 mutation 前以 active collection read model 驗證目標，避免對 tombstone、soft-deleted 或不存在收藏寫入。所有 routes 仍受 localhost Host／Origin guard 保護。

Metadata history read model 保持 projection 與證據分離：

- `GET /api/collections/{id}/metadata` 固定回傳七個 metadata fields；每個 field 分開提供目前 selection、完整 assertions 歷史及 external search results。
- Assertions 保留 source、status、parser run、source reference、confidence total／components、reason 與時間；selected flag 由 selection assertion ID 計算，不以 assertion status 猜測目前值。
- External `search_only` 可能沒有 assertion ID，仍保留 value、來源與 confidence 供追查；suggestion／auto-applied result 則可連回 assertion。兩種資料不合併，以免低信心結果被 UI 誤當成可套用候選。
- Application service 先驗證 active collection，再讀取歷史；tombstone、soft-deleted 與不存在 ID 都回傳相同 not-found 邊界。

## 何時應改用 PostgreSQL

出現下列任一需求時重新評估：

- 多台電腦上的應用程式需要直接共用 catalog
- 同時執行多個後端實例
- 多個 writer 不能排隊或 transaction 經常長時間持有寫鎖
- Database 必須由獨立伺服器集中管理與授權
- 資料量或分析工作已不適合單一本機檔案

目前沒有任何已確認需求符合這些條件。

## 官方資料

- [Appropriate Uses For SQLite](https://www.sqlite.org/whentouse.html)
- [About SQLite](https://www.sqlite.org/about.html)
- [Write-Ahead Logging](https://www.sqlite.org/wal.html)
- [SQLite FTS5 Extension](https://www.sqlite.org/fts5.html)
- [STRICT Tables](https://www.sqlite.org/stricttables.html)
- [JSON Functions And Operators](https://www.sqlite.org/json1.html)
- [SQLite Is Serverless](https://www.sqlite.org/serverless.html)

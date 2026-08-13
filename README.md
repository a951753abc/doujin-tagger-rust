# Doujin Tagger（私藏編目室）

Doujin Tagger 是一套以 Rust 開發、在本機執行的同人作品收藏管理工具。它會掃描使用者指定的資料夾，從 ZIP 檔名解析作品資訊，建立可搜尋的 SQLite catalog，並透過瀏覽器提供書架、編目、批次整理與檔案管理介面。

本專案採本機優先設計：服務只監聽 `127.0.0.1`，不需要雲端帳號，也不會把 catalog 或收藏檔案上傳到遠端。只有在使用外部 metadata 搜尋時，才會向 E-Hentai／ExHentai 或 DLsite 發出查詢。

> 一般使用者可直接下載 Windows portable 版本，解壓縮後雙擊 `私藏編目室.exe`；不需要安裝 Rust、Cargo，也不需要輸入指令。

## 主要功能

- 遞迴掃描新收藏與典藏庫中的 ZIP，建立本機收藏索引。
- 從檔名解析標題、場次、社團、作者、原作、分類、RJ 編號與 DL 標記。
- 以書架、列表或比較模式瀏覽，並依 metadata、來源、標籤或缺漏欄位搜尋及篩選。
- 保存常用篩選為 Saved View，快速回到特定收藏集合。
- 從 ZIP 產生 WebP 縮圖，亦可手動指定其他圖片作為封面。
- 手動編輯 metadata、管理標籤，並保留來源、候選、信心度與裁決歷史。
- 透過背景工作從 E-Hentai／ExHentai、DLsite 補齊外部 metadata。
- 使用工作籃與工作台批次加標籤、補資料、改名、搬移、匯出或刪除收藏。
- 在真正改名、搬移或匯出前先執行預檢，避免路徑衝突與意外覆寫。
- 以檔案內容指紋找出完全相同、內容相同或可能重複的作品，交由使用者裁決。
- 提供品質審核、名稱正規化、同名收藏身分合併與收藏統計。

## 系統需求

- Windows 10 或 Windows 11。
- 現代瀏覽器。
- 足以保存 SQLite catalog 與縮圖快取的本機空間。

目前的新收藏掃描以 ZIP 為主。Catalog、縮圖與程式狀態都會保存在本機；收藏 ZIP 不會被匯入資料庫。

## 快速開始

### 1. 下載 Windows 版

前往 [GitHub Releases](https://github.com/a951753abc/doujin-tagger-rust/releases/latest)，下載最新的：

- `Doujin-Tagger-<版本>-Windows-x64.zip`

將 ZIP **完整解壓縮**到一般資料夾，不要留在 ZIP 內直接執行。

### 2. 雙擊啟動

雙擊解壓縮資料夾內的：

```text
私藏編目室.exe
```

程式會自動啟動本機服務並開啟瀏覽器。日常使用也只需要再次雙擊同一個 EXE；若服務已在執行，會直接開啟原本的 UI，不會啟動第二份互相競爭的服務。

> 目前執行檔尚未進行程式碼簽章。Windows SmartScreen 若顯示警告，請先確認檔案來自本專案 GitHub Releases，並可用 Release 同頁的 `.sha256` 檔核對下載內容。

### 3. 建立或開啟 catalog

第一次雙擊後，程式會以 Windows 對話框要求建立新的 v2 catalog，或選取既有的 v2 catalog。接著瀏覽器會開啟首次設定導引：

1. 登記「新收藏」資料夾，作為尚待整理作品的來源。
2. 登記「典藏庫」資料夾，作為完成整理後的收藏位置。
3. 選擇使用 Windows 預設程式，或指定閱讀器。
4. 決定是否立即預覽第一次掃描。

資料夾必須已存在，並應依實際用途分別登記。舊 Python 版的 `doujin.db` 不能直接當成 v2 catalog 使用，請先參考「舊資料遷移」。

### 4. 執行第一次掃描

在首次設定或「設定 → 資料夾來源」按下掃描後：

1. 先檢查掃描預覽、警告與可能的安全改名。
2. 確認要套用安全改名，或選擇不改名只建立索引。
3. 執行掃描。
4. 回到「書架」或「全部藏書」查看結果。

掃描不會直接刪除收藏。檔名無法安全解析、目的名稱衝突或來源讀取不完整時，系統會保留原檔並顯示問題。

## 介面使用說明

| 區域 | 用途 |
|---|---|
| 書架 | 查看最近加入、主要原作與場次書架，以及已釘選的 Saved Views。 |
| 全部藏書 | 搜尋、篩選、排序、切換列表／比較模式，並選取收藏加入工作籃。 |
| 工作籃 | 暫存跨頁挑選的收藏，準備後續批次操作。 |
| 品質審核 | 集中處理缺少 metadata、低信心度或需要人工確認的項目。 |
| 重複作品 | 建立內容指紋、查看重複候選，並記錄確認或排除結果。 |
| 工作台 | 批次補資料、加標籤、重新命名、搬移、匯出、刪除及處理身分裁決。 |
| 統計 | 查看有效收藏的分類、場次、社團、作者、原作與標籤分布。 |
| 設定 | 管理閱讀器、縮圖、掃描來源、典藏庫與匯出目的地。 |

### 搜尋與整理收藏

- 頂端搜尋可查詢標題、社團、作者、原作與檔名。
- 「全部藏書」可組合 metadata、來源、標籤及缺漏欄位等篩選條件。
- 常用條件可以保存成 Saved View，並釘選到書架。
- 開啟單本詳細資料後，可以修改 metadata、加入標籤、查看證據歷史、選擇封面，或用系統預設程式／指定閱讀器開啟 ZIP。
- 外部 metadata 搜尋是背景工作；可離開目前頁面，進度與結果仍會保留。

### 批次操作

先在「全部藏書」或單本詳細資料把收藏加入工作籃，再前往「工作台」。可執行：

- 批次加入標籤。
- 批次指定原作或分類。
- 只補缺漏欄位，或指定欄位進行外部 metadata 搜尋。
- 依 `{event}`、`{circle}`、`{title}` 等欄位預覽並套用安全改名。
- 將新收藏搬到指定典藏庫；系統會依場次建立安全子資料夾，且不覆寫同名 ZIP。
- 將原始 ZIP 複製成匯出套件；匯出不修改 catalog、metadata、標籤或來源檔。
- 移到資源回收桶，或在再次確認後永久刪除。

涉及檔案變更的操作會在執行前與執行時重新檢查來源、目標、名稱及衝突。一般情況建議優先使用資源回收桶，而非永久刪除。

### 重複作品

前往「重複作品」並啟動掃描後，系統會在背景建立內容指紋。候選分為：

- `exact`：來源檔案完全相同。
- `content`：壓縮方式可能不同，但作品圖片內容相同或高度重疊。
- `probable`：內容高度相似，需要人工確認。

確認重複只會保存裁決結果，不會自動合併身分或刪除檔案。需要移除其中一本時，請明確送入刪除流程並再次核對。

## 進階與開發者用法

以下指令不是一般安裝或日常使用的必要步驟。一般使用者只需下載 Release、解壓縮並雙擊 `私藏編目室.exe`。

### 直接執行本機服務

不安裝 Launcher 也可以從專案根目錄直接啟動：

```powershell
New-Item -ItemType Directory -Force .\data | Out-Null
cargo run --release -p doujin-http -- .\data\doujin-v2.db 5000
```

接著開啟 `http://127.0.0.1:5000/`。按 `Ctrl+C` 可停止服務。

`doujin-http` 的命令格式為：

```text
doujin-http <v2-catalog.db> [port]
```

Port 未指定時預設為 `5000`。Listener 固定為 loopback 位址，不能改成區域網路或公開網路介面。

### Launcher 指令

在 Launcher 安裝目錄執行，或將該目錄加入 `PATH` 後使用：

```powershell
doujin-launcher.exe open --catalog .\doujin-v2.db
doujin-launcher.exe status
doujin-launcher.exe restart
doujin-launcher.exe stop
doujin-launcher.exe help
```

未提供子命令時等同 `open`。Launcher 會自動選擇可用的 loopback port、重用健康且屬於同一 catalog 的既有服務，並在啟動完成後開啟瀏覽器。

Launcher 的設定、服務狀態與 log 預設保存在 `%LOCALAPPDATA%\Doujin Tagger`。若啟動失敗，可先查看其中的 `service-error.log`。

### 設定優先序

閱讀器與縮圖設定建議直接在 Web UI 的「設定」頁管理。進階使用者也可以使用 `config.json` 或環境變數。

啟動時的設定優先序如下：

```text
環境變數 > catalog 內設定 > config.json > 預設值
```

| 環境變數 | 用途 |
|---|---|
| `DOUJIN_READER_PATH` | 指定閱讀器執行檔；必須是絕對路徑。 |
| `DOUJIN_THUMB_DIR` | 指定縮圖快取目錄；必須是絕對路徑。 |
| `DOUJIN_THUMB_SIZE` | 縮圖尺寸，例如 `360x480`。 |
| `DOUJIN_THUMB_QUALITY` | WebP 品質，範圍為 `1` 到 `100`。 |
| `DOUJIN_CONFIG_PATH` | 指定其他 `config.json` 位置。 |
| `DOUJIN_EXHENTAI_COOKIE` | 選用的 ExHentai cookie；未設定時使用公開 E-Hentai。 |

預設縮圖尺寸為 `300x400`、WebP 品質為 `80`；快取目錄位於 catalog 旁的 `<catalog 檔名>.thumbnails`。環境變數鎖定的欄位不能在執行中的 Web UI 覆寫。

### 檔名 Parser CLI

`doujin-parser` 可以獨立測試單筆或批次檔名解析，不會讀寫 catalog。它從標準輸入讀取 JSON，並將結果寫到標準輸出。

```powershell
@'
{
  "filename": "[社團] 作品名稱 (ポケモン).zip",
  "parody_evidence": [
    {
      "raw": "ポケモン",
      "kind": "confirmed_alias",
      "canonical": "ポケットモンスター"
    }
  ]
}
'@ | cargo run --quiet -p doujin-parser
```

若沒有已確認的原作證據，請傳入空的 `parody_evidence` 陣列。也可以傳入 JSON 陣列批次解析多個檔名，輸出順序會與輸入相同。

### 舊資料遷移

`doujin-migrate` 用來把舊 Python catalog 的唯讀副本匯入全新的 v2 catalog。它不會原地升級舊資料庫，也拒絕覆寫已存在的 target。

基本演練流程：

```powershell
New-Item -ItemType Directory -Force .\migration | Out-Null
Copy-Item .\legacy-catalog.db .\migration\legacy-copy.db
cargo run --release -p doujin-migrate -- .\migration\legacy-copy.db .\migration\doujin-v2.db
```

請只把舊 catalog 的副本交給第一次演練，並先確認來源旁沒有 WAL、SHM 或 journal sidecar。正式切換前應備份資料，完成 migration report、path audit 與驗收閘門；完整流程請參考 [正式切換與回復手冊](docs/references/formal-cutover-and-rollback-runbook.md)。

## 資料與安全

- Catalog 使用 SQLite，保存收藏索引、metadata、標籤、工作狀態與稽核歷史。
- 縮圖是可重建的 WebP cache，不存入 SQLite。
- 本機 HTTP 服務只接受 loopback Host，寫入請求也會驗證 Origin／Referer，以降低 DNS rebinding 風險。
- 掃描來源、典藏庫與匯出目的地都必須先登記；瀏覽器不能提交任意檔案路徑或執行檔。
- 改名、搬移、刪除與匯出會再次驗證目前路徑及檔案狀態，並保存操作結果。
- 建議定期備份 catalog；進行大量改名、搬移、永久刪除或舊資料遷移前，請另外備份收藏檔案。

## Workspace 結構

| Crate | 職責 |
|---|---|
| `doujin-http` | Axum 本機服務、內嵌 Web UI 與背景 workers。 |
| `doujin-launcher` | Windows 啟動、停止、重啟、catalog 選擇與瀏覽器開啟。 |
| `doujin-app` | 掃描、metadata、檔案操作、重複判定、改名與匯出的 use cases。 |
| `doujin-storage` | SQLite schema、repository、搜尋索引與稽核資料。 |
| `doujin-parser` | 檔名解析 library 與 JSON CLI。 |
| `doujin-scanner` | 收藏發現、排除規則與安全檔名正規化。 |
| `doujin-thumbnails` | ZIP 圖片選取、資源限制、縮放與 WebP cache。 |
| `doujin-files` | 安全開啟、搬移、刪除與中斷操作復原。 |
| `doujin-provider-ehentai` | E-Hentai／ExHentai metadata provider。 |
| `doujin-provider-dlsite` | DLsite 精確 RJ 與保守 fallback provider。 |
| `doujin-migrate` | 舊 catalog 唯讀遷移與 v2 path audit。 |

## 從原始碼建置

只有開發、修改程式或自行製作 Windows 執行檔時才需要 Rust `1.97` 以上版本、Cargo 與 PowerShell。建議透過 [rustup](https://rustup.rs/) 安裝 Rust。

建置並在目前使用者帳號建立桌面與開始功能表捷徑：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\install_windows_launcher.ps1
```

安裝完成後，一般啟動捷徑會使用不顯示命令列視窗的 `私藏編目室.exe`；服務狀態、重新啟動與停止捷徑則由 `doujin-launcher.exe` 處理。

### 開發驗證

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release -p doujin-http -p doujin-launcher
```

Parser 黃金語料位於 [`tests/fixtures/parser-corpus-v1.json`](tests/fixtures/parser-corpus-v1.json)，需求與驗收情境則整理在 [`docs/bdd`](docs/bdd/README.md)。

## 授權

本專案採用 [MIT License](LICENSE)。

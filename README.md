# JP6 Doujin Archive

JP6 Doujin Archive 是一套以 Rust 開發、在本機執行的同人作品收藏管理工具。它會掃描指定資料夾中的 ZIP，從檔名解析作品資訊，建立可搜尋的 SQLite catalog，並提供書架、編目、批次整理與檔案管理介面。

本專案採本機優先設計：服務只監聽 `127.0.0.1`，不需要雲端帳號，也不會上傳 catalog 或收藏檔案。只有使用 E-Hentai／ExHentai 或 DLsite 搜尋外部 metadata 時才需要網路。

> 應用程式名稱為「JP6 Doujin Archive」；GitHub repository、Release 資產與部分命令仍沿用專案名稱「Doujin Tagger」。

## 下載與快速開始

前往 [GitHub Releases](https://github.com/a951753abc/doujin-tagger-rust/releases/latest)，選擇其中一種 Windows x64 版本：

| 版本 | 下載檔案 | 使用方式 |
|---|---|---|
| 安裝版（推薦） | `Doujin-Tagger-<版本>-Windows-x64-setup.exe` | 執行安裝精靈後，從開始功能表或桌面捷徑開啟。 |
| Portable 版 | `Doujin-Tagger-<版本>-Windows-x64.zip` | 完整解壓縮後，雙擊 `JP6 Doujin Archive.exe`。 |

兩者都不需要安裝 Rust、Cargo 或輸入指令。安裝版只安裝到目前使用者帳號，不需要系統管理員權限；它使用原生視窗，關閉視窗即完全結束程式。Portable 版則會啟動本機服務並在瀏覽器開啟介面。兩者會共用 `%LOCALAPPDATA%\Doujin Tagger` 中的 catalog 選擇與啟動設定。

> 目前執行檔尚未進行程式碼簽章。若 Windows SmartScreen 或防毒軟體顯示警告，請先確認檔案來自本專案 GitHub Releases，並使用 Release 同頁的 `.sha256` 檔核對下載內容。

### 第一次啟動

1. 建立新的 v2 catalog，或選取既有的 v2 catalog。
2. 登記「新收藏」資料夾，作為尚待整理作品的來源。
3. 登記「典藏庫」資料夾，作為整理完成後的收藏位置。
4. 選擇使用 Windows 預設程式，或指定閱讀器。
5. 預覽第一次掃描，確認是否套用安全改名，再建立收藏索引。

資料夾必須已存在，並應依實際用途分別登記。舊 Python 版的 `doujin.db` 不能直接當成 v2 catalog 使用，請先參考「[舊資料遷移](#舊資料遷移)」。

掃描不會直接刪除收藏。檔名無法安全解析、目的名稱衝突或來源讀取不完整時，系統會保留原檔並顯示問題。

## 主要功能

- 遞迴掃描新收藏與典藏庫中的 ZIP，從檔名解析標題、場次、社團、作者、原作、分類、RJ 編號與 DL 標記。
- 以書架、列表或比較模式瀏覽，並依 metadata、來源、標籤或缺漏欄位搜尋與篩選；常用條件可保存為 Saved View。
- 從 ZIP 產生 WebP 縮圖，也可手動選擇其他圖片作為封面。
- 手動編輯 metadata、管理標籤，或透過背景工作從 E-Hentai／ExHentai、DLsite 補齊資料。
- 使用工作籃與工作台批次加標籤、補資料、改名、搬移、匯出或刪除收藏。
- 在改名、搬移與匯出前執行預檢，並以內容指紋協助裁決完全相同或可能重複的作品。
- 提供品質審核、名稱正規化、同名收藏身分合併與收藏統計。

## 介面導覽

| 區域 | 用途 |
|---|---|
| 書架 | 查看最近加入、主要原作、場次書架與自訂智慧書架。 |
| 全部藏書 | 搜尋、篩選、排序、切換列表／比較模式，並選取收藏加入工作籃。 |
| 工作籃 | 暫存跨頁挑選的收藏，準備後續批次操作。 |
| 品質審核 | 集中處理缺少 metadata、低信心度或需要人工確認的項目。 |
| 重複作品 | 建立內容指紋、查看重複候選，並記錄確認或排除結果。 |
| 工作台 | 批次補資料、加標籤、改名、搬移、匯出、刪除及處理身分裁決。 |
| 統計 | 查看有效收藏的分類、場次、社團、作者、原作與標籤分布。 |
| 設定 | 管理閱讀器、縮圖、掃描來源、典藏庫與匯出目的地。 |

### 整理與批次操作

在「全部藏書」或單本詳細資料把收藏加入工作籃，再前往「工作台」，即可批次加入標籤、指定原作或分類、補齊缺漏欄位、套用安全改名、搬移到典藏庫，或複製原始 ZIP 建立匯出套件。

涉及檔案變更的操作會在執行前與執行時重新檢查來源、目標、名稱及衝突。刪除時建議優先移到資源回收桶；永久刪除需要再次確認。

### 重複作品

「重複作品」會在背景建立內容指紋，並將候選分為：

- `exact`：來源檔案完全相同。
- `content`：壓縮方式可能不同，但作品圖片內容相同或高度重疊。
- `probable`：內容高度相似，需要人工確認。

確認重複只會保存裁決結果，不會自動合併身分或刪除檔案。

## 系統需求

- Windows 10 或 Windows 11 x64。
- 安裝版需要 Microsoft Edge WebView2 Runtime；若系統尚未安裝，安裝程式會連網下載並安裝。
- Portable 版需要現代瀏覽器。
- 外部 metadata 搜尋需要網路；掃描、瀏覽、編輯與檔案整理可在本機完成。
- 足以保存 SQLite catalog 與可重建 WebP 縮圖快取的本機空間。收藏 ZIP 不會被匯入資料庫。

## 資料與安全

- Catalog 使用 SQLite，保存收藏索引、metadata、標籤、工作狀態與稽核歷史。
- 縮圖是可重建的 WebP cache，不存入 SQLite。
- 本機 HTTP 服務只接受 loopback Host，寫入請求也會驗證 Origin／Referer，以降低 DNS rebinding 風險。
- 掃描來源、典藏庫與匯出目的地都必須先登記；瀏覽器不能提交任意檔案路徑或執行檔。
- 改名、搬移、刪除與匯出會重新驗證目前路徑及檔案狀態，並保存操作結果。
- 建議定期備份 catalog；大量改名、搬移、永久刪除或舊資料遷移前，也應另外備份收藏檔案。

## 進階用法

以下內容不是一般安裝或日常使用的必要步驟。

### 直接執行本機服務

從專案根目錄啟動：

```powershell
New-Item -ItemType Directory -Force .\data | Out-Null
cargo run --release -p doujin-http -- .\data\doujin-v2.db 5000
```

接著開啟 `http://127.0.0.1:5000/`，按 `Ctrl+C` 停止服務。命令格式為：

```text
doujin-http <v2-catalog.db> [port]
```

Port 未指定時預設為 `5000`。Listener 固定為 loopback 位址，不能改成區域網路或公開網路介面。

### Portable Launcher 指令

在 Portable 版解壓縮目錄執行：

```powershell
doujin-launcher.exe open --catalog .\doujin-v2.db
doujin-launcher.exe status
doujin-launcher.exe restart
doujin-launcher.exe stop
doujin-launcher.exe help
```

未提供子命令時等同 `open`。Launcher 會自動選擇可用的 loopback port、重用同一 catalog 的健康服務，並開啟瀏覽器。設定、服務狀態與 log 預設保存在 `%LOCALAPPDATA%\Doujin Tagger`；啟動失敗時可先查看 `service-error.log`。

### 設定優先序

閱讀器與縮圖建議直接在「設定」頁管理。進階使用者也可使用 `config.json` 或環境變數；優先序為：

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

`doujin-parser` 可獨立測試單筆或批次檔名解析，不會讀寫 catalog。它從標準輸入讀取 JSON，並將結果寫到標準輸出：

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

沒有已確認的原作證據時，請傳入空的 `parody_evidence` 陣列。也可以傳入 JSON 陣列批次解析，輸出順序會與輸入相同。

### 舊資料遷移

`doujin-migrate` 會將舊 Python catalog 的唯讀副本匯入全新的 v2 catalog。它不會原地升級，也拒絕覆寫已存在的 target。

```powershell
New-Item -ItemType Directory -Force .\migration | Out-Null
Copy-Item .\legacy-catalog.db .\migration\legacy-copy.db
cargo run --release -p doujin-migrate -- .\migration\legacy-copy.db .\migration\doujin-v2.db
```

請只使用舊 catalog 的副本，並先確認來源旁沒有 WAL、SHM 或 journal sidecar。正式切換前應完成備份、migration report、path audit 與驗收閘門；完整流程請參考 [正式切換與回復手冊](https://github.com/a951753abc/doujin-tagger-rust/blob/main/docs/references/formal-cutover-and-rollback-runbook.md)。

## 從原始碼建置

所有 crate 都需要 Rust `1.97` 以上版本。建置 Windows 執行檔還需要 PowerShell 與可用的 Rust MSVC 建置環境；製作原生 NSIS 安裝版另需 Node.js／npm。

### Portable 版與本機捷徑

下列腳本會建置 `doujin-http`、`doujin-launcher` 與無命令列視窗的啟動程式，並在目前使用者帳號建立桌面與開始功能表捷徑：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\tools\install_windows_launcher.ps1
```

### 原生 NSIS 安裝版

```powershell
Set-Location .\doujin-desktop
npx --yes @tauri-apps/cli@2.11.4 build
```

安裝檔會輸出到 `target\release\bundle\nsis`。建置時若尚未快取 Tauri CLI，`npx` 需要連網下載套件。

### 開發驗證

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features --locked
cargo build --release --locked -p doujin-http -p doujin-launcher
```

Parser 黃金語料位於 [tests/fixtures/parser-corpus-v1.json](https://github.com/a951753abc/doujin-tagger-rust/blob/main/tests/fixtures/parser-corpus-v1.json)，需求與驗收情境整理在 [docs/bdd](https://github.com/a951753abc/doujin-tagger-rust/blob/main/docs/bdd/README.md)。

## Workspace 結構

| Crate | 職責 |
|---|---|
| `doujin-http` | Axum 本機服務、內嵌 Web UI 與背景 workers。 |
| `doujin-launcher` | Windows 啟動、停止、重啟、catalog 選擇與瀏覽器開啟。 |
| `doujin-desktop` | 原生視窗版；同一 process 內嵌本機服務。 |
| `doujin-app` | 掃描、metadata、檔案操作、重複判定、改名與匯出的 use cases。 |
| `doujin-storage` | SQLite schema、repository、搜尋索引與稽核資料。 |
| `doujin-parser` | 檔名解析 library 與 JSON CLI。 |
| `doujin-scanner` | 收藏發現、排除規則與安全檔名正規化。 |
| `doujin-thumbnails` | ZIP 圖片選取、資源限制、縮放與 WebP cache。 |
| `doujin-files` | 安全開啟、搬移、刪除與中斷操作復原。 |
| `doujin-provider-ehentai` | E-Hentai／ExHentai metadata provider。 |
| `doujin-provider-dlsite` | DLsite 精確 RJ 與保守 fallback provider。 |
| `doujin-migrate` | 舊 catalog 唯讀遷移與 v2 path audit。 |

## 授權

本專案採用 [MIT License](LICENSE)。

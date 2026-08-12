# 相似專案參考資料

最後查核：2026-08-12

## 研究範圍

本文件收錄與下列能力重疊的開源專案：

- 本機漫畫／同人誌收藏管理
- 壓縮檔與圖片資料夾掃描
- 檔名 metadata 解析
- 標籤、搜尋、縮圖與人工修正
- Rust parser 或 Rust 背景處理架構

這些專案用於需求探索、BDD 情境盤點與架構比較；除非授權相容且另外完成技術評估，不代表要引入其程式碼。

## 專案比較

| 專案 | 與本專案重疊之處 | 主要參考價值 | 不直接採用的理由 | 授權 |
|---|---|---|---|---|
| [LANraragi](https://github.com/Difegue/LANraragi) | 漫畫／同人誌壓縮檔、縮圖、namespaced tags、分類、metadata plugins、重複偵測 | 最接近產品領域；可用來盤點收藏匯入、搜尋、標記與 plugin 情境 | 主要以 Redis、Perl 與 configurable regex 為核心，沒有我們預期的候選值、信心度與人工裁決模型 | MIT |
| [chaptr](https://github.com/johnthreekay/chaptr) | Rust 漫畫／輕小說 filename tokenizer 與 parser | 共用 lexer、第二層 classifier、typed output、corpus 與 regression tests | 欄位集中在 volume、chapter、group、language，沒有 event、circle、author、parody | MIT / Apache-2.0 |
| [HappyPanda X](https://github.com/happypandax/happypandax) | Manga／doujinshi 管理、E-Hentai 式搜尋、namespaced tags、metadata plugins、archive importer | 領域詞彙、收藏分組、搜尋與 importer UX | Alpha 專案，公開版本與主要開發活動已停滯，不適合作為新實作基底 | LGPL-3.0 |
| [Omnibus](https://github.com/hankscafe/omnibus) | SQLite、掃描、metadata matching、人工確認、Rust engine | Confidence mode、unmatched queue、self-describing library 與 Rust sidecar 邊界 | 偏美式漫畫／一般 manga；GPL-3.0 與目前 MIT 專案的程式碼採用界線不同 | GPL-3.0 |
| [Codex](https://github.com/ajslater/codex) | 檔案監控、全文搜尋、批次 metadata 編輯、線上 matching、資料重建 | 將可由掃描重建的資料與不可覆寫的使用者資料分離 | 偏一般漫畫與閱讀伺服器，parser 領域不同 | GPL-3.0 |
| [Komga](https://github.com/gotson/komga) | 漫畫資料庫、metadata 編輯、embedded metadata、重複偵測、REST API | 成熟的 library／series／book 模型與 ComicInfo 生態 | 核心單位偏系列與冊數，不以同人誌檔名解析為中心 | MIT |
| [Manga-Tagger](https://github.com/Inpacchi/Manga-Tagger) | 批次 metadata、重新命名、寫入 ComicInfo.xml | Metadata 持久化與可攜式 sidecar 情境 | 只支援 CBZ，且不提供完整收藏瀏覽與人工裁決流程 | MIT |

## 直接相關的實作與規格素材

### 外部 metadata provider

DLsite 相關 Rust／Python client、Playnite extension、公開 request 介面、欄位映射與第一個 provider 的選型，另見 [外部 metadata provider 選型](external-metadata-provider-evaluation.md)。結論是先實作精確 RJ lookup，不移植 Google 結果頁 scraping，也不讓模糊搜尋自動採用第一筆。

### LANraragi RegexParse

[RegexParse filename plugin](https://github.com/Difegue/LANraragi/blob/dev/lib/LANraragi/Plugin/Metadata/RegexParse.pm) 的預設命名模型為：

```text
(Event) [Artist] TITLE (Series) [Language]
```

它證明此命名習慣具有現成先例，也顯示單一 regex 方案的界線：規則可以配置，卻不自然表達候選值、判斷證據、歧義與「寧可不猜」的結果。

### chaptr

[chaptr](https://github.com/johnthreekay/chaptr) 將工作分成共享 lexical layer 與 manga／novel classifiers。其 repository 包含 unit tests、regression tests、benchmark，以及取自 Kavita、Mihon、Nyaa 的 corpus。

可借鏡的原則：

- Parser 保持純函式與無 I/O。
- Tokenization 與領域分類分層。
- 無法識別的欄位使用空值，不讓整次解析失敗。
- 真實檔名 corpus 與具名 regression case 分開管理。
- 數字、範圍與 revision 使用明確型別，不以模糊字串承載。

chaptr 的 corpus 有各自授權；若未來要複製測試資料，必須逐一保留來源與授權資訊。現階段只參考測試策略。

### Omnibus

[Omnibus](https://github.com/hankscafe/omnibus) 將 Web app 與 Rust engine 分開，Rust engine 負責掃描、壓縮檔處理、metadata 與搜尋。它的 Match Confidence Mode、unmatched queue、ComicInfo.xml 與 `series.json` 行為，可作為以下 BDD 問題的參考：

- 系統何時可以自動接受 metadata？
- 什麼結果必須送交人工確認？
- 重新掃描時，哪些資料可以重建？
- 使用者修正如何避免被 parser 或外部來源覆蓋？

## 對本專案的採用界線

1. BDD 可以描述其他產品已證明有價值的行為，但必須由本專案使用者重新確認。
2. MIT／Apache-2.0 來源仍需保留 attribution 並檢查檔案層級授權。
3. GPL／LGPL 專案目前只研究公開行為與架構，不複製程式碼。
4. 既有專案的 regex、資料模型或 UI 不是本專案的預設答案。
5. Rust v2 的 parser corpus 優先來自本專案實際收藏與人工確認結果。

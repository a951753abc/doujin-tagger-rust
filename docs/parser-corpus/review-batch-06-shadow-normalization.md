# Parser Corpus 審閱批次 06：影子比對與正規化邊界

> 審閱狀態：已接受（2026-08-12）；case-025 依使用者裁決改為成功解析後重新命名實體 ZIP

本批次來自 13,566 筆既有收藏的唯讀 shadow comparison。以下案例只處理 Python 與 Rust parser 尚未由既有 BDD 明確裁決的差異；尾端圓括號、巢狀作者、破損作者結構等已確認規則不在本批重問。

各差異標籤可能重疊，因此筆數不能直接相加。案例內的「實際差異」是批次 06 實作前的首次盤點數字；[Python／Rust Parser Shadow Comparison](shadow-comparison-v1.md) 目前保存實作後的最新結果。

## parser-v2-case-025：URL percent-encoded 檔名

```text
%28C77%29%20%28%E5%90%8C%E4%BA%BA%E8%AA%8C%29%20%5B%E9%9B%B7%E7%A5%9E%E4%BC%9A%5D%20%E3%83%A9%E3%83%96%E3%83%9E%E3%83%8A%E3%82%AB%20%28%E3%83%A9%E3%83%96%E3%83%97%E3%83%A9%E3%82%B9%29.zip
```

建議行為：

- 永遠在重新命名紀錄中保留原始檔名。
- 結構解析前，對合法 `%HH` 序列做一次 UTF-8 percent decoding。
- 無效 percent encoding 保持原文並留下解析理由，不猜測修復。
- 新收藏以解碼結果成功解析出場次、分類或創作者結構後，將同目錄的實體 ZIP 重新命名為解碼後的檔名，再以新路徑建立索引。
- 解碼結果必須是合法的單一檔名；若包含路徑分隔符、Windows 禁用字元或保留名稱，不得重新命名。
- 若目標檔名已存在或檔案系統重新命名失敗，不得覆寫目標；保留原檔並回報待處理。
- 本例解碼後的場次為 **C77**、社團為 **雷神会**、標題為 **ラブマナカ**。
- 尾端 **ラブプラス** 仍依 DEC-002 要求證據；沒有 evidence 時保存為 other info。

實際差異：5 筆。

## parser-v2-case-026：來源前綴不是社團

```text
[firelee@2DJGAME](C78) (同人誌) [へらぶな(いるまかみり)] ツマA+ツマB.zip
```

建議行為：

- leading bracket 符合 `[名稱@來源]` 時，先視為 **source marker 候選**，不直接當社團。
- 完整原文 `[firelee@2DJGAME]` 保存為 ignored segment，kind 為 `source_marker`，不得直接丟棄。
- 後續仍解析場次 **C78**、社團 **へらぶな**、作者 **いるまかみり** 與標題 **ツマA+ツマB**。
- 若後續沒有可辨識的收藏結構，source marker 候選不能單靠 `@` 自動成立，應保留為未分類資訊。

實際差異：197 筆。

## parser-v2-case-027：底線不做全域空白轉換

```text
[Circle_Name] Work_Title.zip
```

建議行為：

- 社團保存為 **Circle_Name**。
- 標題保存為 **Work_Title**。
- Parser 不把 `_` 全域改成空白，避免破壞正式名稱、顏文字或識別字。
- 搜尋可以另做 `_` 與空白的寬鬆匹配，但不得改寫保存值。

實際差異：547 筆；其中部分同時包含其他差異。

## parser-v2-case-028：標記必須完整匹配

```text
[Digital Lover] D.L. action 56.zip
[社團] 作品 [Digital].zip
[社團] 作品 [Dl版].zip
```

建議行為：

- `[Digital Lover]` 是社團，不得因開頭為 `Digital` 而被 distribution marker 規則吞掉。
- 只有完整 bracket 內容等於已知標記時才辨識標記，不使用前綴或子字串匹配。
- `[Digital]` 與大小寫不同的 `[Dl版]` 都保存為 distribution marker，並令 `is_dl` 為 **true**。
- 標記比對使用 Unicode-aware case-insensitive matching，但保存原始拼法。

舊 Python 規則誤吞 `Digital…` 社團的實際差異：120 筆。

## parser-v2-case-029：已知標記後方還有未知標記

```text
[社團] 作品名稱 (原作候選) [DL版] [音声付き].zip
```

建議行為：

- 即使 `[DL版]` 後方還有未知標記，仍應辨識 `[DL版]`，令 `is_dl` 為 **true**。
- `[音声付き]` 不可丟棄；在尚未定義專用種類時保存為 other info。
- `[音声付き]` 不應阻止前方已知標記或原作候選的解析。
- `原作候選` 仍依 DEC-002 的 evidence 規則決定原作或 other info。

非尾端已知標記差異：41 筆；另有 11 筆 Python 判定為 DL、Rust 尚未判定為 DL。

## parser-v2-case-030：未知尾端方括號保存為其他資訊

```text
[社團] 作品名稱 [無毒漢化組].zip
```

建議行為：

- 標題為 **作品名稱**。
- `[無毒漢化組]` 不應被無條件刪除，也不應直接併入標題。
- 完整原文保存為 other info，reason 為 `unclassified_trailing_marker`。
- 未來外部 metadata 或人工裁決可將它提升為翻譯組、版本或其他正式欄位。

尾端方括號政策差異：317 筆；其中可能包含已知與未知標記。

## parser-v2-case-031：解析全形括號但不改寫標題字形

```text
★五月女レイナ編（セリフ、効果音付き）本編
```

建議行為：

- 保存標題為 **★五月女レイナ編（セリフ、効果音付き）本編**。
- Parser 在結構判斷時把 `（）` 與 `()` 視為等價分隔符，但不因此把標題內的全形括號改寫成半形。
- 原始字形與結構解析使用的正規化視圖應分開保存或處理。

全形括號字形差異：15 筆。

## 已確認、不需重問的差異

- 無 evidence 的尾端圓括號進入 other info：831 筆。
- Rust 支援巢狀作者、舊 Python 無法拆分：90 筆。
- Rust 新增的明確 DL 偵測：26 筆。
- 無法可靠拆作者時轉外部 metadata 流程：14 筆。
- 括號破損不應沿用舊 Python 的貪婪場次猜測：31 筆。

## 回覆格式

本批次已全部接受；原回覆格式保留供審閱紀錄：

```text
批次 06 接受
```

若有修改：

```text
case-025：percent encoding 不要自動解碼，原因是……
case-030：未知尾端方括號應保留在標題，原因是……
```

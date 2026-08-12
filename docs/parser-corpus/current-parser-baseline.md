# 現有 Python Parser 對照基線

以第一批 24 筆 draft corpus 比較現有 `parse_filename()` 的核心輸出：場次、社團、作者原文、標題、原作與 DL 標記。

目前有 5 筆核心輸出與 Rust v2 草稿預期不同：

| 案例 | 現有行為 | Rust v2 草稿 |
|---|---|---|
| `parser-v2-case-014` | 無法拆出巢狀作者，把整段當社團 | 社團為 `macdoll`，作者保留為 `士嬢マコ(・c_・ )` |
| `parser-v2-case-015` | 把破損 bracket 全部當社團 | 不猜社團／作者，保存 other info 並要求外部補足 |
| `parser-v2-case-016` | 把非尾端作者括號的 bracket 全部當社團 | 不猜社團／作者，保存 other info 並要求外部補足 |
| `parser-v2-case-017` | 無證據仍把 `角色名稱` 當原作 | 原作保持空白，`角色名稱` 歸入 other info |
| `parser-v2-case-021` | 裸 `DL版` 留在標題且未標記 DL | 從標題移除並標記為 DL 版 |

這個數字沒有涵蓋資料模型差異。現有 parser 尚不能輸出作者清單、原始值與 canonical、other info、ignored segments、identifier、解析狀態或下一步，因此即使核心文字相同，Rust v2 仍需要新的結構化輸出。

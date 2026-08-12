# Parser Corpus 審閱批次 03：巢狀與破損括號

> 審閱狀態：已接受（2026-08-12）

本批次包含 `parser-v2-case-014` 至 `parser-v2-case-016`。核心原則是：平衡的外層括號可以拆，但不遞迴解讀作者名稱；結構破損或作者括號不在尾端時，不猜社團與作者。

## parser-v2-case-014：作者名稱含巢狀括號

```text
(C105) [macdoll (士嬢マコ(・c_・ ))] 挿しつ射されつふたなり姉妹 (オリジナル).zip
```

- 場次：**C105**
- 社團：**macdoll**
- 作者原文：**士嬢マコ(・c_・ )**
- 作者清單只有一項：**士嬢マコ(・c_・ )**
- 不再把 `(・c_・ )` 當成另一層 metadata。
- 解析狀態：**complete**

## parser-v2-case-015：多出一個右括號

```text
(C104) [70 Nenshiki Yuukyuu Kikan (Ohagi-san))] Ripe flower buds.zip
```

- 場次：**C104**
- 社團／作者：**都保持空白**
- 標題：**Ripe flower buds**
- Other info：**70 Nenshiki Yuukyuu Kikan (Ohagi-san))**
- 原因：**malformed_circle_author**
- 解析狀態：**partial**
- 下一步：**嘗試外部 metadata**；外部結果仍不可靠時才送人工確認。

## parser-v2-case-016：作者括號不在 bracket 尾端

設計案例：

```text
(C100) [社團 (作者) 補充] 作品名稱.zip
```

- 場次：**C100**
- 社團／作者：**都保持空白**
- 標題：**作品名稱**
- Other info：**社團 (作者) 補充**
- 原因：**author_parenthesis_not_at_tail**
- 解析狀態：**partial**
- 下一步：**嘗試外部 metadata**；外部結果仍不可靠時才送人工確認。

## 回覆格式

全部正確時可以直接回覆：

```text
批次 03 接受
```

若有修改：

```text
case-014：巢狀作者應改成……
case-015：社團仍應填入……
```

# Parser Corpus 審閱批次 04：尾端括號與版本標記

> 審閱狀態：已接受（2026-08-12）

本批次包含 `parser-v2-case-017` 至 `parser-v2-case-022`。核心原則是尾端括號必須有原作證據；否則保存為 other info。已知版本標記可以忽略，但不得吞掉前面的原作或標題內容。

## parser-v2-case-017：沒有原作證據

```text
[社團] 作品名稱 (角色名稱).zip
```

- 社團：**社團**
- 標題：**作品名稱**
- 原作：**空白**
- Other info：**角色名稱**
- 原因：**insufficient_parody_evidence**

## parser-v2-case-018：具有 canonical alias 證據

```text
[社團] 作品名稱 (ポケモン).zip
```

- 社團：**社團**
- 標題：**作品名稱**
- 原作 raw：**ポケモン**
- 原作 canonical：**ポケットモンスター**
- 證據：**confirmed_alias**

## parser-v2-case-019：原作後方還有版本標記

```text
(C93) [翼 (緋ノ丘シュウジ)] 倫理崩壊 (Fate Grand Order) (修正版).zip
```

- 標題：**倫理崩壊**
- 原作 raw：**Fate Grand Order**
- 原作 canonical：**Fate/Grand Order**
- `(修正版)`：保存為 **version_marker**，不屬於標題或原作。

## parser-v2-case-020：標題本身含括號

```text
(C100) [Part K (羊羽忍)] 愛されたいカーマは素直になれない。 (※チョロい) (Fate Grand Order).zip
```

- 標題：**愛されたいカーマは素直になれない。 (※チョロい)**
- 原作 raw：**Fate Grand Order**
- 原作 canonical：**Fate/Grand Order**
- `(※チョロい)` 保留在標題，不應因為也是括號就被移除。

## parser-v2-case-021：全形原作括號與裸 DL 標記

```text
[clear glass (めにも)] ヒュプノスとタナトスのアリス DL版 （神様のメモ帳）.zip
```

- 社團／作者：**clear glass／めにも**
- 標題：**ヒュプノスとタナトスのアリス**
- 原作：**神様のメモ帳**
- 裸 `DL版`：從標題移除、保存為 **distribution_marker**，並將 `is_dl` 設為 **true**。
- 全形 `（）` 與半形 `()` 使用相同的結構判斷。

## parser-v2-case-022：只有版本標記，沒有原作

```text
(C92) [みくろぺえじ (黒本君)] JC拉致って性教育2 (別スキャン).zip
```

- 社團／作者：**みくろぺえじ／黒本君**
- 標題：**JC拉致って性教育2**
- 原作：**空白**
- `(別スキャン)`：保存為 **version_marker**。

## 回覆格式

全部正確時可以直接回覆：

```text
批次 04 接受
```

若有修改：

```text
case-017：角色名稱應改成原作，因為……
case-020：(※チョロい) 不屬於標題，應改成……
```

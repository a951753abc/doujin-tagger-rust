# Python／Rust Parser Shadow Comparison

- 產生時間：2026-08-12T01:17:03+08:00
- 資料庫：`L:\doujin-tagger\doujin.db`
- Rust binary：`L:\doujin-tagger-rust\target\debug\doujin-parser.exe`
- 比較筆數：13,566
- 資料庫 size／mtime 檢查未變更：是
- Rust 輸入未提供原作 evidence；原作差異獨立統計，不計入核心結構差異。
- 作者比較使用 `authors.raw`，不把 Rust 新增的作者清單視為差異。

## 摘要

- 核心欄位完全相同：12,448（91.76%）
- 至少一個核心欄位不同：1,118（8.24%）
- 原作結果不同：10,931
- 其中屬於缺少 evidence：10,917

## 核心差異欄位

| 欄位 | 筆數 |
|---|---:|
| `title` | 883 |
| `circle` | 510 |
| `author_raw` | 176 |
| `is_dl` | 35 |
| `event` | 23 |

## 差異組合

| 欄位組合 | 筆數 |
|---|---:|
| `title` | 547 |
| `circle, title` | 236 |
| `circle` | 111 |
| `circle, author_raw` | 84 |
| `circle, author_raw, title` | 66 |
| `is_dl` | 19 |
| `title, is_dl` | 16 |
| `author_raw` | 15 |
| `event, circle, author_raw, title` | 9 |
| `event, title` | 5 |
| `event` | 4 |
| `event, circle, title` | 3 |
| `event, circle` | 1 |
| `event, author_raw` | 1 |
| `author_raw, title` | 1 |

## 初步原因標籤

| 原因 | 筆數 |
|---|---:|
| `trailing_parentheses_moved_to_other_info` | 719 |
| `underscore_normalization_difference` | 541 |
| `legacy_skip_tag_consumes_circle` | 120 |
| `trailing_square_bracket_policy_difference` | 95 |
| `nested_author_supported` | 90 |
| `expanded_dl_detection` | 29 |
| `leading_source_tag_difference` | 27 |
| `malformed_delimiter_difference` | 23 |
| `creator_deferred_to_external_metadata` | 15 |
| `fullwidth_trailing_marker_moved_to_other_info` | 14 |
| `fullwidth_parenthesis_normalization_difference` | 13 |
| `legacy_dl_marker_not_recognized` | 6 |
| `nonterminal_known_marker_difference` | 6 |
| `duplicate_archive_extension_difference` | 1 |

## Rust 分類分布

| 分類 | 筆數 |
|---|---:|
| `同人誌` | 12,378 |
| `CG` | 923 |
| `商業誌 / 成年コミック` | 263 |
| `商業誌 / 官能小説` | 2 |

## Rust Parse Status

| 狀態 | 筆數 |
|---|---:|
| `complete` | 13,551 |
| `partial` | 15 |

## 核心差異範例

### 1. DB id 1522

- 檔名：`[firelee@2DJGAME](C78)_(同人誌)_[おかりな(ぢょんたいらん)]_COWAREMONO#12__(オリジナル).zip`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`leading_source_tag_difference, trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "ぢょんたいらん", "circle": "おかりな", "event": "C78", "is_dl": false, "subcategory": null, "title": "COWAREMONO#12"}`
- Rust：`{"author_raw": null, "circle": "firelee@2DJGAME", "event": null, "is_dl": false, "subcategory": null, "title": "(C78)_(同人誌)_[おかりな(ぢょんたいらん)]_COWAREMONO#12__"}`

### 2. DB id 1523

- 檔名：`[firelee@2DJGAME](C78)_(同人誌)_[床子屋(鬼頭えん)]_ED×WIN_3_(鋼の錬金術師).zip`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`leading_source_tag_difference, trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "鬼頭えん", "circle": "床子屋", "event": "C78", "is_dl": false, "subcategory": null, "title": "ED×WIN 3"}`
- Rust：`{"author_raw": null, "circle": "firelee@2DJGAME", "event": null, "is_dl": false, "subcategory": null, "title": "(C78)_(同人誌)_[床子屋(鬼頭えん)]_ED×WIN_3_"}`

### 3. DB id 1524

- 檔名：`[firelee@2DJGAME](C78)_(同人誌)_[床子屋(鬼頭えん)]_どたんばせとぎわ崖っぷち_17_(よろず).zip`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`leading_source_tag_difference, trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "鬼頭えん", "circle": "床子屋", "event": "C78", "is_dl": false, "subcategory": null, "title": "どたんばせとぎわ崖っぷち 17"}`
- Rust：`{"author_raw": null, "circle": "firelee@2DJGAME", "event": null, "is_dl": false, "subcategory": null, "title": "(C78)_(同人誌)_[床子屋(鬼頭えん)]_どたんばせとぎわ崖っぷち_17_"}`

### 4. DB id 4626

- 檔名：`（C90） [瓢屋 (もみお)] 実在性グランブルーファンタジーMANIAC.zip`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`fullwidth_parenthesis_normalization_difference, nested_author_supported`
- Python：`{"author_raw": null, "circle": null, "event": null, "is_dl": false, "subcategory": null, "title": "(C90) [瓢屋 (もみお)] 実在性グランブルーファンタジーMANIAC"}`
- Rust：`{"author_raw": "もみお", "circle": "瓢屋", "event": "C90", "is_dl": false, "subcategory": null, "title": "実在性グランブルーファンタジーMANIAC"}`

### 5. DB id 9251

- 檔名：`(Gunrei Bu Shuho & Houraigekisen! Yo-i! Goudou Enshuu) [clesta (Cle Masahiro)] CL-orz 34 (Kantai Collection -KanColle-) [Chinese] [空気系☆漢化].zip`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`trailing_square_bracket_policy_difference, trailing_parentheses_moved_to_other_info, nested_author_supported`
- Python：`{"author_raw": null, "circle": null, "event": null, "is_dl": false, "subcategory": null, "title": "(Gunrei Bu Shuho & Houraigekisen! Yo-i! Goudou Enshuu) [clesta (Cle Masahiro)] CL-orz 34"}`
- Rust：`{"author_raw": "Cle Masahiro", "circle": "clesta", "event": "Gunrei Bu Shuho & Houraigekisen! Yo-i! Goudou Enshuu", "is_dl": false, "subcategory": null, "title": "CL-orz 34"}`

### 6. DB id 9467

- 檔名：`(Gunrei Bu Shuho & Houraigekisen! Yo-i! Goudou Enshuu 2Senme) [DogStyle (Menea The Dog)] Kore de Fini~sh  (Kantai Collection -KanColle-).zip`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, nested_author_supported`
- Python：`{"author_raw": null, "circle": null, "event": null, "is_dl": false, "subcategory": null, "title": "(Gunrei Bu Shuho & Houraigekisen! Yo-i! Goudou Enshuu 2Senme) [DogStyle (Menea The Dog)] Kore de Fini~sh"}`
- Rust：`{"author_raw": "Menea The Dog", "circle": "DogStyle", "event": "Gunrei Bu Shuho & Houraigekisen! Yo-i! Goudou Enshuu 2Senme", "is_dl": false, "subcategory": null, "title": "Kore de Fini~sh"}`

### 7. DB id 9474

- 檔名：`(Kuchiku shiteyaru!~Nano Desu! RikuKaiKuu Goudou Enshuu 2) [Nagiyamasugi (Nagiyama)] KanMusu Ryoujoku 7 ~I... Inazuma no Oshioki wo Miru no desu...~ (Kantai Collection -KanColle-).zip`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, nested_author_supported`
- Python：`{"author_raw": null, "circle": null, "event": null, "is_dl": false, "subcategory": null, "title": "(Kuchiku shiteyaru!~Nano Desu! RikuKaiKuu Goudou Enshuu 2) [Nagiyamasugi (Nagiyama)] KanMusu Ryoujoku 7 ~I... Inazuma no Oshioki wo Miru no desu...~"}`
- Rust：`{"author_raw": "Nagiyama", "circle": "Nagiyamasugi", "event": "Kuchiku shiteyaru!~Nano Desu! RikuKaiKuu Goudou Enshuu 2", "is_dl": false, "subcategory": null, "title": "KanMusu Ryoujoku 7 ~I... Inazuma no Oshioki wo Miru no desu...~"}`

### 8. DB id 11788

- 檔名：`（C92）[ねこのこね（タケユウ）]铃谷级改二（舰队これくしょん - 舰これ - ）`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`fullwidth_parenthesis_normalization_difference, trailing_parentheses_moved_to_other_info, nested_author_supported`
- Python：`{"author_raw": null, "circle": null, "event": null, "is_dl": false, "subcategory": null, "title": "(C92)[ねこのこね(タケユウ)]铃谷级改二(舰队これくしょん - 舰これ - )"}`
- Rust：`{"author_raw": "タケユウ", "circle": "ねこのこね", "event": "C92", "is_dl": false, "subcategory": null, "title": "铃谷级改二"}`

### 9. DB id 11957

- 檔名：`(悩蓠_) [籼尬兖纪_(_红_尬底)] [201204] 纪邸卧岙锱趱汚硌峁见氐则_约_纫冦冈岙汹邸纫冦壮_则 (冈负伫_纡_冲_氐仼) (COMIC1_6)`
- 差異欄位：`event, circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "红 尬底", "circle": "籼尬兖纪", "event": "悩蓠", "is_dl": false, "subcategory": null, "title": "[201204] 纪邸卧岙锱趱汚硌峁见氐则 约 纫冦冈岙汹邸纫冦壮 则 (冈负伫 纡 冲 氐仼)"}`
- Rust：`{"author_raw": "_红_尬底", "circle": "籼尬兖纪_", "event": "悩蓠_", "is_dl": false, "subcategory": null, "title": "[201204] 纪邸卧岙锱趱汚硌峁见氐则_约_纫冦冈岙汹邸纫冦壮_则 (冈负伫_纡_冲_氐仼)"}`

### 10. DB id 1101

- 檔名：`(C62)_(同人誌)_[ぷりおりソフト(おりみや舞)]_キッドナッパー_0001_(機動戦艦ナデシコ_他).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "おりみや舞", "circle": "ぷりおりソフト", "event": "C62", "is_dl": false, "subcategory": null, "title": "キッドナッパー 0001"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C62", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[ぷりおりソフト(おりみや舞)]_キッドナッパー_0001_"}`

### 11. DB id 1102

- 檔名：`(C64)_(同人誌)_[GALAXIST(BLADE)]_CRIMSON_CRAVEN_(侍魂).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "BLADE", "circle": "GALAXIST", "event": "C64", "is_dl": false, "subcategory": null, "title": "CRIMSON CRAVEN"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C64", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[GALAXIST(BLADE)]_CRIMSON_CRAVEN_"}`

### 12. DB id 1104

- 檔名：`(C67)_(同人誌)_[西南西ニ輝ケル星(森野ぱぴこ)]_らぐなろく夜話_Level.5_(ラグナロクオンライン)_(別スキャン_2010-03).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "森野ぱぴこ", "circle": "西南西ニ輝ケル星", "event": "C67", "is_dl": false, "subcategory": null, "title": "らぐなろく夜話 Level.5 (ラグナロクオンライン)"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C67", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[西南西ニ輝ケル星(森野ぱぴこ)]_らぐなろく夜話_Level.5_(ラグナロクオンライン)_"}`

### 13. DB id 1105

- 檔名：`(C68)_(同人誌)_[70年式悠久機関(袁藤沖人)]_時計仕掛けのメルヴェイユ_(オリジナル).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "袁藤沖人", "circle": "70年式悠久機関", "event": "C68", "is_dl": false, "subcategory": null, "title": "時計仕掛けのメルヴェイユ"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C68", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[70年式悠久機関(袁藤沖人)]_時計仕掛けのメルヴェイユ_"}`

### 14. DB id 1106

- 檔名：`(C68)_(同人誌)_[Type-G(イシガキタカシ)]_CUP_NOODLE_SONG_(ガンダムSEED_DESTINY)_(別スキャン_2010-01).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "イシガキタカシ", "circle": "Type-G", "event": "C68", "is_dl": false, "subcategory": null, "title": "CUP NOODLE SONG (ガンダムSEED DESTINY)"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C68", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[Type-G(イシガキタカシ)]_CUP_NOODLE_SONG_(ガンダムSEED_DESTINY)_"}`

### 15. DB id 1107

- 檔名：`(C68)_(同人誌)_[西南西ニ輝ケル星(森野ぱぴこ)]_らぐなろく夜話_Level.6_(ラグナロクオンライン)_(別スキャン_2010-03).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "森野ぱぴこ", "circle": "西南西ニ輝ケル星", "event": "C68", "is_dl": false, "subcategory": null, "title": "らぐなろく夜話 Level.6 (ラグナロクオンライン)"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C68", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[西南西ニ輝ケル星(森野ぱぴこ)]_らぐなろく夜話_Level.6_(ラグナロクオンライン)_"}`

### 16. DB id 1109

- 檔名：`(C69)_(同人誌)_[夢よりすてきな(久坂宗次)]_MY_PRECIOUS_(シスタープリンセス)_(別スキャン_2010-01).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "久坂宗次", "circle": "夢よりすてきな", "event": "C69", "is_dl": false, "subcategory": null, "title": "MY PRECIOUS (シスタープリンセス)"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C69", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[夢よりすてきな(久坂宗次)]_MY_PRECIOUS_(シスタープリンセス)_"}`

### 17. DB id 1111

- 檔名：`(C72)_(同人誌)_[Fetish_Children(あっぷるーと)]_ODIN_SPHERE_(オーディンスフィア).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "あっぷるーと", "circle": "Fetish Children", "event": "C72", "is_dl": false, "subcategory": null, "title": "ODIN SPHERE"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C72", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[Fetish_Children(あっぷるーと)]_ODIN_SPHERE_"}`

### 18. DB id 1112

- 檔名：`(C72)_(同人誌)_[爆乳フルネルソン(黒龍眼)]_鎧袖一触_(クイーンズブレイド)_(別スキャン_2010-01).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "黒龍眼", "circle": "爆乳フルネルソン", "event": "C72", "is_dl": false, "subcategory": null, "title": "鎧袖一触 (クイーンズブレイド)"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C72", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[爆乳フルネルソン(黒龍眼)]_鎧袖一触_(クイーンズブレイド)_"}`

### 19. DB id 1115

- 檔名：`(C74)_(同人誌)_[Zi(睦月ぎんじ)]_CodeBLUE_(コードギアス).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "睦月ぎんじ", "circle": "Zi", "event": "C74", "is_dl": false, "subcategory": null, "title": "CodeBLUE"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C74", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[Zi(睦月ぎんじ)]_CodeBLUE_"}`

### 20. DB id 1116

- 檔名：`(C74)_(同人誌)_[まごの亭(夏庵)]_カユミドメ+α_(ToHeart2).zip`
- 差異欄位：`circle, author_raw, title`
- 初步標籤：`trailing_parentheses_moved_to_other_info, underscore_normalization_difference`
- Python：`{"author_raw": "夏庵", "circle": "まごの亭", "event": "C74", "is_dl": false, "subcategory": null, "title": "カユミドメ+α"}`
- Rust：`{"author_raw": null, "circle": null, "event": "C74", "is_dl": false, "subcategory": null, "title": "_(同人誌)_[まごの亭(夏庵)]_カユミドメ+α_"}`

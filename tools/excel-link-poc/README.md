# excel-link-poc

Excel リンクモードの **技術検証ステップ1**（[`../../docs/plans/0018-excel-link-mode/plan.md`](../../docs/plans/0018-excel-link-mode/plan.md) §7）用の
再現ハーネス。**「既存ブックの一部セルだけ更新して保存したとき、他が壊れないか」** を実測する。

計画 §6.1 はこれを**最大リスク**とし「他に手を付ける前に潰す」としている。ここが崩れると設計ごと見直しになる。

## なぜこの方式か（ライブラリの API を信じない）

`.xlsx` は **XML の zip**。そこで本ハーネスは **ライブラリの主張ではなく、出力ファイルの中身を数える**。

- `inspect` / `diff` は **クレート非依存**。zip のパート一覧と XML マーカー（`<f>`・`t="array"`・
  `mergeCell`・`conditionalFormatting`・`pageSetup` 等）を数えるだけなので、**どのバックエンドも同じ物差しで裁ける**
- fixture は **実 Excel（COM）に作らせる**。ライブラリが生成したファイルでは意味がない
  （Excel は数式に `<f>` と `<v>` の両方を書き、`calcChain.xml` や `metadata.xml` を持つ）
- 最終確認は**実 Excel で開いて値を読む**。機械比較では「静かに誤った値」を拾えないため

## セットアップ

前提: **Excel がインストール済み**（fixture 生成と目視確認に COM を使う）、Rust ツールチェーン。

```bash
cd tools/excel-link-poc
pwsh -File gen-fixture.ps1     # → fixtures/rich.xlsx（Excel が20要素を生成）
```

## 使い方

```bash
# ベースラインの中身（zip パート + XML マーカー）
cargo run -- inspect fixtures/rich.xlsx

# 1セルだけ更新して保存（2つのバックエンド）
cargo run -- rmw fixtures/rich.xlsx umya   # → out/rich-after-umya.xlsx
cargo run -- rmw fixtures/rich.xlsx zip    # → out/rich-after-zip.xlsx

# 保持/欠落を表で出す。許可された差分（calcPr のみ）以外があれば非ゼロ終了する
cargo run -- diff fixtures/rich.xlsx out/rich-after-zip.xlsx   # → PASS / exit 0
cargo run -- diff fixtures/rich.xlsx out/rich-after-umya.xlsx  # → FAIL / exit 1
```

`D2`（アプリ所有列 `EC単価` 相当）に `1234` を書く。**`J2` の配列数式は意図的に遠い位置**に置いてあり、
「書き込み位置から離れたセルが壊れないか」を見る。

`diff` は**完全比較方式**（PR #20 レビュー #2）。マーカーの個数比較は偽陰性を通す
（別セルの `<v>` 書き換え、数値→shared string 化は個数が変わらない）ため、**許すのは意図した2変更だけ**とし、
それ以外はバイト一致を要求する。

- ZIP エントリ集合が**完全一致**する
- 対象シートと `workbook.xml` **以外**の全パートが**バイト一致**する
- 対象シートは「before に `D2` 変更だけを適用した XML」と**完全一致**する
- `workbook.xml` は「before に `fullCalcOnLoad="1"` だけを適用した XML」と**完全一致**する
- `fullCalcOnLoad="1"` を**値込み**で確認する（属性名の有無だけでなく）

これで別セルの値変更や意図しない追加も検出し、「zip 直編集は意図した2変更のみ」を機械的に証明できる。
違反があれば**非ゼロ終了**。判定ロジック `evaluate()` には**回帰テスト6本**（`cargo test`）があり、
別セル改変・shared string 化・`fullCalcOnLoad="0"`・パート追加・コピーパート改変がいずれも FAIL になることを
fixture 無しで確認している。

**依存は固定**: umya は `=3.0.1`、`zip` は `Cargo.lock`（コミット済み）で実測時のバージョンに固定。
クリーンチェックアウトで同じ結果が再現する。

## 既知の結果（2026-07-17 実測 / Excel 16.0 / umya-spreadsheet 3.0.1）

### 結論: **umya-spreadsheet は no-go、zip 直編集は go**

| 検証項目（plan §7 ステップ1） | umya-spreadsheet | zip 直編集 |
|---|---|---|
| 1. 一部セル更新で保持されるか | **✗ 配列数式が壊れる** | **✓ 意図した差分のみ** |
| 2. 数式セルを識別できるか | **✓**（`Array`/`Shared`/`Normal` と `ref` まで） | ✓（XML から直接） |
| 3. dirty-cell のみ更新できるか | ✗（副作用あり・下記） | **✓ 他パートはバイト単位でコピー** |
| 4. `calcPr fullCalcOnLoad` を設定できるか | **✗ API が存在しない** | **✓** |
| Excel の破損報告（`RepairedRecords`） | 0 | 0 |

### umya-spreadsheet が落としたもの

```
-- package parts: 20 -> 18 --
   [LOST] xl/calcChain.xml
   [LOST] xl/metadata.xml            ← 動的配列のメタデータ
-- workbook.xml --
   [DIFF] calcPr
          before: <calcPr calcId="191029"/>
          after : <calcPr calcId="122211"/>     ← 元の計算設定を読み捨て、固定値で上書き
   [FAIL] fullCalcOnLoad present in output: false
-- xl/worksheets/sheet1.xml --
   [LOST]   array   t="array"                2 -> 0
   [LOST]   ref= attr (array/shared range)   5 -> 3
```

**最も重大: 配列数式が静かに誤った値になる。**

| | 元ファイル | umya 出力 |
|---|---|---|
| XML | `<f t="array" ref="J2">SUM(C2:C4*D2:D4)</f>` | `<f>SUM(C2:C4*D2:D4)</f>` |
| Excel | `HasArray=True` / **3950**（正） | `HasArray=False` / **2468**（**誤**。正は 5618） |

`t="array"` を失って通常数式に降格し、暗黙のインターセクションで別の数値になる。
**書き込んだ `D2` から離れた `J2` が壊れる**ため、書き込み位置のガードでは防げない。
BOM の小計・合計でこれが起きれば**誤発注に直結**する。

原因はソース上明白（`src/structs/cell_formula.rs` の `write_to` が `CellFormulaValues::Array` のとき
`t` を書かず、`ref` は shared 経路からしか出力されない）。umya は**全パートをデシリアライズ→再シリアライズ**
する設計のため、モデル化されていない要素は保存時に落ちる。

**`calcPr` も制御できない。** `calcId="122211"` がハードコードで、`fullCalcOnLoad` はソース上コメントアウト。
リーダー側に `calcPr` のパースが無く、元の設定は読み捨てられる。
なお実測では umya 出力でも依存数式が再計算された（`E2`=2468）が、これは
**`calcId` が Excel の `191029` より古くなったことで偶然フル再計算が誘発された副作用**であり、
`fullCalcOnLoad` による設計された挙動ではない（キャッシュ値自体は古い `<v>800</v>` のまま書かれていた）。

### zip 直編集の結果

```
-- package parts: 20 -> 20 --
   [ ok ] no part lost
-- workbook.xml --
   [DIFF] calcPr
          before: <calcPr calcId="191029"/>
          after : <calcPr calcId="191029" fullCalcOnLoad="1"/>
   [ ok ] fullCalcOnLoad present in output: true
   [ ok ] definedName                        3 -> 3
-- xl/worksheets/sheet1.xml --   [ ok ] all markers unchanged
-- xl/worksheets/sheet2.xml --   [ ok ] all markers unchanged
-- xl/worksheets/sheet3.xml --   [ ok ] all markers unchanged
-- verdict --
   [PASS] no disallowed change (only calcPr may differ)   (exit 0)
```

**`calcChain.xml` は保持する。** 当初は削除していたが、それだと `[Content_Types].xml` の Override と
`xl/_rels/workbook.xml.rels` の Relationship が**存在しないパートを指す不整合パッケージ**になる
（Excel 16.0 は黙って直すが、他バージョン・LibreOffice・他ライブラリでの整合は保証されない）。
calcChain は数式の**計算順序**であって値ではなく、値セルの更新で順序は変わらないため保持して問題ない。
`fullCalcOnLoad="1"` により再計算は保証される。

実 Excel での確認:

```
RepairedRecords: 0
D2 = 1234                          (アプリが書いた)
E2 = 2468                          C2*D2 = 2*1234 — fullCalcOnLoad で正しく再計算
J2 HasArray = True / value = 5618  配列数式が保たれ、正しく再計算された
J5=CBT3-8 J6=CBT3-10 J7=SFB6-20    動的配列のスピルも健在
Shapes=2 Charts=1 Tables=1 Merged=True ColWidthB=22 Orientation=2 CondFmt=1
PrintArea=$A$1:$F$8  PrintTitleRows=$1:$1
```

**書き換えたのは `xl/worksheets/sheet1.xml` と `xl/workbook.xml` の2パートだけ。残り18パートは
バイト単位でコピー**（`raw_copy_file`、calcChain.xml を含む）。触っていないものは原理的に壊れない。

### この PoC が**証明していない**こと

zip 直編集は**方式**として成立するが、本実装には未対応の穴がある。

- `patch_cell` は `<c r="D2"` を**文字列検索**する。Excel は `r` を先頭に書くが、
  **他の writer が属性順を変えた場合はマッチしない**
- **対象セルが存在しない場合の挿入**（`<c>` / `<row>` の生成、`spans` 更新）は未実装
- **文字列値の書き込み**（`sharedStrings` か `inlineStr` か）は未検証。今回は数値のみ
- `sheet_part_for` は**シート順**で `sheetN.xml` を引いている。正しくは `r:id` リレーション経由
- 検証は**この fixture 1本**。実 BOM での確認は §7 ステップ3

## 注意

- **fixture 生成と目視確認には Excel COM が必要**なため、そこは CI では動かない。ただし
  `diff` の判定ロジックの**回帰テスト（`cargo test`）は Excel 不要**でどこでも走る
- 本ハーネスは**一時的な調査用**。恒久的な回帰テスト（plan §7.1.1）は go 判定後に
  `src-tauri` 側へフィクスチャ同梱で作る
- 一時ファイルは**このディレクトリ配下に閉じる**（`fixtures/` `out/` `target/` は `.gitignore`）。
  クラウド同期フォルダには書かない
- `src-tauri` には手を付けていない。go/no-go が出るまで本体クレートの依存を汚さないため

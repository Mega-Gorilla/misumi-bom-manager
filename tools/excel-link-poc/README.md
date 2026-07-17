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

# 保持/欠落を表で出す
cargo run -- diff fixtures/rich.xlsx out/rich-after-umya.xlsx
cargo run -- diff fixtures/rich.xlsx out/rich-after-zip.xlsx
```

`D2`（アプリ所有列 `EC単価` 相当）に `1234` を書く。**`J2` の配列数式は意図的に遠い位置**に置いてあり、
「書き込み位置から離れたセルが壊れないか」を見る。

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
-- package parts: 20 -> 19 --
   [LOST] xl/calcChain.xml            ← 意図的に削除（Excel が再構築する）
-- workbook.xml --
   [DIFF] calcPr
          before: <calcPr calcId="191029"/>
          after : <calcPr calcId="191029" fullCalcOnLoad="1"/>
   [ ok ] fullCalcOnLoad present in output: true
   [ ok ] definedName                        3 -> 3
-- xl/worksheets/sheet1.xml --   [ ok ] all markers unchanged
-- xl/worksheets/sheet2.xml --   [ ok ] all markers unchanged
-- xl/worksheets/sheet3.xml --   [ ok ] all markers unchanged
```

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

**触ったのは `xl/worksheets/sheet1.xml` と `xl/workbook.xml` の2パートだけ。残り17パートはバイト単位でコピー**
（`raw_copy_file`）。触っていないものは原理的に壊れない。

### この PoC が**証明していない**こと

zip 直編集は**方式**として成立するが、本実装には未対応の穴がある。

- `patch_cell` は `<c r="D2"` を**文字列検索**する。Excel は `r` を先頭に書くが、
  **他の writer が属性順を変えた場合はマッチしない**
- **対象セルが存在しない場合の挿入**（`<c>` / `<row>` の生成、`spans` 更新）は未実装
- **文字列値の書き込み**（`sharedStrings` か `inlineStr` か）は未検証。今回は数値のみ
- `sheet_part_for` は**シート順**で `sheetN.xml` を引いている。正しくは `r:id` リレーション経由
- 検証は**この fixture 1本**。実 BOM での確認は §7 ステップ3

## 注意

- **Excel COM が必要**なため CI では動かない。本ハーネスは**一時的な調査用**であり、
  恒久的な回帰テスト（plan §7.1.1）は go 判定後に `src-tauri` 側へフィクスチャ同梱で作る
- 一時ファイルは**このディレクトリ配下に閉じる**（`fixtures/` `out/` `target/` は `.gitignore`）。
  クラウド同期フォルダには書かない
- `src-tauri` には手を付けていない。go/no-go が出るまで本体クレートの依存を汚さないため

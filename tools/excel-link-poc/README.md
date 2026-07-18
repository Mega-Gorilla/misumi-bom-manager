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

# 保持/欠落を表で出す。許可された差分（対象セル D2 と calcPr の2変更）以外があれば非ゼロ終了する
cargo run -- diff fixtures/rich.xlsx out/rich-after-zip.xlsx   # → PASS / exit 0
cargo run -- diff fixtures/rich.xlsx out/rich-after-umya.xlsx  # → FAIL / exit 1

# --- ステップ2: 数式 PoC ---
pwsh -File gen-scenarios.ps1                        # 4パターンの BOM を生成（Excel 必要）
cargo run -- stale-scan fixtures/scenario-A.xlsx   # 全数式 stale 方針の影響を分類
cargo run -- check-write fixtures/scenario-B.xlsx B2  # 数式セルへの書き込み → BLOCK
cargo run -- check-write fixtures/rich.xlsx J6        # スピル範囲との交差 → BLOCK
cargo run -- rmw fixtures/rich.xlsx zip-nofco      # 対照: fullCalcOnLoad なしの書き込み
pwsh -File lifecycle.ps1                            # 対照実験つき stale→trusted 実 Excel ループ
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

## ステップ2の結果（2026-07-18 実測 / Excel 16.0）

数式の `stale` 運用が成立するかの検証と、**影響分類**（どこに数式があると重いか）。状態機械は
純関数として `src/state.rs` に実装し、**Excel 不要の回帰テスト**（`cargo test`）で固定。
実 Excel との一致は `lifecycle.ps1` で確認。

### (A) `stale` ライフサイクルは実 Excel で成立し、`fullCalcOnLoad` が再計算の原因である

`lifecycle.ps1` の実測（全段 assert・**`CalculateFull()` は一切呼ばない**・`restore` は §4.4.2 の
指紋ベース復元規則）。**対照実験**により `fullCalcOnLoad` の効果を分離証明した:

```
CONTROL: fullCalcOnLoad なしで書き込み → Excel で開く → E2 = 800 のまま（古いキャッシュ）
         → 開くだけでは再計算されない（§4.4.1 の問題そのものを実証）
1. アプリが EC セルを書く（zip 編集・fullCalcOnLoad 設定）→ 生キャッシュ E2=800 を確認 → Stale
2. 誰も保存しない → 指紋不変 → restore = Stale             ← 再起動しても stale 維持
3. Excel が開く「だけ」（CalculateFull なし）→ E2 = 2468 を assert  ← フラグが再計算の原因
4. 保存 → 指紋変化 → restore = Trusted、保存ファイルの生 <v> キャッシュ = 2468 を assert
== LIFECYCLE OK: control stayed stale; fullCalcOnLoad alone recalculated; Stale -> Trusted ==
```

**復元規則が効いている**（シナリオ5・6）: 未保存なら指紋が変わらず `Stale` を維持し、
単純な再読込で `Unverified` に戻らない。`Trusted` へ戻る契機は「Excel を閉じたこと」ではなく
「再計算後に保存されたファイルを再読込したこと」（＝指紋の変化）。

### (B) 書き込みガード: 数式セル自体と、スピル範囲との交差の両方を拒否する

`check-write` は2段のガード（両方 fail closed）:

1. **対象セル自身が数式**（通常・shared anchor/follower・array を問わず）→ 拒否
   （`patch_cell` は `<f>` を値で置換してしまうため。§4.4「数式セルには絶対に書き込まない」）
2. 対象セルが**配列/動的スピル範囲内**（スピル結果セルは `<f>` を持たない）→ 拒否

```
check-write scenario-B.xlsx B2 → [BLOCK] B2 is itself a formula cell（型番の数式）
check-write rich.xlsx J6       → [BLOCK] J6 intersects a spill range (J5:J7)
check-write rich.xlsx D2       → [ ok ]  plain value cell, clear of every spill
```

回帰テスト: 通常数式・shared anchor・shared follower（自己閉鎖 `<f/>`）・スピル結果セルの
いずれも拒否、値セルは許可（fixture 不要の XML ベース）。

### (C) 影響分類 — 「全数式 stale」の重さは業務列の位置で決まる

**これは影響の分類と scanner の動作確認であって、発生頻度の測定ではない**（合成 fixture の
数式配置は作成時に決めたもの。**実 BOM でどの程度発生するかは未測定**で、実 BOM を入手したら
`stale-scan <実BOM>` で同じ指標を出せる）。

代表4パターン（`gen-scenarios.ps1`）での分類結果:

| パターン | 全数式セル | うち業務列 | 影響 |
|---|---|---|---|
| A: 小計・合計が数式 | 4 | 4（小計） | 限定的（EC取得後に小計が一時未確定。Excel を開けば戻る） |
| B: 型番・数量が数式 | 9 | 9（型番・数量・小計） | **重い（EC 取得の入力自体が止まる）** |
| C: 注文番号が数式 | 6 | 6（小計・注文番号） | 中（注文処理へ影響） |
| D: 業務列は手入力 | 1（装飾 =TODAY()） | 0 | 実質なし |

→ **MVP 境界の判断**（§5 に反映）: 全数式 stale を基本方針とし、**型番・数量・注文番号の業務列に
数式がある場合はリンク時に警告する**。これは**頻度測定に基づく決定ではなく、影響分類に基づく
安全側の製品判断**として採用する（型番・数量が止まると EC 取得自体が成立しないため、頻度に
かかわらず警告する価値がある）。`stale-scan` の分類がそのまま検出ロジックになる。

### この PoC が**証明していない**こと

zip 直編集は**方式**として成立するが、本実装には未対応の穴がある。

- `patch_cell` は `<c r="D2"` を**文字列検索**する。Excel は `r` を先頭に書くが、
  **他の writer が属性順を変えた場合はマッチしない**
- **対象セルが存在しない場合の挿入**（`<c>` / `<row>` の生成、`spans` 更新）は未実装
- **文字列値の書き込み**（`sharedStrings` か `inlineStr` か）は未検証。今回は数値のみ
- `sheet_part_for` は**シート順**で `sheetN.xml` を引いている。正しくは `r:id` リレーション経由
- 検証は**この fixture 1本**。実 BOM での確認は §7 ステップ3

ステップ2固有の未検証:

- `stale-scan` の業務列判定は**ヘッダ文字列マッチ**（`型番`/`数量`/`小計`… の部分一致）。
  実 BOM の多様なヘッダには**列 role 割り当て**（本実装の構造契約 §4.7）で対応する
- **実 BOM での発生頻度は未測定**。今回できたのは合成 fixture による**影響分類**まで。
  実 BOM を入手したら `stale-scan <実BOM>` で同じ指標を出す（MVP 境界の警告方針は
  頻度ではなく影響に基づく安全判断なので、頻度測定の結果で覆る性質のものではない）
- `restore` の「指紋が変わった＝Excel が再計算した」は厳密には証明でない（§4.4.2 の残余リスク。
  第三者ツールが再計算せず保存した場合を区別できない）。実用上の妥協として受け入れる
- lifecycle の対照実験は **Excel 16.0・自動計算モードでの結果**。他バージョンの「開くだけで
  再計算しない」挙動が同一かは未確認（`fullCalcOnLoad` を立てる限り実害はない）

## 注意

- **fixture 生成と目視確認には Excel COM が必要**なため、そこは CI では動かない。ただし
  `diff` の判定ロジックの**回帰テスト（`cargo test`）は Excel 不要**でどこでも走る
- 本ハーネスは**一時的な調査用**。恒久的な回帰テスト（plan §7.1.1）は go 判定後に
  `src-tauri` 側へフィクスチャ同梱で作る
- 一時ファイルは**このディレクトリ配下に閉じる**（`fixtures/` `out/` `target/` は `.gitignore`）。
  クラウド同期フォルダには書かない
- `src-tauri` には手を付けていない。go/no-go が出るまで本体クレートの依存を汚さないため

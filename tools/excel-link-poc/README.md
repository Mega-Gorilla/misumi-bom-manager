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

# --- ステップ3＋3b: 構造変更 PoC / 保持テスト / 環境判定 ---
pwsh -File gen-mutations.ps1                        # base + 変異体20本（Excel 必要）
cargo run -- verify-structure fixtures/mutations    # 期待(ファイル名) vs 判定 → 全PASS / exit 0
python dump-testbom.py                              # TestBom → TSV（SELECT のみ・gitignore 領域）
pwsh -File gen-testbom.ps1                          # 準実 BOM 生成
cargo run -- rmw fixtures/testbom.xlsx zip H2       # EC単価列へ書き込み（セル引数化）
cargo run -- diff fixtures/testbom.xlsx out/testbom-after-zip.xlsx H2   # PASS / exit 0
pwsh -File check-env.ps1                            # junction/.lnk/Drive入口の環境判定（読み取りのみ）
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

## ステップ3＋3b の結果（2026-07-19 実測 / Excel 16.0）

構造変更の3判定（§4.9）・実ファイル保持テスト・環境判定（§4.10）。判定器は純関数として
`src/structure.rs` に実装し、**Excel 不要の回帰テスト19本**で固定。実測は COM fixture で機械照合。

### (A) 構造変更の3判定 — 変異体 21/21 が期待どおり

`gen-mutations.ps1` が base＋20変異体を実 Excel で生成（**ファイル名プレフィクスが期待判定**）。
`verify-structure fixtures/mutations` が判定と照合し、**不一致があれば非ゼロ終了**する。

```
[ ok ] base                       Safe { new_user_columns: [] }
[ ok ] safe-add-row / safe-del-row / safe-reorder-rows    Safe（行編集はヘッダに影響しない）
[ ok ] safe-new-user-col          Safe { new_user_columns: ["備考"] }
[ ok ] warn-move-col              Confirm("app-owned column moved (with: 数量, EC単価, 小計)")
[ ok ] warn-rename-header         Confirm("header renamed? '数量' -> '数'")
[ ok ] warn-rename-sheet          Confirm("target sheet renamed to 'BOM2'")
[ ok ] warn-move-header-row       Confirm("header row moved: row 1 -> row 2")
[ ok ] warn-move-app-col          Confirm("app-owned column moved ...")
[ ok ] broken-del-required-col    Broken("required column '型番' missing")
[ ok ] broken-dup-header          Broken("header '型番' appears more than once")
[ ok ] broken-del-sheet           Broken("target sheet 'BOM' deleted")
[ ok ] broken-formula-in-app-col  Broken("formula found in app-owned column 'EC単価' data area")
[ ok ] broken-rename-app-col      Broken("app-owned column 'EC単価' missing or renamed")
[ ok ] broken-combo-movedhdr-formula  Broken("formula found in app-owned ...")  ← 複合: 移動+数式
[ ok ] broken-combo-rename-delapp     Broken("app-owned column 'EC単価' ...")   ← 複合: 改名+削除
[ ok ] broken-combo-rename-formula     Broken("formula found in app-owned ...")  ← 複合: 改名+数式
[ ok ] broken-combo-rename-dup         Broken("header '型番' appears ...")       ← 複合: 改名+重複
[ ok ] safe-reorder-sheets            Safe（r:id 経由でないと別シートを読み誤判定）
[ ok ] warn-headerless-col            Confirm("column G has data but no usable header")
[PASS] every mutation judged as its file name expects   (exit 0)
```

行の増減・並べ替えは判定に**関与しない**（ヘッダが Safe なら現在行から再構築 — §4.5/§4.9 どおり）。
ユーザー列の数式（小計）は許容され、**アプリ所有列の数式だけが Broken** になることも確認。

### (B) 実ファイル保持テスト — 準実 BOM（TestBom 実データ＋リッチ要素）

`dump-testbom.py`（**SELECT のみ**）が DB から TestBom（実 MISUMI 型番・4行）を TSV へ、
`gen-testbom.ps1` が COM でリッチ要素（数式・別シート VLOOKUP・スピル・テーブル・条件付き書式・
結合セル・図形・グラフ・印刷設定）を重ねて生成。**完全な実務ファイルではない**（TestBom データ＋合成）。

`rmw zip H2`（EC単価列）→ `diff H2` → **PASS / exit 0（18パートがバイト一致）**。実 Excel で全 assert:

```
RepairedRecords=0  H2=1234  I2(=C2*H2)=1234  I7(=SUM)=2434  K2(VLOOKUP)=webview
UNIQUE spill=3  Shapes=2 Charts=1 Merged=True CondFmt=1 Ref table=1
```

書き込みガードも準実 BOM で機能: `H2`（値セル）→許可、`I2`（小計の数式）→**BLOCK**。

> 教訓（fixture 生成で2件のバグを踏んだ）: ① PS のパイプラインは配列を**フラット化**する
> （`ForEach-Object { $_ -split ... }` → 単項カンマ `,()` で防ぐ）。② TSV の数値を文字列のまま
> セルに入れると数式が **#VALUE!** で保存される。**生成後に before 側を assert で検証**してから
> 保持テストへ進む手順にした（壊れた before では保持テストが無意味なため）。

### (C) 環境判定（§4.10）— 実体解決とfail closed の判定

`resolve`（`canonicalize`）＋ `decide`（判定の権威は Rust の `link_allowed`）＋ `check-env.ps1`（実測）:

```
junction 経由      → 同一実体パスへ解決 [ok]
.lnk 経由          → 同一実体パスへ解決 [ok]
symlink            → [untested]（開発者モード/管理者が必要なため。失敗ではなく未検証と記録）
ローカル NTFS 実体  → Allow
未解決パス          → WarnNoWriteBack（fail closed）
FAT32              → WarnNoWriteBack
Google Drive 入口 H:\（FS=FAT32 を報告） → WarnNoWriteBack   ← 仮想入口は拒否
H:\マイドライブ.lnk → C:\GoogleDrive_hahahadesu（NTFS）→ Allow ← 実体は許可
```

**ドライブ文字のハードコードなし**に、入口（仮想 FS）と実体（NTFS）を判別できた。Drive へは
**読み取りのみ**（書き込み・ファイル作成は一切していない）。

**3b は部分完了**: junction / `.lnk` は成立、**symlink は本環境で作成権限がなく未検証**
（`New-Item -ItemType SymbolicLink` も管理者権限を要求 = 開発者モード無効。junction と同じ
`canonicalize` 経路なので同挙動の見込みだが、実測は開発者モード環境が用意でき次第）。

### この PoC が**証明していない**こと

- 変異体は**合成**。3判定の**判定器が仕様どおり動くこと**の証明であり、実運用の多様な編集を
  網羅したものではない（複合は**2要素×4種**を検証。3要素以上・全組合せは未検証）
- **契約は PoC 版**（`(label, ownership)` の連続列前提）。本実装の構造契約（§4.7・DB）は
  **そのままでは移設できない**: 安定キー `ColumnDef.key`・`role`（partNo/source/orderNo1-3）・
  supplier 列の `link.field`・読み飛ばし列・任意の Excel 列位置を持ち、**ヘッダ名は列の恒久的な
  識別子ではなく「最後に確認した表示名」**として扱う必要がある。移設できるのは**判定の骨格**
  （異常収集・Broken>Confirm>Safe・fail closed）
- ヘッダ検出は**共有文字列のみ**（inlineStr のヘッダは未対応。Excel 保存ファイルでは通常 sharedStrings）
- **symlink 経由は未検証**（権限）。junction/.lnk と同じ `canonicalize` 経路なので同挙動の見込みだが実測なし
- 準実 BOM は TestBom（4行）ベース。大規模 BOM・実務ファイルそのものは未検証
  （実務 BOM「YUBI Glove Assy_BOM」が DB にあり、同じコマンドで検証可能 — 実行は別途判断）
- §7.2 の「定義名・テーブルの追跡手段」「backup 別ボリューム挙動」は未実測（オプション扱い）

## ステップ3c ハーネス（2026-07-19 実装・ドライラン検証済み → Drive 実測完了・GO）

Issue #23 の試験仕様（8ケース＋プレゼンスON 2ケース・GO/条件付きGO/INCONCLUSIVE/NO-GO）を
実行するハーネス。

```
cargo run -- marker <xlsx> [ec] [user]        # 版マーカー機械読取: markers D2=.. G2=.. fp=..
cargo run -- watch-stable <f> [tOut] [tStab] [baseHex]  # 収束待ち: exit 0=収束/2=無変化/3=未収束
cargo run -- sync-init <mirrorRoot> <runId> <template>  # 専用フォルダ+sentinel+ケース11本
cargo run -- sync-guard <dir> <parentRoot> <runId> [--delete]  # 削除条件の検証（exit code が権威）

pwsh -File gen-syncpoc.ps1                    # テンプレート生成（COM）
pwsh -File run-sync-poc.ps1 -Init [-DryRun] [-MirrorRoot <path>]  # barrier はステップ分割コマンド
pwsh -File run-sync-poc.ps1 -Case 03 -Step 1  # 手順は sync-poc-runbook.md
```

**ドライラン実測（-DryRun・ローカルルート・端点Bは直接上書きで模擬）**:

```
11ケース × 全ステップ  → すべて期待どおり（合否ケースは PASS 判定・exit 0）
  02: リモート先行を指紋変化で検出し書き込み拒否（置換前の早期検出）
  03: 最終照合を通過→barrier before-replace→R1 到着→ReplaceFileW 実行→
      backupFp≠F0 の事後検出で競合（§4.2.2）。backup が R1 を保全（削除しない）
  06: 端点A無変化＋A1 生存で PASS-local（合否確定には B/Web の A1 マーカー確認が必要）
  08: ReplaceFileW の backup が B0 のまま残存
sync-guard: 正当な削除は成立 / wrong-run-id・ミラールート直上・期待ルート外の同名フォルダは違反列挙で拒否
cargo test --locked: 58 passed（sync 11本: マーカー抽出・収束状態機械〔読取不能でも期限で終了〕・guard 条件〔期待親ルート束縛含む〕）
```

**ドライランが証明していないこと**: Drive 実環境の挙動（端点B模擬は「Excel 保存済みの R1 版 fixture で
上書き」であり、実際の Drive クライアントの置換動作と同一とは限らない）→ 下の実測で解消。

## ステップ3c の結果（2026-07-19〜08-05 実測 / Drive for desktop v128 ミラー・2端点・Excel 16.0）→ **GO**

同一アカウント2PC（端点A=ハーネス・端点B=Excel 手操作のみ、指示は Issue #23 コメント経由）で
全11ケースを実測。**合否対象6ケース全 PASS・総合判定 GO**（詳細は plan.md §3.12）。

```
case-01 通常同期        PASS  ReplaceFileW 置換が新バージョンとして同期。ファイルID・共有リンク・権限維持
case-02 リモート先行     PASS  置換前の指紋照合が R1 到着を検出し書き込み拒否（早期検出）
case-03 置換前の窓       PASS  §4.2.2 事後検出が実 Drive で成立: backupFp≠F0 で競合検出・backup に外部版保全
case-04 置換後に到着     PASS  後から保存した側が新バージョンとして線形化。敗者は版履歴へ。指紋不一致で検出可
case-06 オフライン復帰   PASS  復帰端点は最新版を pull。古い B0 が A1 を上書きしない（A/B/Web 3点確認）
case-08 backup 同期     PASS  backup は全端点・Web へ伝播（→ 本実装はフォルダ外へ）。共有リンク・ID 影響なし
case-05a/05b/07 (参考)       pause 窓競合4シナリオすべて競合コピーなし。勝者=後から再開した端点。
                             敗者は版履歴のみ（Lost & Found 含め別名ファイルはゼロ）
case-91/92 プレゼンスON      表示・挙動とも OFF と完全同一（ReplaceFileW は Office プレゼンスの検知対象外）
```

**鍵となる知見**: ミラーモードの Drive は競合コピーを作らず**後勝ち線形化＋版履歴**で解決する。
Drive/Office からの警告は一切ないため、**外部変更の検出はアプリの指紋照合が唯一の手段**（§4.6.2/§4.2.2 の
設計が必然）。Web/Sheets で触るだけでもバイト列が再シリアライズされ指紋が変わる（int→float 実測）点は
fail-safe 方向の誤検出として仕様に明記する。テストルートは sync-guard 検証つきクリーンアップで削除済み・
残骸なし。セッション記録は `out/syncpoc-results.md`（gitignore・実測ログの原本）。

## 注意

- **fixture 生成と目視確認には Excel COM が必要**なため、そこは CI では動かない。ただし
  `diff` の判定ロジックの**回帰テスト（`cargo test`）は Excel 不要**でどこでも走る
- 本ハーネスは**一時的な調査用**。恒久的な回帰テスト（plan §7.1.1）は go 判定後に
  `src-tauri` 側へフィクスチャ同梱で作る
- 一時ファイルは**このディレクトリ配下に閉じる**（`fixtures/` `out/` `target/` は `.gitignore`）。
  クラウド同期フォルダへの書き込みは**ステップ3c の専用フォルダ `__mbm_sync_poc_<run-id>/` のみ**（Issue #23 で合意。削除は `sync-guard` の5条件検証を通る経路のみ）
- `src-tauri` には手を付けていない。go/no-go が出るまで本体クレートの依存を汚さないため

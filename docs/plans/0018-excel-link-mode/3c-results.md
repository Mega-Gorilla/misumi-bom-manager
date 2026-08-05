# 技術検証 3c: 実同期 PoC 実測記録（監査証跡）

Issue #23 の試験仕様に基づく実測の全記録。判定は plan.md §3.12 に固定し、本書は**ケースごとの証跡**
（時刻・マーカー・指紋・watch 結果・通知・競合コピー捜索・ID/リンク比較・判定根拠）を残す。

- 実施期間: 2026-07-19 〜 2026-08-05（JST）
- 端点A: ハーネス実行 PC。Drive for desktop ミラー実体（ローカル NTFS。本書では `<mirror-A>` と表記）
- 端点B: 2台目 PC・同一アカウント。ミラー先 `<mirror-B>`。Excel 手操作のみ（ハーネスなし）
- 両PC: Drive for desktop **v128.0.0.0** / Excel **16.0.20131.20154**
- Office リアルタイムプレゼンス: case-01〜08 は **OFF**・case-91/92 のみ **ON**
- テストルート: `<mirror-A>\__mbm_sync_poc_20260719-145101-2e4b\`（マイドライブ直下・sentinel UUID 束縛）
- 版マーカー: **B0** = D2=800/G2="B0"（テンプレート・fp `5a440da8…`）／ **A1** = D2=1234（アプリ書き込み）／
  **R1** = G2="R1"（端点Bで Excel 手入力）
- 指示・返信チャネル: [Issue #23](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23) コメント
  （🔴/🔵=端点A発の指示・🟢=端点B返信）。以下、各ケースに該当コメントをリンクする
- 共有リンク（制限付き・範囲変更なし）を case-01 / case-08 に事前作成し、置換前後の ID を比較
  （実ファイル ID は公開リポジトリには記録しない。ローカル原本 `tools/excel-link-poc/out/syncpoc-results.md` に保持）

> **証跡の所在について**: 各ケースフォルダの `log.jsonl` は、テストルートの sync-guard 検証つき
> クリーンアップ（後述）と共に削除された。本書のログ値（時刻・指紋・exit code）はセッション中に
> ハーネス出力から転記した値であり、集約原本はローカルの `out/syncpoc-results.md`（gitignore）にある。

## 合否対象

### case-01 通常同期 → **PASS**

- 07-19 15:11 Step1: ReplaceFileW 成立。F0=`5a440da8…` → target=`9799c6c5…`（A1）、backup=F0 一致（正常系）
- 端点B: D2=1234/G2=B0 到着確認（保存なし）。**`bom.backup.xlsx` も B に同期されていた**（case-08 の先行観測）
- Web: 共有リンク**開けた・ファイル ID 同一**（ReplaceFileW でも ID 維持）。版履歴2版（現行15:11・版1 14:40=テンプレート）
- 16:10 Step2: A1 生存を機械確認 → PASS
- 判定根拠: アプリ書き込みが全端点へ正常伝播・ID/リンク/権限維持・破損なし
- 証跡: [🟢 B確認](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5014804405)

### case-02 リモート更新が先 → **PASS**

- 16:10 Step1: F0=`5a440da8…`（B0）記録
- 端点B: 16:34 G2=R1 保存・アップロード確認（Web 版履歴 現行16:34）
- 16:49 Step2: watch-stable **exit 0（Converged）** — R1 到着（D2=800/G2=R1・fp=`737ec72c…`）→
  現在指紋≠F0 で**書き込み拒否** → PASS
- 判定根拠: 置換前の指紋照合が外部変更を検出し、ユーザー変更を上書きしなかった
- 証跡: [🟢 B操作](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5014888536)

### case-03 読込後・置換前にリモート反映（§4.2.2 事後検出） → **PASS**

- 16:49 Step1: F0=`5a440da8…` 記録・temp 生成・**最終指紋照合を通過** → barrier before-replace で停止
- 端点B: 17:23 G2=R1 保存・アップロード確認（Web 版履歴 現行17:23）
- 17:29 Step2: watch Converged（R1 到着）→ **F0 を取り直さず ReplaceFileW** →
  backupFp=`e0273efe…` **≠ F0** → post-replace-conflict 検出 → PASS。
  target=A1（`faabf81a…`）、**backup に R1 版（D2=800/G2=R1）を保全**（自動削除なし）
- 判定根拠: 照合〜置換の窓に入った外部変更を §4.2.2 の事後検出が実 Drive で捕捉し、外部版を保全した
- 証跡: [🟢 B操作](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5015022357)

### case-04 アプリ置換後にリモート更新到着 → **PASS**

- 手順: B同期停止 → 17:35 A が A1 置換（backup=F0 正常系）→ Web で A1 到達確認（順序保証）→
  17:42 B が B0 ベースで G2=R1 保存 → B 同期再開
- 結末: **R1 が本体になり全端点へ伝播**（A の watch exit 0・最終 D2=800/G2=R1・fp=`3ed8e508…`）。
  A1 は Web 版履歴の版2（17:35）に残存（3版: 現行17:42/版2 17:35/版1 14:40）
- 通知・競合コピー: **なし**（pause 窓の衝突は「後から保存した側が新版」として線形化）
- 判定根拠: ユーザー変更 R1 は本体として生存（サイレント消失なし）。失われた A1 はアプリ所有 EC 列で
  DB から再生成可。アプリは last_app_write 指紋（`23011dc0…`）≠現在（`3ed8e508…`）で外部変更を検出可能
- 証跡: [🟢 その1](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5015054084) /
  [🟢 その2](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5015093721)

### case-06 オフライン復帰 → **PASS**

- B停止（B0 のまま未編集）→ 07-19 20:02 A が A1 置換 → Web で A1 到達確認（現行20:02・2版）→ B 再開
- 20:41〜20:46 A 側 watch **exit 2（5分間無変化）**・A1 指紋完全一致（`31043e90…`）→
  古い B0 はクラウド/A を上書きしていない
- 07-22 端点B再確認: **B ローカル D2=1234/G2=B0**（初回報告の D2=800 は同期完了前の表示）→
  **A・B・Web の3点で A1 確認** → PASS
- 判定根拠: オフライン端点の復帰は正しく最新版を pull し、古い版が新しい版を上書きしない
- 証跡: [🟢 初回報告](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5015498179) /
  [🟢 B再確認](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5042075765)

### case-08 backup 同期 → **PASS**

- 07-22 14:42 Step1: A1 置換成立（target=`b7fc5462…`・backup=F0 `5a440da8…` 正常系）
- 端点B: `bom.backup.xlsx` **出現（D2=800/G2=B0）**・bom.xlsx=A1 → **backup はクラウド経由で B にも同期される**
- Web: 事前共有リンク**開けた**（内容=A1）・**ファイル ID 同一**・版履歴 現行14:42/版1 7/19 14:40・backup も Web 出現
- 08-05 Step2: backup 内容が B0 のまま（fp=`5a440da8…` テンプレートと完全一致・2週間経過後も不変）→ PASS
- 判定根拠: backup の保全性は成立。同期フォルダ内の backup は全端点・Web へ伝播する（→ 実装はフォルダ外へ）
- 証跡: [🟢 B/Web確認](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5042435918)

## 参考試験（合否対象外）

### case-05a 同期停止中に双方変更・A先行再開 → 観測完了

- 両PC停止 → 19:57 A が A1 置換（停止中）→ 20:04 B が R1 保存（停止中）→ **A 先行再開** → Web で A1 版確認 → B 再開
- 結末: **R1（後から再開した B のアップロード）が本体に**（Web 3版: 現行20:04/版2 19:57 A1/版1 14:40）。
  A 側 watch Converged（D2=800/G2=R1・`f4f68226…`）。競合コピー・通知なし
- 証跡: [🟢 その1](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5015459548) /
  [🟢 統合報告](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5015498179)

### case-05b 同期停止中に双方変更・B先行再開 → 観測完了（**検出不能の反例**）

- 07-22 14:09 A が A1 置換（両PC停止中・target=`2a245223…`）→ 14:14 B が R1 保存（停止中）→
  **B 先行再開**（Web 現行=R1 を確認）→ A 再開
- 結末: **A1（後から再開した A のアップロード）が本体に**。A 側 watch **exit 2（5分無変化）**・
  fp は置換直後と完全一致 = **A のファイルは一度も変化していない**。B ローカルも A1 へ置換された
- **含意（plan.md §3.12 に固定）**: 端点Bのユーザー版 R1 は本体から消えて版履歴のみに残ったが、
  アプリ側端点では現在指紋が last_app_write と一致したままのため、
  **指紋照合・通知・競合コピーのいずれでも検出できない**。非対応シナリオ（同時変更）の残余リスク
- Web 表示の注記: 版履歴の現行時刻ラベルが 14:14 のまま実体は A1、という表示と実体の食い違いを観測
  （キャッシュ回避再読込でも同じ。現行版を開いて実体 A1 を確認）。また端点Bが Sheets で確認中に
  誤編集→「元に戻す」を実施し 14:57 の復元版が履歴に追加された（検証操作由来として扱う）。
  この復元版が A へ同期され **D2 が int→float（1234→1234.0）に再シリアライズ**・指紋変化
  （`43fed705…`）— **Sheets での編集・復元操作後にバイト列が変わる**ことの実測（閲覧のみは未検証）
- 証跡: [🟢 B操作](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5042174047) /
  [🟢 Web再確認](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5042435918)

### case-07 競合コピー捜索 → 観測完了（生成されず）

- 05a と同型の競合（20:02 A1 vs 20:04 R1）→ R1 線形化（Web 3版）
- 捜索結果: 競合コピー・別名ファイル（`bom (1).xlsx` 等）は**全候補位置に出現せず** —
  ケースフォルダ・PoC ルート・ミラールート直下・端点B・Drive Web のタイトル検索
  （「見つかったファイル」「Lost & Found」「bom (1).xlsx」いずれも該当なし。
  現行 Web UI に Lost & Found 導線が見つからずタイトル限定検索で代替）
- 注: 「生成されない」の一般保証ではなく**今回の構成・シナリオでの不生成の観測**（plan.md §6.2）
- 証跡: [🟢 統合報告](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5015498179) /
  [🟢 Lost & Found](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5042075765)

## プレゼンス ON セット（実運用確認・2026-08-05）

### case-91 プレゼンス ON 通常同期（case-01 相当） → PASS

- 12:47 Step1: A1 置換正常（target=`4faefedd…`・backup=F0）。A 側に表示・干渉なし
- 端点B: D2=1234 到着（Excel で開いて確認）。**プレゼンスバッジ・通知は一切表示されず**
- 13:19 Step2: A1 生存 → PASS
- 証跡: [🟢 B確認](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5187489287)

### case-92 プレゼンス ON pause 窓競合（case-04 相当） → 観測完了

- 13:19 Step1: B 停止中に A1 置換（target=`0aae61d2…`）→ Web で A1 到達確認（現行13:19）
- 13:26:51 B が R1 保存（停止中・**プレゼンス表示なし** — 開封中・保存時とも）→ 13:31 B 再開
- 結末: R1 線形化（13:39 A 側 watch exit 0・D2=800/G2=R1・fp=`f37ce308…`）。通知・別名ファイルなし —
  **case-04（OFF 時）との差を観測せず**
- クリーンアップ前の最終証跡（Web 版履歴）: 現行=13:26（R1）/ **版2=13:19（A1）残存** / 版1=7/19 14:40
- 証跡: [🟢 B操作](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5187577011) /
  [🟢 版履歴](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5187643827)

## クリーンアップ（2026-08-05）

- `-Cleanup`: Invoke-Preflight（期待親パスの独立再導出・session.json 非依存）→ sync-guard 5条件
  （literal dir・非 reparse・canonical name・sentinel UUID 一致・期待親配下）全通過 → テストルート削除
- ローカルミラーからの消滅を機械確認。削除はクラウド・端点Bへ伝播（Drive Web ゴミ箱で確認）
- 副作用: 各ケースフォルダの `log.jsonl` もテストルートと共に削除された（上記「証跡の所在」参照）

## 総合判定 → **GO**（詳細・適用範囲・残余リスクは plan.md §3.12）

- 合否対象6ケース全 PASS。ユーザー変更のサイレント消失は合否対象で発生せず。
  ファイル ID・共有リンク・権限は置換後も維持（条件付き GO ではなく GO）
- 規定時間内収束（watch-stable 5分/10秒安定）・ファイル破損なしを全ケースで満たす
- GO の適用範囲は**単一ユーザーの順次操作＋同期遅延**（Issue #23 の合否対象）。
  非対応シナリオの検出不能リスク（case-05b）は同時編集非対応の運用警告とセットで受容
- 最終報告: [✅ 完走コメント](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/23#issuecomment-5187681134)

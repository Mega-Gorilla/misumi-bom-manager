# ドキュメント構成

| カテゴリ | 用途 | 命名 |
|---|---|---|
| `misumi-api/` | **参照（reference）**：MISUMI 内部 API の仕様・調査結果（長寿命） | 連番 `NN-トピック.md` |
| `plans/` | **計画（plan）**：機能ごとの計画・意思決定記録（フェーズで更新） | `NNNN-スラッグ/`（`NNNN`= umbrella issue 番号の 4 桁ゼロ詰め） |

## 一覧

### 参照
- [`misumi-api/`](./misumi-api/) — MISUMI 型番→単価・出荷日 API の仕様（連鎖・価格・Akamai・バッチ上限・テスト基盤）

### 計画
- [`plans/0003-bom-editor/`](./plans/0003-bom-editor/) — BOM 編集表 + Excel 取込 + MISUMI 一括取得（[Issue #3](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/3)）
- [`plans/0018-excel-link-mode/`](./plans/0018-excel-link-mode/) — Excel リンクモード：Excel を BOM の正本とし、編集権の受け渡しで同期（[Issue #18](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/18)）

## 運用メモ
- 計画フォルダ名の `NNNN` は **umbrella issue 番号**（例: Issue #3 → `0003-`）。Phase ごとの実装 issue が派生しても、計画は umbrella の 1 フォルダにまとめる。
- 参照ドキュメント（`misumi-api/`）は計画とは性質が異なるため `plans/` 配下に置かない。

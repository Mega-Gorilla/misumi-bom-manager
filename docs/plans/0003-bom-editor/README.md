# BOM エディタ 機能計画

BOM（部品表）を編集・管理し、`ORDER`（サプライヤ）に対応する行へ単価・出荷日を自動付与する機能の計画ドキュメント。
取得はプロバイダ抽象の上に載せ **Phase 1 は MISUMI を実装**（将来は他 EC へ拡張）。
単一型番ルックアップ（[Issue #1](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/1) 完了）の次フェーズ。

- 関連 Issue: [#3 BOM編集表 + Excel取込 + MISUMI一括取得（機能計画）](https://github.com/Mega-Gorilla/misumi-bom-manager/issues/3)
- ステータス: **計画中**（本ドキュメントのマージ後に Phase 1 から実装）

## ドキュメント

| ファイル | 内容 |
|---|---|
| [plan.md](./plan.md) | 詳細計画：実ファイル分析・要件・データモデル・アーキテクチャ・Excel取込設計・フェーズ計画・リスク |

## フェーズ概要

| Phase | 内容 |
|---|---|
| 1 | **SQLite データ層** + 編集表（AG Grid）+ **サプライヤ抽象＋MISUMI実装** で `ORDER=MISUMI` 一括取得 + **横断キャッシュ** |
| 2 | Excel 取込 UI（シート + ヘッダ行 + 列マッピング + プレビュー + テンプレ保存、数量倍率対応） |
| 3 | 複数 BOM ライブラリ（検索・絞り込み）・価格/納期履歴・Excel エクスポート・全行再取得・合計 |

## 決定事項（要約）

- 編集グリッド: **AG Grid Community**
- Excel 読込: **SheetJS (`xlsx`)**
- サプライヤ抽象: `SupplierProvider`（`resolve`/`quote`、transport `webview`/`http-api`、正規化 quote+raw）。**Phase1=MISUMI のみ実装**、将来 EC 追加可
- 一括取得: バックエンド `quote(supplier, items)` でプロバイダにディスパッチ（MISUMI=`MisumiCore.lookupMany`、≤100件チャンク）
- 連携: フィールド→列のリンクは**ユーザー設定**（手動/リンク/追加 ＋ 書込ポリシー `overwrite`/`fillEmpty`/`suggest`、差異ハイライト、`field`=正規化フィールドパス）
- 永続化: **SQLite を正(system-of-record)**（rusqlite、任意列/サプライヤ値は JSON1）。**JSON は import/export 専用**
- キャッシュ: 取得結果を `supplier_cache`（**(supplier,型番) キー**・全 BOM 共有）に保持し、型番入力時はキャッシュ優先

関連: MISUMI API 仕様は [`../../misumi-api/`](../../misumi-api/)、バッチ上限は [`../../misumi-api/07-batch-and-limits.md`](../../misumi-api/07-batch-and-limits.md)。

# BOM エディタ 機能計画

BOM（部品表）を編集・管理し、`ORDER=MISUMI` の行に単価・出荷日を自動付与する機能の計画ドキュメント。
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
| 1 | 編集表（データモデル + AG Grid、行/任意列の追加・編集・削除、JSON 保存）+ `ORDER=MISUMI` 一括ルックアップ |
| 2 | Excel 取込 UI（シート + ヘッダ行 + 列マッピング + プレビュー + テンプレ保存、`発注セット数` 対応） |
| 3 | Excel エクスポート・全行再取得・キャッシュ・合計（小計合計 / 最大リードタイム） |

## 決定事項（要約）

- 編集グリッド: **AG Grid Community**
- Excel 読込: **SheetJS (`xlsx`)**
- 一括取得: バックエンド `lookup_parts`（`MisumiCore.lookupMany`、≤100件チャンク）
- 永続化: BOM はローカル JSON、列マッピングは再利用テンプレ

関連: MISUMI API 仕様は [`../../misumi-api/`](../../misumi-api/)、バッチ上限は [`../../misumi-api/07-batch-and-limits.md`](../../misumi-api/07-batch-and-limits.md)。

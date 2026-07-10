# MISUMI 型番→単価・出荷日 API 仕様ドキュメント

MISUMI（ミスミ）EC サイトが、型番から単価・出荷日を自動取得する仕組みの調査結果をまとめたものです。
本アプリ（Misumi BOM Manager）で部品の最新情報を取得するための一次情報として使用します。

> ⚠️ **重要 / 免責**
> ここに記載する API は **MISUMI が公開している正式な開発者向け API ではありません**。
> EC サイト（`jp.misumi-ec.com`）が内部的に使用しているエンドポイントを、
> ブラウザのネットワーク通信を観測してリバースエンジニアリングしたものです。
> - **予告なく仕様変更・廃止される可能性があります。**
> - 利用にあたっては MISUMI の**利用規約**を確認し、**過度な連続アクセスを避けて**ください。
> - 商用・大規模・自動連携での利用は、MISUMI の **eProcurement / API 連携（法人契約）** の検討を推奨します（[06-implementation-notes.md](./06-implementation-notes.md) 参照）。

## ドキュメント一覧

| ファイル | 内容 |
|---|---|
| [01-overview.md](./01-overview.md) | 全体像・前提・対象ホスト・技術スタック |
| [02-api-flow.md](./02-api-flow.md) | 型番入力から単価・出荷日取得までの API 連鎖（4 段） |
| [03-price-delivery-check.md](./03-price-delivery-check.md) | ★核心：単価・出荷日チェック API の完全仕様 |
| [04-supporting-endpoints.md](./04-supporting-endpoints.md) | 補助エンドポイント（suggest / preview / inner / category） |
| [05-akamai-and-auth.md](./05-akamai-and-auth.md) | Akamai Bot Manager・Cookie・ログインの扱い |
| [06-implementation-notes.md](./06-implementation-notes.md) | 本アプリでの実装方針・注意事項・ToS |
| [07-batch-and-limits.md](./07-batch-and-limits.md) | バッチ（複数同時問い合わせ）の可否・件数/レイテンシ上限の実測 |
| [08-testing.md](./08-testing.md) | テスト基盤：共有コア（単一ソース）+ ヘッドレス CLI |
| [09-cart-add.md](./09-cart-add.md) | カート投入 API（`cart-detail/add`）の調査・認証設計・実装方針（Phase A/B） |

## 調査メタ情報

| 項目 | 値 |
|---|---|
| 調査日 | 2026-06-24 |
| 調査方法 | Playwright + Microsoft Edge（実ブラウザ）でネットワーク通信を観測 |
| 対象ページ | `https://jp.misumi-ec.com/order/part-number/create`（見積・注文 / 型番手入力） |
| 検証型番 | `CBT3-8`（六角穴付ボルト －高強度βチタン合金・純チタン－ / ブランド: ミスミ） |
| ログイン | **未ログインで標準カタログ価格・標準出荷日を取得可能**（顧客別契約価格はログイン時に変動の可能性） |

## 一言サマリ

型番を入力すると、**型番正規化 → カタログ概要 → 商品マスタ → 単価・出荷日計算** の順に内部 API が呼ばれる。
最終的に
`POST https://api-jp.misumi-ec.com/price-delivery-calculation/v1/sales-price-delivery/check`
に `{型番, 数量, ブランドコード}` を投げると、**単価（税別/税込）と出荷予定日**が返る。
全体は **Akamai Bot Manager** で保護されており、素の HTTP クライアントでは 403。実ブラウザ（= Tauri 内蔵 WebView2）経由が前提。

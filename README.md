# Misumi BOM Manager

部品の型番から **単価・出荷日** を取得して表示する、ローカル動作の Windows デスクトップアプリ。
MISUMI EC サイトの内部 API を、Tauri 内蔵 WebView 経由で利用する（[なぜ WebView 経由か](#アーキテクチャ)）。

- **スタック**: Tauri 2 + React 19 + TypeScript（フロント）/ Rust（バックエンド）
- **対象**: Windows
- **現状**: 型番入力 → 単価・出荷日表示（MVP）

## 開発

前提: Node.js v22 系 / Rust（stable-msvc）/ Microsoft Edge WebView2。

```bash
npm install
npm run tauri dev      # 開発起動（アプリウィンドウ + Vite）
npm run tauri build    # 配布ビルド
```

## アーキテクチャ

MISUMI のサイトは **Akamai Bot Manager** で保護されており、素の HTTP クライアントでは 403。
そこで **非表示のブリッジ WebView** に `jp.misumi-ec.com` を実ロードし（実ブラウザ＝Akamai 通過）、
そのページコンテキストで API を `fetch` する。型番→価格の fetch 連鎖は
[`shared/misumi-lookup.js`](./shared/misumi-lookup.js) に **一本化**され、アプリ本体と CLI が共用する。

```
React UI ──invoke──> Rust(lookup_part) ──eval──> 非表示ブリッジWebView(jp.misumi-ec.com)
                                                      │  shared/misumi-lookup.js を注入
                                                      ▼  suggest → sales-price-delivery/check
                          結果 <──mbm:// / IPCイベント── 単価・出荷日 JSON
```

## ディレクトリ

| パス | 内容 |
|---|---|
| `src/` | React フロントエンド（型番入力・結果表示 UI） |
| `src-tauri/` | Rust バックエンド（ブリッジ WebView・`lookup_part` コマンド） |
| `shared/misumi-lookup.js` | 型番→単価・出荷日の fetch 連鎖（**単一ソース**。アプリ/CLI 共用） |
| `tools/misumi-cli/` | ヘッドレス CLI（CLI/CI/Claude Code からの検証用） |
| `tools/misumi-api-probe/` | 低レベルな API 調査用 probe |
| `docs/misumi-api/` | MISUMI 内部 API の仕様ドキュメント（01〜08） |

## ドキュメント

- API 仕様・設計・テスト基盤: [`docs/misumi-api/`](./docs/misumi-api/)（[索引](./docs/misumi-api/README.md)）
- CLI でのテスト: [`docs/misumi-api/08-testing.md`](./docs/misumi-api/08-testing.md)

## 注意

MISUMI の **非公開な内部 API** を利用している。利用規約を順守し、過度な連続・大量アクセスを避けること。
仕様は予告なく変わり得る。商用・大規模連携は MISUMI の eProcurement / API 連携（法人契約）を検討する。

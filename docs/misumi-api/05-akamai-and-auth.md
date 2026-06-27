# 05. Akamai Bot 対策・認証・Cookie

## 概要

MISUMI EC サイトは **Akamai Bot Manager** で全面的に保護されている。
これが「素の HTTP クライアントでは API を叩けない」最大の理由であり、本アプリの設計を決定づける要素。

## 観測された Bot 対策の痕跡

| 痕跡 | 内容 |
|---|---|
| Cookie `_abck` | Akamai Bot Manager のコア判定 Cookie。無効だとブロック |
| Cookie `bm_sz`, `bm_s`, `bm_so`, `bm_ss` | Akamai セッション／センサー関連 Cookie |
| `POST /d0xgub/.../...`（難読化パス） | ブラウザ環境を収集した **`sensor_data`** を送る Akamai センサーendpoint。`{"success": true}` を返す |
| `POST /akam/13/pixel_xxxx` | Akamai のフィンガープリント収集ピクセル |

`sensor_data` はマウス挙動・画面・端末特性などを難読化エンコードしたもので、
**Akamai の難読化 JS を実ブラウザで実行して初めて有効な `_abck` が発行される**。

## 結果として

- **素の `curl` / `reqwest`（User-Agent 偽装のみ）→ ページ HTML は 200 でも、有効な `_abck` を生成できないため API 連鎖は通らない／いずれブロックされる。**
  - 実測：ブラウザ UA 付き `curl` で create ページ HTML 自体は 200・Cookie 発行までは到達するが、JS 未実行のため bot 判定を通過できない。
- **実ブラウザ（Edge/Chromium）でページを開く → Akamai JS が実行され `_abck` が有効化 → 以降の API が通る。** 本調査はこの方法で成功。

## 認証（ログイン）

- 今回の調査は **未ログイン**で実施し、**標準カタログ価格・標準出荷日を取得できた**
  （GA 計測イベントに `up.u_login_flg=no_login` を確認）。
- ログインは別ドメインの OAuth（`account.misumi-ec.com/api-auth-v1/oauth/login`）。
- **ログイン後は顧客別契約単価・数量割引・与信・専用納期**などが反映され、`sales-price-delivery/check` の返り値が変わり得る。
  - 顧客実勢価格が必要なら、ユーザー自身がアプリ内 WebView でログインする運用が必要。

## CORS

- フロント `jp.misumi-ec.com` から価格 API `api-jp.misumi-ec.com` への `fetch` は、本家サイトが実際に行っている。
  → `api-jp` 側は **`jp.misumi-ec.com` オリジンからのクロスオリジン要求を許可**していると判断できる。
- したがって、**`jp.misumi-ec.com` オリジン上で動く WebView から fetch すれば CORS は通る**。
  逆に、自前の `tauri://localhost` や `http://localhost` オリジンから直接叩くと CORS で弾かれる可能性が高い。

### 🛑 credentials の落とし穴（実測で確認）

`api-jp` への POST を **`credentials: "include"` で送ると `TypeError: Failed to fetch`** になる。
実測比較（`jp.misumi-ec.com` ページ内 `fetch`）:

| 呼び出し | 結果 |
|---|---|
| `suggest`（同一オリジン, include） | ✅ 200 |
| `price` を `credentials: "include"` | ❌ Failed to fetch |
| `price` を **資格情報なし（既定/omit）** | ✅ 200（単価取得） |

理由：`api-jp` は `Access-Control-Allow-Origin: *` を返す。CORS 仕様上、`*` と資格情報付き（`include`）は **併用不可**のためブラウザがブロックする。
→ **価格 API は資格情報なしで呼ぶ**こと（未ログインの標準価格に Cookie は不要）。
本家アプリも api-jp 呼び出しは資格情報なし。`suggest` は同一オリジンなので影響なし。

> 補足：Playwright の APIRequestContext や素の HTTP クライアントは CORS 非対象のため、`include` 相当でも通ってしまい、この罠は**ブラウザ内 fetch 特有**。WebView 実装（Tauri 本体）では必ず踏むので注意。

## 設計への含意（要点）

1. **本物のブラウザエンジンを経由する**こと（= Tauri 内蔵 WebView2）。
2. **`jp.misumi-ec.com` オリジン上**で fetch を発行すること（Akamai Cookie + CORS の両方を満たす）。
3. 詳細な実装パターンは [06-implementation-notes.md](./06-implementation-notes.md)。

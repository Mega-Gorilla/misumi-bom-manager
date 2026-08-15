// Excel リンクモードのファイル監視 (PR-7・plan.md §4.2.3) が発火する合成イベントを
// 処理するための直列キュー。App から切り出してあるのは、ここが「順序」と「取りこぼし
// 無し」という2つの不変条件を担う唯一の場所であり、単体テストで固定するため。
//
// 不変条件:
//  1. FIFO — enqueue した順にしか実行しない。保存して閉じた場合 Changed(再読込) と
//     ExcelClosed(反映待ちの再試行) が同一バーストで届くため、後者が先に走ると
//     apply が古い last_read_fp を見て fingerprint_changed になり pending が残る。
//  2. busy 中は「開始も破棄もしない」— 進行中の操作 (更新・反映・EC 取得・ウィザード)
//     が終わるまで **自分のキュー位置を保持したまま** 待つ。再キュー (末尾へ積み直し)
//     は後続タスクに追い越されるため使わない。EC 取得は数十秒に及び得るので時間上限も
//     設けず、破棄するのは isAlive() が false になったとき (エディタ離脱・BOM 切替の
//     cleanup) だけにする。
//
// 監視自体は利便性トリガであり、整合性は読込時検証と書込前の指紋照合が担う
// (§4.2.3 の責任分界)。ここで待たされた処理を最終的に落としても、UI の
// 「反映待ち・再実行」導線が受け皿として残る。

export interface WatchQueue {
  /** タスクを末尾に積む。busy の間は自分の順番のまま待機する。 */
  enqueue(isAlive: () => boolean, task: () => Promise<void>): void;
  /** 現在キューに積まれている処理がすべて終わるまでの Promise (テスト用)。 */
  settled(): Promise<unknown>;
}

export interface WatchQueueOptions {
  /** 進行中の操作があるか (true の間タスクは開始しない)。 */
  isBusy: () => boolean;
  /** 待機のスリープ (テストで差し替える)。 */
  sleep?: (ms: number) => Promise<void>;
  /** busy 監視のポーリング間隔。 */
  pollMs?: number;
}

const defaultSleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

export function createWatchQueue(opts: WatchQueueOptions): WatchQueue {
  const sleep = opts.sleep ?? defaultSleep;
  const pollMs = opts.pollMs ?? 120;
  let chain: Promise<unknown> = Promise.resolve();

  return {
    enqueue(isAlive, task) {
      chain = chain
        .catch(() => {})
        .then(async () => {
          // 自分の順番を手放さずに待つ (再キューしない = 追い越されない)。
          while (opts.isBusy()) {
            if (!isAlive()) return; // 離脱した時だけ諦める
            await sleep(pollMs);
          }
          if (!isAlive()) return;
          await task();
        })
        .catch(() => {}); // 個々の失敗でキューを止めない (次イベント・手動導線に委ねる)
    },
    settled() {
      return chain;
    },
  };
}

import { describe, expect, it } from "vitest";
import { createWatchQueue } from "./watchQueue";

/** マクロタスクを1つ跨ぐスリープ (テスト側から busy を落とせるようにする)。 */
const tick = (ms = 1) => new Promise<void>((r) => setTimeout(r, ms));

describe("createWatchQueue", () => {
  it("busy の間は開始せず、解除後に enqueue 順で実行する (Changed → ExcelClosed)", async () => {
    let busy = true;
    const order: string[] = [];
    const q = createWatchQueue({ isBusy: () => busy, sleep: () => tick(), pollMs: 1 });

    q.enqueue(
      () => true,
      async () => {
        order.push("changed");
        await tick();
      },
    );
    q.enqueue(
      () => true,
      async () => {
        order.push("excelClosed");
      },
    );

    await tick(20);
    expect(order).toEqual([]); // busy 中は何も始まらない

    busy = false;
    await q.settled();
    // 先に積まれた Changed の完了後に ExcelClosed が走る (追い越し無し)。
    expect(order).toEqual(["changed", "excelClosed"]);
  });

  it("busy が長く続いても破棄せず、解除後に必ず実行する (EC 取得が数十秒でも失わない)", async () => {
    let busy = true;
    let polls = 0;
    const order: string[] = [];
    const q = createWatchQueue({
      isBusy: () => {
        polls++;
        return busy;
      },
      sleep: () => tick(),
      pollMs: 1,
    });

    q.enqueue(
      () => true,
      async () => {
        order.push("changed");
      },
    );
    q.enqueue(
      () => true,
      async () => {
        order.push("excelClosed");
      },
    );

    // 何度ポーリングされても諦めない (時間上限は設けない設計)。setTimeout の粒度は
    // 環境依存なので、回数は「繰り返し待っている」ことが分かる程度に緩く見る。
    await tick(60);
    expect(order).toEqual([]);
    expect(polls).toBeGreaterThan(3);

    busy = false;
    await q.settled();
    expect(order).toEqual(["changed", "excelClosed"]);
  });

  it("破棄するのは離脱 (cleanup で isAlive=false) のときだけ", async () => {
    let busy = true;
    let alive = true;
    const order: string[] = [];
    const q = createWatchQueue({ isBusy: () => busy, sleep: () => tick(), pollMs: 1 });

    q.enqueue(
      () => alive,
      async () => {
        order.push("changed");
      },
    );
    await tick(10);

    alive = false; // エディタ離脱 / BOM 切替の cleanup
    busy = false;
    await q.settled();
    expect(order).toEqual([]);
  });

  it("タスクが失敗してもキューは止まらない", async () => {
    const order: string[] = [];
    const q = createWatchQueue({ isBusy: () => false, sleep: () => tick(), pollMs: 1 });

    q.enqueue(
      () => true,
      async () => {
        order.push("first");
        throw new Error("boom");
      },
    );
    q.enqueue(
      () => true,
      async () => {
        order.push("second");
      },
    );

    await q.settled();
    expect(order).toEqual(["first", "second"]);
  });
});

// GameTransport — セッション系 command とイベント push の配送路 (spec 23 Phase A)。
//
// ホスト権威の要の抽象: frontend はこの seam の向こうに「正本を持つ backend」が
// いると信じて request を送り、view DTO を受け取って描くだけ。
// - LocalTransport = Tauri invoke + listen の薄い包み (単騎/ホスト自身。挙動不変)。
// - RemoteTransport = DataChannel 越しに同じ要求をホストへ送る (Phase C で実装)。
//
// **seam に入れるのはセッション系だけ** — 設定・パッケージ管理・ログ保存などの
// ホストローカル command は従来どおり invoke 直呼び (ゲストは自分のローカル設定を使う)。
// アセット解決 (resolve_asset_path) も seam の外 — 各クライアントが**自分の**
// ローカルパッケージで解決する (Multiplayer 契約 asset_wire)。
//
// onEvent は購読解除関数を返す (HMR・画面遷移での多重購読を防ぐ)。
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** transport が中継するイベント名 (backend の emit と 1:1)。 */
export const GAME_EVENTS = ["synopsis-compacting", "synopsis-failed", "epilogue-writing"] as const;
export type GameEventName = (typeof GAME_EVENTS)[number];

export type GameEventHandler = (name: GameEventName, payload: unknown) => void;

export interface GameTransport {
  /** invoke 相当。cmd はセッション系 command 名、戻りは同じ view DTO。 */
  request<T>(cmd: string, args?: Record<string, unknown>): Promise<T>;
  /** emit 相当の push を購読する。戻り値で購読解除。 */
  onEvent(handler: GameEventHandler): () => void;
}

/** 単騎/ホスト用 — Tauri IPC の薄い包み。挙動は従来の invoke/listen と同一。 */
class LocalTransport implements GameTransport {
  request<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
    return invoke<T>(cmd, args);
  }

  onEvent(handler: GameEventHandler): () => void {
    // listen は Promise<UnlistenFn> — 解除要求が購読完了より先に来ても漏らさないよう、
    // フラグで「解除済みなら即 unlisten」を保証する。
    let dead = false;
    const unlistens: UnlistenFn[] = [];
    for (const name of GAME_EVENTS) {
      void listen(name, (ev) => handler(name, ev.payload)).then((un) => {
        if (dead) un();
        else unlistens.push(un);
      });
    }
    return () => {
      dead = true;
      for (const un of unlistens) un();
      unlistens.length = 0;
    };
  }
}

/** 現在の配送路。Phase C で join 時に RemoteTransport へ差し替える。 */
export const transport: GameTransport = new LocalTransport();

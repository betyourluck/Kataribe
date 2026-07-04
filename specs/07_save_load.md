# 07. セーブ / ロード — セッションの器を file にする

Status: **Draft rev1（Phase A+B 実装済・Phase C=GUI は査読待ち）** / 2026-07-04
Scope: 進行中のセッション（正本 state + 語りの継続性）を 1 file に保存し、後から再開できる
ようにする。動機は実プレイの実害: Gemini 長セッション（~1h, 2〜300 円分）が provider 側の
エラー（quota / 変形応答, failures.md #34）で打ち切られると全て失われる。

## 北極星との関係

- **「語り部 = 自己完結 file の純粋実行器」の完成形**。data_contract の Campaign 節が予告した
  「1 file = {骨格 + GameState + RngState}、save-state 化は型レベルで可能」を実体化する。
  `GameState`/`RngState`/`TurnLog`/`CheckOutcome`/`MemoryFragment` は全て Serialize+Deserialize
  済み — 新しい直列化コードはほぼ書かない（器を束ねるだけ）。
- **正本は 1 つ**: セーブは state の**スナップショット**であり、生きた可変コピーではない
  （ロードした瞬間に唯一の正本へ戻る）。split-brain は構造的に起きない。
- **gm_core 無改修**: file IO は harness/app 層の責務（load_package と同じ配置）。

## 設計 — SessionSave（データ契約）

```yaml
# セーブ 1 file = 再開に要る全て。骨格 (scenario) は含まない。
version: 1                      # セーブ形式版
content: { kind: package, path: packages/gnosia_village }   # 何を遊んでいたか
                                # kind: package | campaign | scenario (CLI の 3 起動形に対応)
package_version: "0.1.0"        # 記録時の manifest.version (不一致はロード時に警告、拒否しない)
module: null                    # campaign 中の現在モジュール id (単発は null)
state: { ... }                  # GameState 丸ごと (rng/turn/fired/votes/flag_turns/... 全部)
campaign_memory: { ... }        # spec 02 の persistent フラグ蓄積 (**save 永続化の積み残しを本 spec で解決**)
history: [ ... ]                # chronicle (TurnLog 全量) — GM の中期記憶。失うと「経緯を忘れる GM」に戻る
last_narration: "..."           # 直前の語り (継続性 #27 の持ち越し)
pending_checks: [ ... ]         # 直前ターンの判定結果 (次ターン還流分)
pending_lore: [ ... ]           # 発火済み recall の持ち越し (MemoryFragment 丸ごと = 自己完結)
```

### 決定事項

1. **骨格は保存しない**: scenario/campaign は package から再ロード（単一真実源・セーブ肥大回避・
   content 修正がロード後に生きる）。`content.path` + `package_version` を刻み、版不一致は
   **警告のみ**（拒否すると軽微な typo 修正でセーブが全滅する）。content が非互換に変わった
   場合の破れは engine の却下が守る（未宣言 stat への op は通らない = 正本の閉世界がそのまま
   セーフティネット）。
2. **形式は YAML 1 file**: プロジェクトの流儀（content も YAML）・人間可読・デバッグ資産。
3. **語りの継続性も保存する**: state だけでは「忘れない GM」が再開時に経緯を忘れる
   （chronicle / last_narration / pending_* は state-truth と独立の第二チャネル、#27 系）。
   history は**全量**保存（prompt 側は既存の文字予算 2400 が打ち切るので肥大の実害なし。
   file は安く、将来のリプレイ/デバッグ資産になる）。
4. **pending_lore は fragment 丸ごと**: 不変 lore の複製は害がなく、セーブが自己完結する
   （memoria/ フォルダ欠損でもロード可能）。
5. **オートセーブは受理ターン毎に上書き**: apply 後（正本確定後）に書く。クラッシュ/quota 死で
   失うのは高々 0 ターン。書き込みは tmp → rename の原子的置換（書きかけセーブで死なない）。
6. **RngState 込み = 出目まで再現**: ロード後のダイス列はセーブ時点の続きから決定論。

## Phases

- **Phase A（✅実装済 2026-07-04）**: harness `SessionSave` + `save_session`/`load_session`
  （YAML、tmp→rename 原子書き込み）+ `HarnessError::SessionLoad`。
  PoC: `session_save_roundtrips_state_and_carryovers`（進行中 state・votes・present_overrides・
  rng cursor・chronicle・pending 一式の roundtrip 同値）。
- **Phase B（✅実装済 2026-07-04）**: CLI 結線 — 受理ターン毎に `--save <path>`（既定
  `kataribe_autosave.yaml`）へ自動保存、`--resume <file>` で再開（content 種別に応じ
  package/campaign/scenario を再ロードし、state/chronicle/継続性を復元。campaign は
  current_module + campaign_memory も復元）。
- **Phase C（査読待ち）**: GUI — `play_turn` 毎に app data dir へ autosave、パッケージ選択時に
  autosave が在れば「続きから / 最初から」を提示。Tauri command `resume_game` + frontend。
- **Phase D（将来）**: 手動セーブスロット・セーブ一覧 UI・メタ表示（ターン数/場所/日時）。

## スコープ外

- セーブデータの暗号化・改竄防止（シングルプレイのローカル file、正本は engine が守る）。
- リプレイ再生（TurnLog 全量保存が将来の素材にはなる）。
- クラウド同期。

## 未決（査読事項）

1. **GUI のセーブ置き場**: app data dir（OS 標準・アンインストールで消える）vs パッケージ隣接
   （見つけやすい・配布フォルダを汚す）。推奨は app data dir + パッケージ path のハッシュで
   1 autosave。
2. **「続きから」の UX**: パッケージ選択時に自動提示（推奨）vs 明示のロードボタン。
3. **手動スロット**: Phase D に送るか、Phase C に含めるか。
4. **セーブ版数の互換ポリシー**: v1 は実験的（版上げで切り捨て可）とするか、migration を約束
   するか。推奨は「v1 は実験的」宣言（serde(default) で前方互換の余地は残る）。
5. **エラー時の自動リトライ強化**: 本 spec の外だが同じ実害への対策 — 429 の Retry-After 尊重 /
   backoff 上限引き上げ（現状 1s→10s 上限・3 回）。セーブがあれば致命傷ではなくなるため優先度は
   下がる。

# 07. セーブ / ロード — セッションの器を file にする

Status: **Done（Phase A+B+C 実装済 2026-07-04。Phase D 手動スロット実装済 2026-07-16 — ユーザー要望「気に入ったシーンから何度でもやり直す」）**
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
- **Phase C（✅実装済 2026-07-04、査読 3 点確定）**: GUI —
  - **置き場 = app data dir**（`app_data_dir/saves/<パッケージパスの FNV-1a ハッシュ>.yaml`。
    配布 zip を差し替えてもセーブが消えない・パッケージフォルダを汚さない。パッケージ別 1 autosave）
  - **書き込み**: `play_turn` の受理ターン + campaign 遷移確定後に上書き（却下では書かない、
    失敗は警告のみ = 救済機構が本体を殺さない）
  - **提示 = パッケージ選択時に自動**: `list_packages` が各パッケージの `autosave_turn` を返し、
    選択中パッケージに autosave が在ればヘッダーに「続きから (turn N)」ボタン（ember 主ボタン化、
    「新しいゲーム」は控えめ配色へ後退 = 誤クリックで上書き進行しにくい）
  - **再開**: Tauri command `resume_game` — `open_package`（new_game と共通化した scope 許可 +
    entry 分岐ロード、campaign はセーブの `module` を注入込みで開く）→ 正本 + 継続性を復元、
    `GameView.resumed {turn, last_narration, warnings}` で開幕ログに「── 続きから (turn N) ──」+
    前回までの語り + 版不一致警告を表示
  - **手動スロットは Phase D 送り**（まず「消失しない」を守る）
- **Phase D（✅実装済 2026-07-16、ユーザー要望）**: 手動セーブスロット（GUI）—
  動機 = **気に入ったシーン（ロマンス等）を何度もロールプレイし直したい**。autosave（最新進捗
  1 本・受理ターン毎上書き）だけでは「あの場面に戻る」ができない → スロット = 任意時点の凍結点。
  - **器は追加ゼロ**: autosave と同じ `SessionSave` を別ファイル名へ書くだけ。
    `app_data_dir/saves/<パッケージパスの FNV-1a>_slot{1..5}.yaml`（autosave `<hash>.yaml` と
    同フォルダ・非衝突）。パッケージ別 5 スロット。
  - **UI**: ヘッダーの「続きから」と「新しいゲーム」の間に「セーブ」「ロード」ボタン。
    どちらも 5 スロットのリストダイアログ（`SaveSlotDialog`）を開き、クリックで保存/再開。
    メタ表示 = turn + 保存日時（file mtime、locale 表示）+ 直前の語りの冒頭（シーン識別の手がかり）。
    上書きセーブとプレイ中ロードは confirm。
  - **Tauri command 3 つ**: `list_save_slots`（package_path 省略時 = プレイ中 session の
    パッケージ = セーブ先の真実は session が握る。指定時 = ヘッダー選択中 = ロード用）/
    `save_slot`（生きた session をスナップショット — autosave と共通の `session_save_of`）/
    `load_slot`（`resume_game` と共通の `restore_session` — `GameSession` を**丸ごと差し替え**）。
  - **LLM のリセットは構造が保証**: ターンループは毎ターン messages を state/chronicle/synopsis
    から新規構築する（LLM に持続会話は無い）ので、session 差し替え = 次ターンから GM はロード
    された記憶だけを読み直す。プレイ中ロードでも前のプレイの記憶は残りようがない。
  - **ロード後の autosave 書き先は常に autosave パス**（ロード元スロットは以後のプレイで
    上書きされない = 凍結点のまま何度でも戻れる）。ロード時に autosave は書かない
    （「眺めるだけロード」で最新進捗を破壊しない — autosave が動くのは次の受理ターンから）。
  - **孤児掃除**: `delete_autosave` がスロットも一括処分（`list_packages` が `has_slots` を
    返し、パッケージ削除時の確認にスロットの有無も含める）。
  - PoC: `save_file_names_are_stable_and_slots_distinct_from_autosave` /
    `narration_snippet_truncates_at_char_boundary_and_flattens_newlines`。

## スコープ外

- セーブデータの暗号化・改竄防止（シングルプレイのローカル file、正本は engine が守る）。
- リプレイ再生（TurnLog 全量保存が将来の素材にはなる）。
- クラウド同期。

## 未決（査読事項）→ 確定 (2026-07-04 ユーザー回答)

1. **GUI のセーブ置き場**: → **app data dir** + パッケージ path の FNV ハッシュで 1 autosave。
2. **「続きから」の UX**: → **パッケージ選択時に自動提示**（ヘッダーのボタンで常時可視）。
3. **手動スロット**: → **Phase D に送る**（Phase C は autosave のみ）。
4. **セーブ版数の互換ポリシー**: 「v1 は実験的」宣言のまま（版不一致はロード拒否、
   serde(default) で前方互換の余地は残す）。
5. **エラー時の自動リトライ強化**（429 Retry-After 尊重 / backoff 引き上げ）: 本 spec 外に残置。
   autosave により実害（セッション消失）が消えたため優先度低。

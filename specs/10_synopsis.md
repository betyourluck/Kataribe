# 10. あらすじ — 長期の物語記憶とユーザー可視化

Status: **Phase A〜D Done（2026-07-14 rev2 査読 → 同日実装・PoC green。Phase E = 実 LLM 長編での
事実忠実度・コスト実測が残。GUI 実機の目視も残）**
Scope: chronicle の古い経緯を LLM で「章あらすじ」へ圧縮し、(a) GM の prompt に
「これまでのあらすじ」として恒久注入、(b) 右ペイン第 3 タブでユーザーにも見せる。
gm_core は**無改修**（harness + llm_client + app/CLI + frontend）。
spec 08 Phase C（章要約圧縮・見送り）の実体化 — 当時の再検討条件は「関連想起で拾えない
伏線の定量確認」だったが、**「ユーザーに見せるあらすじ」という独立した動機**が加わり起票。

## 問題（物語の連続性は retrieval では戻らない）

GM の記憶は現在三層 + 相補（短期 = recent_narration / 中期 = chronicle 注入予算 2400 字 /
相補 = Memoria + state）。spec 08 で古い経緯の**関連想起**は入ったが、想起はピンポイントの
事実（T3 で鍵を拾った）を返すだけで、**物語の連続した流れ（誰と何があってどう転がったか）は
注入予算からあふれた時点で GM から消える**。またプレイヤー側も、長いセッションや再開時に
「ここまで何があったか」を一覧する手段が会話ログの遡り読みしかない。

## 決定（2026-07-14 ユーザー確定）

1. **タイミング = モジュール遷移 + あふれ時**（毎ターン書き直しは棄却）
2. **タブ内容 = あらすじ + 最近の出来事**（未圧縮 chronicle の tail を併載）
3. **要約モデル = 別プロファイル指定可**（未指定なら GM と同じ client へフォールバック）
4. **圧縮は受理ターン直後に同期実行**（rev2 査読確定。頻度 10〜20 ターンに 1 回・体感数秒。
   非同期化の利得より、状態競合・終了時消失・recent_log との瞬間不整合を防ぐ利得が大きい。
   タイムアウト 15 秒、UI は「あらすじをまとめています…」インジケータ。Phase 2 で必要なら
   バックグラウンド化を再検討）

### 核: append-only（一度書いた segment は不変）

毎ターン書き直しを棄却する理由: (a) **書き直しの複利ドリフト** — 要約の要約を重ねるたびに
事実が落ちる（「忘れない GM」の逆行）、(b) ユーザーが読んだあらすじが毎ターン揺れる、
(c) 毎ターン 1 リクエストのコスト。segment は一度書いたら不変の**確定した語り素材**
（chronicle の TurnLog と同格）とし、追記だけで伸びる。

### 圧縮の契機（2 イベント）と範囲の厳密定義

未圧縮境界は `compressed_upto = synopsis.last().map(|s| s.upto_turn).unwrap_or(0)`
から導出（別カウンタは持たない）。**圧縮範囲は inclusive**:

- **① campaign モジュール遷移時**: `Range { start: compressed_upto + 1, end: 遷移確定時の
  最終受理ターン }`（= その章の未圧縮全量）。章題 = 遷移元モジュールの title。
- **② chronicle あふれ時**: 未圧縮エントリが **20 ターン超**になったら
  `Range { start: compressed_upto + 1, end: current_turn − 10 }`（= **直近 10 ターンは
  常に温存**）。章題 = 「ターン m〜n」。単一モジュール長編用。
- 次回の `start` は必ず `前回 upto_turn + 1` — **境界はオフバイワンなく接続**する
  （rev2: Range の inclusive 性を明記、重複・欠落の芽を凍結）。

→ 短編（〜20 ターン）は圧縮ゼロ回 = コストゼロ。長編は 10〜20 ターンに 1 回に自動調整。

### 失敗時の分岐（rev2: 契機別に分離）

- **あふれ契機の失敗**: skip して次の受理ターンで再試行（閾値超過が続くので自然に再発火。
  範囲は再計算してよい — 温存幅 10 が動くだけで章は跨がない）。
- **遷移契機の失敗**: **`pending_transition_range` として範囲を凍結して持ち越し**、
  以後の受理ターンで**同一範囲のまま**リトライする。範囲を再計算すると新モジュールの
  ターンが混入し「前章の要約に次章の内容が入る」事故になるため、**拡張禁止**。
  凍結範囲が消化されるまで、あふれ契機の判定は凍結範囲の後ろ（start = 凍結 end + 1）で行う。
- どちらも非致命（プレイは止めない）。恒常的に失敗する環境（ローカル弱モデル等）でも
  未圧縮 chronicle が消えるわけではない — history_note の予算打ち切りに戻るだけ
  （= 現行挙動へのグレースフルデグラデーション）。
- 遷移時に未圧縮が 3 ターン未満なら LLM を呼ばず**機械 join**で segment を作る
  （2 ターンの章に 1 リクエストは割に合わない）。機械 join は summary 行に加えて
  **機械タグ（location / items）を併記**する（rev2: summary 行だけだと GM が幻覚した
  summary がそのまま確定化する — engine 事実を必ず混ぜる）。機械 join も 400 字の
  本文上限でカットする（rev2）。

### 信頼チャネルの防衛（#47 と同族のリスク）

あらすじは非検証の言語チャネルで、混入した誤りは「確定した過去」として以後の全ターンを
汚染する（chronicle 自己汚染 #47 のあらすじ版）。守りは二層:

- **接地した入力**: 要約の入力は chronicle の summary 行 + **spec 08-B の機械タグ**
  （location / present / flags_set / items / checks = engine 事実）。指示に
  「記録に無い新事実を発明しない・固有名は記録の表記のまま・解釈や推測を書かない」。
- **前章 tail の扱い（rev2）**: `SynopsisRequest` に文体接続用として渡す直前 segment の
  末尾 1〜2 文は**非検証チャネル**なので、要約指示に「**tail は文体の接続のみに参照し、
  事実の出典として使わない**（事実は本リクエストの記録のみ）」を明記する。
- **ユーザー可視タブ自体が観測装置**: 作者・プレイヤーがドリフトに気づける
  （キャッシュ警告・開幕⚠ と同じ「静かな破れを surface する」系譜)。

## データ

```rust
// harness（gm_core には置かない — 可変世界状態ではなく確定した語り素材。chronicle と同格）
pub struct SynopsisEntry {
    pub upto_turn: u64,   // この segment が覆う最終ターン（範囲は前 entry の upto_turn+1 から、inclusive）
    pub title: String,    // 章題（モジュール title or「ターン m〜n」）
    pub text: String,     // 圧縮された物語（400 字以内、機械 join も同上限）
}
```

- 保持: `GameSession` / CLI ループが `Vec<SynopsisEntry>` を chronicle と並置で保持。
- **chronicle (`Vec<TurnLog>`) は従来どおり全量保持**（rev2 で明確化）: 現行実装も
  Vec は全量・`history_note` が注入時に予算 2400 字で切るだけ。あらすじ導入後も
  **Vec = 全量 / prompt = 予算スライス / retrieval = 全量対象**で不変。1 エントリ
  100〜200 バイト程度ゆえ数百ターンでも数十 KB — メモリ・セーブサイズの線形増加は許容
  （spec 07 の「chronicle 全量」セーブと同じ判断）。
- セーブ: `SessionSave.synopsis: Vec<SynopsisEntry>`（serde default = **旧セーブ互換**、
  spec 07/08 と同じ前方互換流儀）。`pending_transition_range` もセーブに含める
  （凍結リトライがセーブ跨ぎで生きる）。resume で復元し、タブも prompt も再開直後から埋まる。
  synopsis と history は同一セーブで同時 snapshot されるので resume では常に整合。
  将来 undo / 巻き戻し機能を入れる場合は **`upto_turn >= 巻き戻し先ターン` の segment を
  削除**する（rev2: 宙に浮く segment の遮断ルールを先に凍結）。
- 依存性逆転: `Summarizer` trait（`DeltaProposer` / `Memoria` と同型）。
  PoC は `ScriptedSummarizer`（fake）、実装は `LlmClient`。

```rust
pub trait Summarizer {
    fn summarize(&self, request: &SynopsisRequest) -> Result<String, SummarizeError>;
}
// SynopsisRequest = 章題 + 対象 TurnLog 列（summary + 機械タグ）+ 直前 segment の tail
// （文体接続のみ・事実参照禁止を指示に明記。全文は渡さない = 再要約ドリフトの入口を作らない）
```

## GM への注入

- `prompt::synopsis_note(&[SynopsisEntry])` — 「# これまでのあらすじ（確定した過去。
  矛盾する語りをしない）」として **history_note（経緯）の前**に注入。segment 0 件なら節なし。
- 役割分担: **あらすじ = 物語の連続性 / retrieval = 個別事実のピンポイント想起**
  （置き換えでなく相補。retrieval は全量 chronicle を対象に動き続ける）。
- 注入予算 2000 字・新しい章優先。**6 章目（目安 60 ターン以降）で最古の章から
  「（それ以前の章は省略）」に潰れ始める**（rev2: 数値を正直に。400 字 × 5 章 = 2000 字）。
  省略された章の個別事実は retrieval が全量 chronicle から拾えるため「忘れない」は
  破れない — 失うのは最古章の物語的連続性のみで、恒久解（古い章の階層再圧縮）は Phase 2。
- prompt caching: system 側の安定プレフィックスは不変。**user 側キャッシュは圧縮が走った
  ターンに再計算される**が、10〜20 ターンに 1 回なので許容（rev2: 誤解を防ぐ補足）。

## 要約モデルのプロファイル指定（ユーザー決定 ③、rev2 で読み先を分離）

- **app (Tauri)**: `app_data/.env` の `SUMMARY_LLM_*`（BASE_URL / API_KEY / MODEL /
  USE_TOOLS 相当）を**正**とする。設定「AI モデル」タブに「あらすじ要約用」プロファイル選択
  （既存プロファイル一覧から選ぶ or「GM と同じ」= 既定）。localStorage の選択 id は
  **UI 表示用**で、実体は env（決定時に書く = `set_llm_config` / #46 と同経路・同流儀）。
- **CLI**: 優先順位 = **1. CLI 引数（`--summary-model` / `--summary-base-url` 等）
  2. カレント / repo の `.env` の `SUMMARY_LLM_*` 3. GM 用 LLM 設定へフォールバック**。
  **app_data は探さない**（app_data は app 専用の前提を維持。共通化は OS 別パス解決が
  複雑になるだけで v1 は分離が最も堅牢）。
- backend は `SUMMARY_LLM_*` が揃っていればそれで要約用 `LlmClient` を組み、無ければ
  **GM の client を共用**（受領者ゼロ設定で動く）。
- 要約は tools 不要のプレーン生成 — `LlmClient` に `generate_text(system, user)`
  （tool 無し・schema 無し）を新設。no-tools サーバでもそのまま動く。

## UI（右ペイン第 3 タブ）

- 縦タブ「あらすじ」を進行 / 状態の下に追加。**Ctrl+3**（2026-07-08 に予約済みの拡張枠）、
  **Ctrl+Tab は 3 枚巡回**へ変更。アイコンは本（book）を Icon.vue に追加。
- 内容: ①segment 列（章題 + 本文、古い順）②「最近の出来事」= 未圧縮 chronicle の
  ターン別 1 行 `T{turn}: {summary}`（rev2 確定: chronicle summary そのまま =
  **GM が見ている記憶とユーザーが見るものが一致**、コストゼロ、ドリフト観測装置。
  将来物足りなければ `TurnLog.player` の先頭 20 字を薄色で括弧併記する拡張余地 —
  データ構造は不変のまま拡張できる）。序盤（圧縮前）でもタブが空にならない。
- **リスト key は `upto_turn`、表示は `title`**（rev2: モジュール title が「ターン 5〜10」
  形式の文字列でも衝突しない。title は表示専用・識別に使わない）。
- DTO（rev2 で差分方式に確定): `GameView.synopsis: Vec<SynopsisView>`（new_game/resume の
  全量）+ `TurnView.new_synopsis: Option<SynopsisView>`（圧縮が走ったターンの**追記差分
  のみ**。append-only ゆえ frontend は push するだけ — 全量置換のマージ分岐を持たない）。
  `recent_log` は `TurnView` に毎ターンの 1 行を差分で載せ、frontend が蓄積・圧縮時に
  `new_synopsis.upto_turn` 以前を tail から取り除く。

## 北極星との整合

- **可変状態の正本は `GameState` 専有**は不変。あらすじは chronicle・Memoria と同じ
  「確定した語り素材」であり、engine は存在すら知らない（gm_core 無改修）。
- 「忘れない GM」の第四層: 短期（直前語り）/ 中期（chronicle + retrieval）/
  **長期（あらすじ）**/ 相補（Memoria = authored 伏線）。
- 「矛盾しない GM」: append-only ゆえ過去の確定記述が書き換わらない =
  ユーザーに見せても揺れない。

## 実装（Phase 分割、各 Phase Red→Green）

- **Phase A（harness 核）**: `SynopsisEntry` / `Summarizer` trait / 圧縮判定の純粋関数
  `synopsis_due(history, compressed_upto, pending: Option<Range>) -> Option<(Range, 契機)>`
  （inclusive Range・遷移凍結範囲の優先消化）/ `build_synopsis_request`（機械タグ接地 +
  発明禁止 + tail 文体限定指示）/ `synopsis_note`（予算 + 節生成）/ 機械 join fallback
  （タグ併記 + 400 字カット）。
  PoC: あふれ契機・直近 10 温存・境界接続（オフバイワンなし）・遷移凍結範囲の同一リトライ
  （新章ターン非混入）・append-only・注入節・fallback タグ併記・発明禁止/tail 限定文言。
- **Phase B（llm_client）**: `generate_text`（プレーン生成、no-tools 両対応）+
  `Summarizer for LlmClient`（タイムアウト 15 秒）。PoC: リクエスト形状（tools 無し）・
  失敗時 Err。
- **Phase C（app/CLI 結線 + セーブ）**: `GameSession.synopsis` / play_turn 受理後の
  同期圧縮チェック / モジュール遷移契機 + `pending_transition_range` /
  `SessionSave.synopsis` + `pending_transition_range`（旧セーブ互換 PoC）/
  `SUMMARY_LLM_*` 設定 command（app_data/.env）。CLI ループにも同結線
  （引数 > .env > GM フォールバック）。
- **Phase D（frontend）**: 第 3 タブ + Ctrl+3 / Ctrl+Tab 3 枚巡回 + book アイコン +
  差分 DTO の蓄積表示 + 「あらすじをまとめています…」インジケータ。
  vue-tsc / vite build green。
- **Phase E（実測）**: 実 LLM 長編（30 ターン超）で ①あらすじの事実忠実度（発明ゼロか）
  ②GM が古い経緯を踏まえるか ③要約 1 回のコスト、を通しプレイで確認。
- data_contract.yaml に `SynopsisEntry` / `SUMMARY_LLM_*` / 圧縮契機・Range 定義を凍結
  （Phase A 前）。

## 決定パラメータ（rev2）

| 項目 | 値 |
|---|---|
| あふれ閾値 | 未圧縮 20 ターン超で圧縮 |
| 温存幅 | 直近 10 ターンは常に未圧縮 |
| 圧縮範囲 | inclusive `[compressed_upto+1, end]`、次回 start = 前回 upto_turn+1 |
| segment 本文 | 400 字以内（LLM 指示 + 機械 join とも上限カット） |
| 遷移時の最小 LLM 対象 | 3 ターン以上（未満は機械 join = summary + location/items タグ） |
| 注入予算 | 2000 字・新しい章優先・6 章目以降は最古から省略（retrieval が事実を補完） |
| 要約タイムアウト | 15 秒 |
| 失敗時 | あふれ契機 = skip して次ターン再計算 / 遷移契機 = 範囲凍結で同一リトライ |
| 同期実行 | play_turn 受理後に同期（10〜20 ターンに 1 回・数秒） |

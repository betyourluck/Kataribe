# CLAUDE — Kataribe (語り部)

クラウド LLM をナレーター、Rust の決定論エンジンを正本とした TRPG-GM。

## 北極星

**「忘れない・矛盾しない GM」。** 売りは無限の自由ではなく一貫性。buzz/karma は反指標。
LLM-GM の死因は文章力でなく忘却と矛盾 → LLM に状態の真実を持たせないことで構造的に断つ。

## アーキテクチャ — 三権分立

> LLM は提案し、エンジンが裁き、Memoria が覚え、シナリオが縛る。

- **エンジン（正本 / Rust `crates/gm_core`）**: HP/所持品/ダイス/フラグ/位置の唯一の真実。
  - `adjudicate(state, scenario, delta) -> Verdict`: **state を一切変えない純粋関数**。却下理由は構造化 `RejectReason`（文字列でなくデータ）で返し、文面は提示層が `localize(lang)` で生成（Ja/En、i18n の土台）。検証ルールはエンジン不変、文字列だけ分離。
  - `apply(...)`: 受理時のみ**原子的**に適用。1つでも不正 op があれば全体却下・state 無傷。
  - ダイスは seeded RNG（`RngState`）で決定論・監査可能。LLM は出目を持てない（op 構造上不可能）。
- **LLM（提案 / Rust `crates/llm_client`）**: `StateDelta { narration, ops }` を返す。`narration` は検証しない（LLMの領分）、`ops` は全件検証。
  - OpenAI 互換 chat/completions を `base_url`+`api_key` で抽象化。**tool-use 強制**（単一ツール `emit_delta`）で構造化出力。
  - `emit_delta` の schema は schemars が `gm_core::StateDelta` から**機械生成**（規格=実装の単一真実源・手書き禁止）。
  - `tool_choice` 非尊重サーバ向けに content のフェンス JSON フォールバックあり。パース失敗は `raw` を保持し再生成の燃料にする。
  - **裁定はしない**。messages 構築と `adjudicate` は上位（harness）の責務。
- **ターンループ（結線 / Rust `crates/harness`）**: 三権を 1 ターンに繋ぐ。`run_turn`: 提案 → `adjudicate` → `Accept` なら `apply`／`Reject` なら理由を messages に積んで再生成（最大N回、LocalAI `_self_repair_loop` 同型）。ループは `DeltaProposer` trait に対して書き、実 LLM とテスト用 fake を差し替えられる。
- **シナリオ（拘束 / `scenarios/*.yaml`）**: `Gate` 条件つきの場所グラフ。即興が筋から外れすぎないよう縛る。
- **Memoria（記憶）**: 伏線・キャラ性格の semantic recall。**可変世界状態は絶対に置かない**（曖昧な recall は「忘れる GM」を再現する）。

## 掟（Mandate）

- **データ・ファースト**: コードの前に `data_contract.yaml`（名詞）を凍結する。
- **PoC 必須**: バグ修正・新機能は Red→Green をテストで実証してから完了。推測修正は不可。
- **リサーチ先行**: 実装前に三点測量（既存コード grep / 仕様 / 記憶）。
- **失敗時に謝罪しない**: 観察 → 仮説棄却 → 次の検証ステップの三段で進む。失敗は接地を深める信号。
- **層分け**: 仕様・契約はこの file 台帳（`data_contract.yaml` / README / docs）に書く。Memoria には蒸留した教訓・判断だけを残す。

## 主要コマンド

```bash
cargo test --workspace            # PoC テスト（gm_core + llm_client、計 18）
cargo clippy --workspace --all-targets  # lint
```

## 現状

- ✅ `gm_core` 正本エンジン: 17/17 green。密室脱出（真偽の最小盤面）で不正遮断・原子性・敵対ターン・決定論ダイス、力の試練（数値の最小盤面）で**数値ステータス**を実証。
  - **数値ステータス** (`stats: BTreeMap<StatKey, i64>`): 四則演算をエンジンが代行。`AdjustStat`(＋/−) と `ScaleStat`(×/÷) で LLM は意図だけ提案・値は持てない。HP の 0 クランプ、**ゼロ除算は却下**、未宣言 stat（幻ステータス）の遮断、`StatAtLeast` 数値 gate を実証。式（ダイス＋能力修正の技能判定）は次の盤面。
  - **実 LLM 検証 (2026-06-23, claude-opus-4-8)**: 力の試練を通しプレイし、LLM が `adjust_stat`（鍛錬で str+2/hp-2 のトレードオフ）と `scale_stat`（「腕力を倍に」→ str 12×2=24）を**プロンプト変更ゼロ**で提案（schemars 自動露出）、`StatAtLeast` gate（str≥15）を越えて goal 到達。「LLM が意図を言い、エンジンが値を計算する」を端から端まで実証。LLM は現在値を読んで数値推論し（"14 では一歩足りない"）、不可能な手は自ら拒否。
  - **キャラ別ステータス** (`entities: BTreeMap<EntityId, BTreeMap<StatKey,i64>>`): 数値を entity（`"player"`/NPC/ヒロイン）別に保持。op/gate は `entity` を取る（省略時 `player`）。外部キャラ定義 `CharacterDef`（name/profile/stats[`StatDecl` initial・min・max]/taboos）を scenario が持つ。**境界クランプ**（好感度 max 100 等）と**未宣言 stat/entity の遮断**を実証（邂逅シナリオ）。`profile`（設定・背景・性向）は語りに供給。**禁忌は2種**: 硬い禁忌（豚肉を断つ＝`taboos` の Gate、✅Phase B 実装済）/ 柔らかい性向（同性を好む＝profile、語り）。Phase B: `check_taboos` が delta を state の clone に射影し taboo(Gate) が false→true 真化するなら却下（キャラは自分の禁忌を破れない＝「正本 > 文章力」のキャラ版）。禁忌(却下)とトリガー(発火, Phase C 設計凍結)は同一機構の双対。
  - **却下理由の多言語化**: `Verdict::Reject` は構造化 `RejectReason`（12 種）。`localize(Lang::{Ja,En})` で文面生成、bin は `KATARIBE_LANG=en`。検証ルールはエンジン不変、文面だけ言語層。
  - **外部キャラファイル**: `characters/*.yaml`（1キャラ1ファイル、ファイル名＝EntityId）を `harness::load_characters` が読み、bin が scenario に注入（inline 優先）。**実 LLM 検証 (2026-06-23)**: 邂逅シナリオ（inline キャラ無し）で alice を外部ファイルから注入 → LLM が `adjust_stat entity=alice` で好感度 0→55、profile（人見知り・甘いもの・豚肉の誓い）を語りに自発反映、好感度 gate で goal 到達。
- ✅ `crates/llm_client` ナレーター脚: LocalAI `llm_client.py` を Rust 移植。OpenAI 互換 + tool-use 強制 + schemars 機械生成 schema + フェンス JSON フォールバック + 指数 backoff。PoC 9/9 green。罠 6 件は `failures.md`。
- ✅ `crates/harness` GM ターンループ: 提案 → 裁定 → 却下なら理由を戻して再生成（`_self_repair_loop` 同型）→ 受理なら原子適用。`DeltaProposer` trait で依存性逆転し、`ScriptedProposer` で「却下→再生成」を実 API なしで実証。PoC 6/6 green（一発合格・再生成・理由還流・最大試行で state 無傷・ダイス経路・prompt 健全性）。罠 4 件（#7-10）は `failures.md`。
- 合計 **24/24 green**、clippy clean。
- ✅ `harness` bin `play`: 実クラウド通しプレイ CLI。`LlmClient` を `DeltaProposer` に配線、`.env` 実キーで密室脱出を回す（`cargo run -p harness --bin play`、stdin 対話 or 台本流し込み）。
- ✅ **核心的未知の測定 (2026-06-23, claude-opus-4-8 @ Anthropic 互換)**: 密室脱出を実 LLM で通しプレイし **goal 到達（turn 4, 4/4 一発合格）**。LLM がエンジンの制約内で構造化出力を出し続けられることを実証。schemars 生成スキーマも Anthropic 互換層が受理（`failures.md #3` 解決）。実 API で判明した罠は `failures.md #12-13`（temperature 非対応 / dotenv 副作用）。
- ✅ **敵対プレイ実測 (2026-06-23)**: 「正本 > 文章力」を実 API で実証。LLM は scenario_brief の gate を読み不可能な単独行動（解錠前 move・幻 master_key）を自ら拒否（prompt 層接地）。複数ステップを束ねた行動では原子性違反デルタを提案 → エンジン却下 → 理由還流 → LLM が合法な部分手に修正（attempts=2）。`failures.md #15`（再生成のプレーン echo 形式が実 API で通る）を実証、`#14`/`#16` は watch。
- ⬜ 次: Memoria 脚の接続（伏線・キャラ性格の semantic recall のみ、可変世界状態は禁忌）→ Tauri+Vue UI 殻の移植。UI 接続時は narration の literal `\n` 正規化（`#16`）。

## ルーツ

LocalAI（`D:/Github/LocalAI`、廃棄）から摘出: `llm_client` / SSE ストリーミング骨格 / HybridMemory / Tauri UI 殻。
中核（正本・シナリオ脊椎）は新規。詳細な判断経緯は LocalAI のコードと本台帳を参照。

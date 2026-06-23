# CLAUDE — Kataribe (語り部)

クラウド LLM をナレーター、Rust の決定論エンジンを正本とした TRPG-GM。

## 北極星

**「忘れない・矛盾しない GM」。** 売りは無限の自由ではなく一貫性。buzz/karma は反指標。
LLM-GM の死因は文章力でなく忘却と矛盾 → LLM に状態の真実を持たせないことで構造的に断つ。

## アーキテクチャ — 三権分立

> LLM は提案し、エンジンが裁き、Memoria が覚え、シナリオが縛る。

- **エンジン（正本 / Rust `crates/gm_core`）**: HP/所持品/ダイス/フラグ/位置の唯一の真実。
  - `adjudicate(state, scenario, delta) -> Verdict`: **state を一切変えない純粋関数**。
  - `apply(...)`: 受理時のみ**原子的**に適用。1つでも不正 op があれば全体却下・state 無傷。
  - ダイスは seeded RNG（`RngState`）で決定論・監査可能。LLM は出目を持てない（op 構造上不可能）。
- **LLM（提案）**: `StateDelta { narration, ops }` を返す。`narration` は検証しない（LLMの領分）、`ops` は全件検証。
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
cargo test                  # PoC テスト（gm_core）
cargo clippy --all-targets  # lint
```

## 現状

- ✅ `gm_core` 正本エンジン: 密室脱出シナリオで 9/9 green。不正遮断・原子性・敵対ターン・決定論ダイスを実証。
- ⬜ 次: `crates/llm_client`（LocalAI `llm_client.py` を Rust 移植 / クラウド structured output）→ GM ターンループ（`adjudicate` 却下→再生成、LocalAI `_self_repair_loop` と同型）。
- ⬜ その後: Memoria 脚の接続、Tauri+Vue UI 殻の移植。

## ルーツ

LocalAI（`D:/Github/LocalAI`、廃棄）から摘出: `llm_client` / SSE ストリーミング骨格 / HybridMemory / Tauri UI 殻。
中核（正本・シナリオ脊椎）は新規。詳細な判断経緯は LocalAI のコードと本台帳を参照。

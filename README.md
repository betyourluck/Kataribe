# 🎲 Kataribe (語り部) — 忘れない・矛盾しない GM

クラウド LLM をナレーターに、**Rust の決定論エンジンを正本（ゲーム状態の唯一の真実）**に据えた TRPG ゲームマスター。

AI Dungeon 系の LLM-GM が必ず崩れる死因は、文章力ではなく**忘却と矛盾**（持ち物・誰が死んだ・どこにいる・前回何を決めたか）。Kataribe はその故障モードを、LLM に状態を持たせないアーキテクチャで構造的に断つ。売りは「無限の自由」ではなく **一貫性**。

> 仮称。リポジトリ名は変更可。

## 設計の核 — 三権分立

> **LLM は提案し、エンジンが裁き、Memoria が覚え、シナリオが縛る。**

| 脚 | 役割 | 実装 | 状態 |
|---|---|---|---|
| **エンジン（正本）** | HP/所持品/ダイス/フラグ/位置を決定論的に裁く | `crates/gm_core` (Rust) | ✅ PoC green |
| **LLM（提案）** | 情景描写・NPC台詞・行動提案。数値の真実を持たない | `crates/llm_client` (予定) | ⬜ 次フェーズ |
| **Memoria（記憶）** | エピソード・伏線・キャラ性格の semantic recall | (予定) | ⬜ ループ green 後 |
| **シナリオ（拘束）** | beat graph + gate 条件で筋から外れすぎを防ぐ | `scenarios/*.yaml` | ✅ 最小版 |

**鉄則:** 可変世界状態は state machine。埋め込み想起（ベクトル recall）には**絶対に置かない** — 曖昧な recall は「忘れる GM」を再現してしまう。伏線・性格だけが Memoria の領分。

## GM ターンループ

```
プレイヤー入力
  → 関連 canon/記憶を文脈に注入
  → LLM が StateDelta（structured output: narration + ops）を提案
  → エンジンが adjudicate（不正なら理由つき却下 → 再生成、最大 N 回）
  → 受理なら原子的に state 更新 + beat 前進
  → （後で）Memoria に記録
```

`adjudicate` は state を一切変えない純粋関数。`apply` は受理時のみ原子的に適用する。

## 今ここまで動く（PoC）

正本エンジン単体を、密室脱出シナリオで実証済み（`cargo test` で 9/9 green）:

- ✅ 正規の筋（引き出し→鍵→解錠→脱出）で goal 到達
- ✅ 不正状態の遮断（鍵なし解錠・解錠前移動・引き出し前の鍵取得）
- ✅ **敵対ターン**: 持っていない「マスターキー」で開けようとしても却下 = 正本が LLM の流暢さに勝つ
- ✅ 原子性: 一部不正なデルタは全体却下、state は無傷
- ✅ ダイスは決定論的・監査可能（seeded RNG）

## ビルド & テスト

```bash
cargo test          # PoC テスト
cargo clippy --all-targets
```

## 構成

```text
Kataribe/
├── data_contract.yaml     # ★名詞の凍結（GameState / StateDelta / Gate の契約）
├── scenarios/
│   └── locked_room.yaml   # PoC シナリオ（密室脱出）
└── crates/
    └── gm_core/           # Rust: 正本（state / spine / engine）
        └── src/
            ├── state.rs   # GameState / StateOp / StateDelta / 決定論RNG
            ├── spine.rs   # Scenario / Location / Exit / Gate
            └── engine.rs  # adjudicate / apply / is_goal + PoCテスト
```

## ルーツ

LocalAI（廃棄プロジェクト）から摘出予定の資産: `llm_client`（OpenAI互換抽象）/ SSE ストリーミング骨格 / HybridMemory / Tauri+Vue UI 殻。中核（正本エンジン・シナリオ脊椎）は新規。

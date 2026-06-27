# 02. フラグの三値持続性 — 捨てる / 世界に持つ / その場所に持つ

Status: **Done（campaign-蓄積モデル実装済・PoC green）** / 2026-06-27
Scope: campaign のモジュール跨ぎで、フラグの持続性を二値（global/局所）から三値へ広げ、
「再訪で宝箱が復活する」問題を断つ。

## 北極星整合

- **gm_core は純粋のまま**: 追加するのは `Scenario.persistent_flags`（宣言＝データ）と
  その閉世界検査だけ。**蓄積ロジックは持たない**（campaign は engine の概念ではない）。
- **蓄積は harness（orchestration 層）**: モジュール別のフラグ記憶を campaign 層に持ち、
  `transition` の二値とは独立に糸通しする。
- **帰結は engine/作者が握る**: どのフラグが「その場所に持つ」かは作者が宣言、値は engine が握る。
  生成器/LLM は持続性も値も持てない（既存の閉世界の延長）。

---

## 問題（実プレイ前に構造で判明）

現状のフラグ持続性は二値:
- **局所（捨てる）**: `transition` が捨てる。次モジュールへ持ち越さない（既定）。
- **世界（持つ）**: `Scenario.global_flags` に宣言。`transition` が次モジュールへ運ぶ。

`transition(prev, prev_scenario)` は**遷移元**モジュールの `global_flags` だけを運ぶ。
ゆえにフラグが campaign 全体を通して生き残るには、**通過する各モジュールが個別に global 宣言**
しないといけない。双方向グラフ（A→B→A）で:
- `chest_opened` が局所 → A→B→A の再訪で `initial_state` がフラグを空に戻す → **宝箱が復活**（矛盾する GM）。
- `chest_opened` を全モジュールで global 宣言 → 動くが、宝箱の状態が**無関係なモジュールにも
  ambient な世界真実として漏れる**（namespace 汚染）。さらに B が global 宣言を忘れると B→A で消える（脆い）。

**三つ目の値が要る**: その場所（モジュール）でだけ意味を持ち、そのモジュールを再訪したとき**だけ**
思い出されるフラグ。これが「その場所に持つ」。

---

## モデル（合意: campaign 状態に蓄積）

フラグの持続性 = 三値:

| 値 | 宣言 | transition の挙動 | 例 |
|---|---|---|---|
| **捨てる（局所）** | どこにも宣言しない（既定） | 次モジュールへ運ばない | `door_open` `lamp_lit`（その場の一時状態） |
| **世界に持つ（ambient）** | `Scenario.global_flags` | 次モジュールへ運ぶ（既存・全モジュールで見える） | `met_alice`（一度会えばどこでも既知） |
| **その場所に持つ（place）** | `Scenario.persistent_flags`（新規） | campaign 層がモジュール別に記憶し、**そのモジュール再訪時だけ**復元 | `chest_opened`（その部屋に戻った時だけ覚えている） |

- `persistent_flags ⊆ allowed_flags`（`global_flags` と同型の閉世界検査）。
- global と persistent の両方に挙げるのは冗長（global が支配）。エラーにはしない（最小）。

### campaign 蓄積層（harness）

gm_core 無改修で、`advance_*`（遷移点）に**モジュール別フラグ記憶**を糸通しする:

```
CampaignMemory = BTreeMap<ModuleId, BTreeMap<FlagKey, bool>>
```

`advance_with` で遷移を起こすとき:
1. **harvest**: 遷移**元**モジュールの `persistent_flags` を `state.flags` から読み、
   `memory[current_module]` に上書き保存（その場所の最新の記憶を更新）。
2. **transition**: 既存どおり（局所フラグ＝persistent も含め捨て、global だけ運ぶ）。
3. **overlay**: 遷移**先**モジュールの記憶 `memory[next_module]` を `next_state.flags` に重ねる
   （再訪なら過去の persistent フラグが蘇る。初訪なら記憶が無く何も起きない）。

これで「その場所に持つ」が成立する。namespace は `ModuleId` キーで分離 — A の `chest_opened` は
B に漏れず、B に同名フラグがあっても衝突しない。

- 数値/所持品/能力/属性は**常に持ち越し**（transition 既存）＝それらは本質的に world。
  三値の問いは flag だけ（bool ゆえ「その場所の記憶」が意味を持つ）。
- `CampaignMemory` はセッション保持（`GameSession` / CLI ループ）。save/load は saves 実装時に拡張。

---

## データモデル（gm_core 追加・純粋データ）

```
Scenario.persistent_flags: BTreeSet<FlagKey>   # その場所に持つ（モジュール再訪で復元）。allowed_flags の部分集合。
```

`validate()` に `ScenarioError::PersistentFlagUndeclared { flag }`（`persistent_flags ⊄ allowed_flags`）。

## harness 追加

```
pub type CampaignMemory = BTreeMap<ModuleId, BTreeMap<FlagKey, bool>>;
fn harvest_persistent(mem, module, scenario, state)   # 遷移元の persistent フラグを記憶
fn overlay_persistent(mem, module, state)             # 遷移先の記憶をフラグへ復元
advance_campaign(..., memory: &mut CampaignMemory)          # 署名拡張
advance_campaign_injected(..., memory: &mut CampaignMemory) # 署名拡張
```

## app / CLI

`GameSession.campaign_memory: CampaignMemory` を保持し `advance_campaign_injected` へ `&mut` で渡す。
CLI `play --campaign` も同型。

---

## PoC（Red→Green、harness fixture）

`crates/harness/fixtures/` に**再訪サイクル**の campaign を新設:
- `campaign_revisit.yaml`: village ⇄ forest（双方向辺）。
- `village.yaml`: `persistent_flags: [chest_opened]` + 局所 `torch_lit`。
- `forest.yaml`: 局所 `forest_marker`。

テスト `persistent_flag_survives_revisit_local_flag_does_not`:
1. village で `chest_opened=true`（place）+ `torch_lit=true`（局所）→ goal で forest へ advance。
2. forest で goal → village へ **再訪** advance。
3. 復帰した village state で `chest_opened==true`（その場所の記憶が蘇る）、
   `torch_lit` 不在（局所は捨てられたまま）、forest の `forest_marker` も漏れない。

---

## スコープ外（次段）

- per-flag のさらに細かい location（モジュール内の単一 location）スコープ。現状は**モジュール**粒度。
- `CampaignMemory` の save/load 永続化（saves 機構が来たら）。

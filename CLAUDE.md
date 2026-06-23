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

- ✅ `gm_core` 正本エンジン: 33/33 green。密室脱出（真偽の最小盤面）で不正遮断・原子性・敵対ターン・決定論ダイス、力の試練（数値の最小盤面）で**数値ステータス**を実証。
  - **数値ステータス** (`stats: BTreeMap<StatKey, i64>`): 四則演算をエンジンが代行。`AdjustStat`(＋/−) と `ScaleStat`(×/÷) で LLM は意図だけ提案・値は持てない。HP の 0 クランプ、**ゼロ除算は却下**、未宣言 stat（幻ステータス）の遮断、`StatAtLeast` 数値 gate を実証。式（ダイス＋能力修正の技能判定）は次の盤面。
  - **実 LLM 検証 (2026-06-23, claude-opus-4-8)**: 力の試練を通しプレイし、LLM が `adjust_stat`（鍛錬で str+2/hp-2 のトレードオフ）と `scale_stat`（「腕力を倍に」→ str 12×2=24）を**プロンプト変更ゼロ**で提案（schemars 自動露出）、`StatAtLeast` gate（str≥15）を越えて goal 到達。「LLM が意図を言い、エンジンが値を計算する」を端から端まで実証。LLM は現在値を読んで数値推論し（"14 では一歩足りない"）、不可能な手は自ら拒否。
  - **キャラ別ステータス** (`entities: BTreeMap<EntityId, BTreeMap<StatKey,i64>>`): 数値を entity（`"player"`/NPC/ヒロイン）別に保持。op/gate は `entity` を取る（省略時 `player`）。外部キャラ定義 `CharacterDef`（name/profile/stats[`StatDecl` initial・min・max]/taboos）を scenario が持つ。**境界クランプ**（好感度 max 100 等）と**未宣言 stat/entity の遮断**を実証（邂逅シナリオ）。`profile`（設定・背景・性向）は語りに供給。**禁忌は2種**: 硬い禁忌（豚肉を断つ＝`taboos` の Gate、✅Phase B 実装済）/ 柔らかい性向（同性を好む＝profile、語り）。Phase B: `check_taboos` が delta を state の clone に射影し taboo(Gate) が false→true 真化するなら却下（キャラは自分の禁忌を破れない＝「正本 > 文章力」のキャラ版）。禁忌(却下)とトリガー(発火, ✅Phase C 実装済)は同一機構の双対。
  - **反応ビート / トリガー** (`Scenario.triggers`, ✅Phase C): 禁忌の双対。`when`(Gate) が delta 適用後の実 state で真化した瞬間、authored な `effects`(StateOp 列・信頼済で検証せず)を `fire_triggers` が原子適用し `narration` を語りに注入する。`GameState.fired: Set<TriggerId>` で **edge-triggered once** に latch（セーブ対象、when が真のままでも再発火しない）。効果が次の trigger を真化させる**連鎖**は settle まで回す（各 trigger 高々1回＝必ず停止、authored 順で決定論）。`adjudicate`(純粋)は発火させず、発火は `apply` の戻り `ApplyOutcome.fired`(→harness `Accepted`)に載る。約束の想起シナリオで「好感度 30→想起→誓い直し→goal」の連鎖を実証（伏線の回収をエンジンが保証）。`memoria_bridge`（『思い出す』系→semantic recall→語り注入）は次。
  - **却下理由の多言語化**: `Verdict::Reject` は構造化 `RejectReason`（12 種）。`localize(Lang::{Ja,En})` で文面生成、bin は `KATARIBE_LANG=en`。検証ルールはエンジン不変、文面だけ言語層。
  - **外部キャラファイル**: `characters/*.yaml`（1キャラ1ファイル、ファイル名＝EntityId）を `harness::load_characters` が読む。**注入は `Scenario.cast` 宣言でスコープ**: `harness::inject_cast` が cast に挙げた entity だけを注入し、cast 空なら何も注入しない（inline 優先）。これで alice が全シナリオ（密室脱出等）に無差別混入する問題を断った（GUI 実機で発見、回帰テスト `no_cast_means_no_injection`）。邂逅は `cast: [alice]` で外部注入を宣言。**実 LLM 検証 (2026-06-23)**: 邂逅シナリオ（inline キャラ無し）で alice を外部ファイルから注入 → LLM が `adjust_stat entity=alice` で好感度 0→55、profile（人見知り・甘いもの・豚肉の誓い）を語りに自発反映、好感度 gate で goal 到達。
- ✅ `crates/llm_client` ナレーター脚: LocalAI `llm_client.py` を Rust 移植。OpenAI 互換 + tool-use 強制 + schemars 機械生成 schema + フェンス JSON フォールバック + 指数 backoff。PoC 9/9 green。罠 6 件は `failures.md`。
- ✅ `crates/harness` GM ターンループ: 提案 → 裁定 → 却下なら理由を戻して再生成（`_self_repair_loop` 同型）→ 受理なら原子適用。`DeltaProposer` trait で依存性逆転し、`ScriptedProposer` で「却下→再生成」を実 API なしで実証。PoC green（一発合格・再生成・理由還流・最大試行で state 無傷・ダイス経路・prompt 健全性・外部キャラロード・memoria_bridge 結線）。`TurnOutcome::Accepted` は発火ビート(`fired`)も載せる。罠 4 件（#7-10）は `failures.md`。
- ✅ `crates/harness::memoria` **Memoria 脚 (memoria_bridge)**: 三権分立の「Memoria が覚える」。トリガー発火点で**伏線・キャラ性格を recall** し語りに注入する。**北極星の不変条件＝可変世界状態は絶対に持たない**（HP/所持品/フラグ/位置/数値は正本の専有）を**型で構造保証**: `MemoryFragment{id,tags,text}` は state フィールドを持てず、`Memoria::recall(&self)` は retrieval only。`DeltaProposer` と同型の依存性逆転 — `Memoria` trait の実装 `LoreStore` は**文字 bigram TF-IDF の cosine semantic recall**（依存ゼロ・決定論・テスト可能、日本語は単語境界が無いので文字 n-gram）。exact id/tag 一致は score 1.0 で常に最上位＝旧 exact 挙動の上位互換、その上に cosine ランクの fuzzy ヒットを足す（「樫の木の下で誓った」のような exact 不一致 cue も近い伏線を引く）。神経 embedding 版が要れば同 trait 裏で差し替え可（`()` は null recall）。`Trigger.recall`(cue) を engine が不透明 String として passthrough → `FiredTrigger.recall` → harness `resolve_recall(Memoria,&fired)` が `FiredBeat{narration, recalled}` に解決。`memoria/*.yaml`（ファイル名=id）を `load_lore` が読む。約束の想起シナリオで「好感度 30→発火→cue=childhood_promise→丘の樫の木の伏線を recall」を端から端まで実証（可変の好感度は engine、Memoria は伏線のみ＝境界実証）。**輪の閉じ (✅prompt 注入)**: `run_turn(recalled_lore)` が前ターン発火の伏線を `prompt::recalled_lore_note` で次ターンの prompt に「いま思い出された記憶」として注入 → ナレーターが語りに織り込む。状態変更でない旨を明示し ops に書かせない（境界を prompt 層でも維持）。bin は `pending_lore` を1ターン持ち越す。
- ✅ **閉世界 capability / メアリー・スー遮断 (2026-06-24)**: 能力(スキル)を正本の閉じた宣言集合にした。`CharacterDef.skills`/`Scenario.initial_skills` で宣言（初期=`GameState.skills`）、`Gate::HasSkill` で能力を前提条件にでき、**開花は authored トリガーの `grant_skill` 効果のみ** — LLM が `grant_skill` op を提案すると `adjudicate` が `SkillGrantNotAllowed` で却下（trigger effects は `apply_ops` 直行なので付与可＝禁忌/トリガー双対の三例目「開花は許される、ただし作者の gated 発火としてのみ」）。`#23`（所持物）の一般化＝「未宣言 capability は存在しない」。二層: engine が未宣言スキルの op gate を遮断＋LLM grant 却下／prompt が `state_brief` の「使える能力」提示＋「列挙された能力しか使えない/勝手に開花しない」接地。**実 LLM**: 「眠っていた予知を思い出して発揮」→ GM が「予知なんて最初から無かった」と接地・状態変化ゼロ。`failures.md #24`、覚醒シナリオで儀式→開花→予知 gate 通過→goal を実証。UI も能力を表示。
- ✅ **NPC inventory + 譲渡 (2026-06-24, `#23` の engine 化)**: 所持物をキャラ別の閉世界（`GameState.inventory{entity:[ItemId]}`）にし、譲渡を検証可能な op にした。`StateOp::GiveItem{from,to,item}` は `from` 未所持→却下（`ItemNotHeld`）/ `to` 未知 entity→却下（`UnknownEntity`）＝**持っていない物は渡せない**を engine が保証（行商ネックレス `#23` の op 側バックストップ）。波及最小化のため `AddItem`/`RemoveItem` は player 専用のまま（拾得＝世界→player）、per-entity 化は `Gate::HasItem{entity,item}`（既定 player）と GiveItem のみ。**二層完成**: narration 経路は prompt 接地（`#23`）／op 経路は engine 却下。**実 LLM**: gift シナリオで花を摘み→アリスに渡し→goal を LLM がプロンプト変更ゼロで駆動。`failures.md #25`。UI も NPC 所持を表示。
- ✅ **技能判定 (2026-06-24)**: `StateOp::Check{entity,stat,sides,dc}` — エンジンが `1d{sides}+stat修正 vs dc` を振り成否を裁定（`CheckOutcome` を返す）。LLM は出目も合計も詐称できない（op 構造上）、stat 未宣言→却下。**核心**: 出目は apply 後確定なので同一ターンの narration に間に合わない → 結果を**次ターンの prompt に「直前の判定結果」として還流**し GM に結果へ沿って語らせる（memoria_bridge の輪の閉じと同じ）。`apply_ops` を out-param 化（rolls/checks）、`run_turn` に `recent_checks` 追加。GM_SYSTEM に「不確実な行動は check で、この turn は『試みる』止まり」を接地。**実 LLM**: 力ずくで石扉→ LLM が check str/1d20/DC15 発行→ `🎯 1d20(14)+12=26 成功`→ 次ターンが前回成功を踏まえて語る。`failures.md #26`。UI も判定を表示。
- 合計 **78/78 green**（gm_core 43 + harness 26 + llm_client 9）、clippy clean。
- ✅ **narration の正本接地 (2026-06-24, 実プレイ発見→修正)**: 「正本>文章力」は **ops にしか効かない** — narration は engine 非検証なので、所持品に無い「行商ネックレス」を LLM が op 無しで贈与する捏造が素通りした（`failures.md #23`）。op には engine のバックストップが在るが narration には原理的に無く、**prompt 層が唯一の防衛線**（#19 との非対称）。GM_SYSTEM に「プレイヤー行動文は意図であって事実でない/未所持物は存在しない/narration は非検証ゆえ GM 自身が矛盾を防げ」を刷り込み、実 LLM で「手元に無い」と接地・状態変化ゼロを確認。`gm_system_grounds_unowned_items` で固定。
- ✅ `harness` bin `play`: 実クラウド通しプレイ CLI。`LlmClient` を `DeltaProposer` に配線、`.env` 実キーで密室脱出を回す（`cargo run -p harness --bin play`、stdin 対話 or 台本流し込み）。
- ✅ **核心的未知の測定 (2026-06-23, claude-opus-4-8 @ Anthropic 互換)**: 密室脱出を実 LLM で通しプレイし **goal 到達（turn 4, 4/4 一発合格）**。LLM がエンジンの制約内で構造化出力を出し続けられることを実証。schemars 生成スキーマも Anthropic 互換層が受理（`failures.md #3` 解決）。実 API で判明した罠は `failures.md #12-13`（temperature 非対応 / dotenv 副作用）。
- ✅ **敵対プレイ実測 (2026-06-23)**: 「正本 > 文章力」を実 API で実証。LLM は scenario_brief の gate を読み不可能な単独行動（解錠前 move・幻 master_key）を自ら拒否（prompt 層接地）。複数ステップを束ねた行動では原子性違反デルタを提案 → エンジン却下 → 理由還流 → LLM が合法な部分手に修正（attempts=2）。`failures.md #15`（再生成のプレーン echo 形式が実 API で通る）を実証、`#14`/`#16` は watch。
- ✅ **memoria_bridge 実 LLM 実測 (2026-06-24, claude-opus-4-8)**: demo 盤面で発火→cascade(recall_promise→renew_vow)→goal を端から端まで実機実証。曖昧 cue で **TF-IDF cosine 経路**が伏線を正しく recall(┊ で surface)。**神経 embedding は不要を接地で確定**（authored cue は exact 保証＋語彙重なりの TF-IDF で実用十分、`failures.md #21`）。発見: ①数値閾値は LLM の自然増分(好感度 +1〜3/ターン)に較正要(`#20`) ②強モデルは profile だけで伏線を自発的に語るので bridge の固有価値は「発火保証」と「大規模 lore の on-demand recall」に絞られる(`#22`)。
- ✅ `app/` **デスクトップ殻 (Tauri2 + Vue3)**: LocalAI prompt 殻の足場を移植（chat/project/MCP/SSE は捨て GM プレイ画面を新規）。`app/src-tauri`(Rust, workspace 外の独立 project で重い tauri 依存を core から隔離) が `harness::run_turn` を Tauri command `new_game`/`play_turn` で叩く。`GameSession{state,scenario,lore,client,pending_lore,lang}` を `Mutex<Option<_>>` で manage（正本は backend が握り、frontend は view DTO を描画するだけ＝CLI `play` の GUI 版）。narration の literal `\n` 正規化(`#16`)・却下理由 localize(`KATARIBE_LANG`)は提示層で実施。`app/src`(Vue3+Pinia) は会話ログ(narration/✦beat/┊recalled/rolls/reject) + 行動入力 + 状態パネル。**検証**: backend `cargo check` green（harness 結線が型検査通過）/ frontend `vue-tsc`+`vite build` green。GUI 実機起動（`npm run tauri dev`, WebView2 必須）は Windows 機で確認する段。アイコンは LocalAI 由来の Tauri デフォルト（配布時 `tauri icon` で差し替え）。
- ⬜ 次: (a) GUI 実機起動の確認（`cd app && npm run tauri dev`）。(b) UI の磨き込み（履歴保存・複数キャラ表示・narration ストリーミング等）。(c) 配布アイコン差し替え。〔神経 embedding は語彙乖離 paraphrase が要る時のみ＝現設計では不要〕

## ルーツ

LocalAI（`D:/Github/LocalAI`、廃棄）から摘出: `llm_client` / SSE ストリーミング骨格 / HybridMemory / Tauri UI 殻。
中核（正本・シナリオ脊椎）は新規。詳細な判断経緯は LocalAI のコードと本台帳を参照。

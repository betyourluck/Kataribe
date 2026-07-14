# 12. 統一ツール層 — LLM プロバイダアダプタ (Claude / GPT / Gemini / Grok)

Status: **Phase A〜D Done（2026-07-15 rev4 査読確定 → 同日実装）。残 = Phase E（実 4
プロバイダ live 検証）+ Phase F（grok-4.3 の A/B 検証と通しプレイ、実キー必要）**
- Phase A: canonical + seam + OpenAICompatAdapter（llm_client 30→32 PoC、挙動変更ゼロ）
- Phase B: ClaudeAdapter 正式化（`anthropic::encode(&canonical)` — build_request を統合）+
  `LLM_EFFORT` opt-in（`thinking: adaptive` + `output_config.effort`、未設定なら送らない）+
  非 fatal config 警告（headroom 16000/64000・temperature 併用、純関数 `warnings()`）。
  llm_client 32→35 PoC。summary 用 client は effort を継がない（要約に深い思考は不要）。
- Phase C: GeminiAdapter 新規（`gemini.rs` — generateContent の encode/decode 純関数、
  systemInstruction D3・ANY+allowedFunctionNames K2・`x-goog-api-key` ヘッダ K5・
  id 合成 `call_{seq}_{index}` は client 単位の AtomicU64 で Must 4 準拠）。
  `Provider` 三値化（`/openai` 除外規則で互換エンドポイント利用者を保護、
  語彙 gemini|google、パースは parse_env に共通化）。llm_client 35→38 PoC。
  **留意**: state_delta_schema の oneOf を Gemini の schema サブセットが受理するかは
  未確証 — Phase E の live 検証項目（拒否されたら schema 変換の Phase C.5 を起票）。
- Phase D: Grok `reasoning_effort` を対象モデル（grok-4.3/4.5、prefix 判定）に**既定送出
  （opt-out）** — 4.3→`none` / 4.5→`low`、`LLM_EFFORT` 明示は尊重（xhigh/max は high へ丸め）、
  fast 系/他モデルには送らない。empty-response 防御（text 空 ∧ tool_calls 空 ∧
  finish==length → EmptyResponse を一過性昇格、compat リトライで再抽選）。台帳追従
  （data_contract effort_dialects/empty_response_defense・.env.example）。
  **未決 2 解決**: app は `LLM_PROVIDER`/`LLM_EFFORT` を書いていない → env 手書きのまま。
  llm_client 38→40 PoC。
- workspace 247/247 green + clippy clean + app backend cargo check 通過。
  次 = Phase E/F（実キーでの live 検証 — Grok A/B・Gemini schema 受理・Claude cache 維持）。
rev 履歴: rev1 起草 → rev2 上流知識の導管 + streaming → rev3 主目的 = Grok tool-use 実用化
→ rev4 査読反映（Must 1/3/4 + Should a〜e 受諾、Must 2 の additive 説は claude-api
リファレンスで非確認のため combined 維持・headroom 推奨のみ受諾 — 本文に根拠）
Scope: `crates/llm_client` の内部を **canonical モデル + 3 アダプタ**（OpenAICompat=GPT+Grok /
Claude / Gemini）へ再編し、4 プロバイダを 1 インターフェースで束ねる。写経元は
aituber-onair フォークの `docs/unified-tool-layer-design.md`（2026-07-14、wire 事実は
`docs/llm-tts-wire-knowledge.md` に接地・再接地済み）。**harness / CLI は無改修**
（`generate` / `generate_structured` のシグネチャ不変）。app も backend は無改修だが、
設定 UI がプロバイダ選択肢を持つ場合のみ `gemini` の追加が要る（未決 2 — Phase D で確認）。

## 用語（本 spec 内で一貫して使う）

- **canonical** = プロバイダ中立の内部型（`ChatRequest` / `ChatResponse` / `ToolCall` /
  `ToolChoice`）。Driver 相当（呼び出し側）はこれしか見ない。
- **adapter** = canonical ⇄ 各社ワイヤの双方向翻訳 + HTTP（endpoint / 認証 / 方言 / 防御）を
  **内側に閉じ込める**単位。写経元 D5「プロバイダ防御は adapter の外に漏らさない」。
- **wire** = 各社の生 JSON 形。現行 `wire.rs`（OpenAI 互換）と `anthropic.rs` が既存の wire 層。
- **写経元** = `D:\Github\aituber-onair\docs\unified-tool-layer-design.md`。§番号はそちらの節。

## 目的（なぜ今やるか — 上流の運用知識を取り込む導管）

写経元の aituber-onair は **2 年近く継続開発され今も頻繁に更新される実運用 VTuber システム**で、
プロバイダごとの罠（wire 方言・推論モデルの挙動・空応答・streaming の組み立て）を
32 件の failures と wire-knowledge 台帳に蓄積している。Kataribe が独力で踏んできた同種の罠
（#28-30/#44-45 等）と系統が同じであり、**この層は一度きりの写経でなく、上流の知識更新を
継続的に取り込むための受け口**として設計する — adapter 単位で分離されていれば、上流で
新しい罠や方言が確定した時に該当 adapter だけを追従できる。

直近の具体目標は 2 つ:
1. **LLM 制御の改善** — 推論方言（Claude thinking/effort・Grok reasoning_effort）と
   プロバイダ防御を正しい場所（adapter 内）に持ち、モデルの性能を引き出す制御を
   プロバイダ横断で書けるようにする。
2. **Grok の tool-use 実用化（rev3 で目的を実態へ再接地）** — 現状 Grok は
   `use_tools=true` が実用にならず **no-tools JSON モードで運用**している。ところが
   JSON モードには **narration が極端に短くなる**副作用がある（実プレイ体感 —
   「schema に従う JSON だけ出せ」という prompt 指示が生成全体を構造化タスクに寄せ、
   文章の膨らみを削る。tool-use なら narration は自然文フィールドとして書かれ、この
   圧縮が掛からない）。よって語りの質のために **Grok でも tool-use を通す**ことが目標。
   2026-06-28 の三段修正（`inline_schema_defs` / `filter_authored_only_ops` / 却下 surface）
   で一度は goal 到達まで動いた記録があるので、**現在の失敗は当時と別の真因**。
   実測の症状は **grok-4.3 で空デルタまたはタイムアウト**（ユーザー記憶、2026-07-15）。
   **筆頭仮説（rev4 で査読により精密化）**: xAI の reasoning 既定は**モデル別** —
   grok-4.3 は reasoning-first（常時思考）で `reasoning_effort` は none/low/medium/high を
   設定可・**未送出時の既定は `low`**、grok-4.5 は既定 `high` で無効化不可。
   （wire-knowledge Part 4 の「API 既定は high」は grok-4.5 文脈の記述であり、
   grok-4.3 への一括適用は rev3 の過剰一般化だった。）grok-4.3 は既定 `low` でも
   **常時思考**なので、思考が `max_tokens`（既定 4096、思考+本文の合算）を食えば
   本文空＝空デルタ、長引けばタイムアウト（既定 120s）— 症状と両立する。
   上書き方針は不変: grok-4.3→`none` / grok-4.5→`low`（上流 repo と同判断）。
   仮説の白黒は Phase F の A/B（`none` 手挿し）で 2 往復で確定する。

## 問題（3 経路の分岐が client 本体に露出している）

現行 `llm_client` は OpenAI 互換 tool-use / no-tools JSON (#29) / Anthropic ネイティブ (#44) の
3 経路が `LlmClient::generate{,_structured}` 内の `if provider == ...` 分岐で同居する。
このままでは:

1. **Gemini ネイティブを足す場所がない** — 4 経路目の if を積むと、経路ごとの防御
   (キャッシュ計測 / sticky ヘッダ / CoT 除去) がどの経路に効いているか追えなくなる。
   Grok tool-use 対応 (2026-06-28) の三段デバッグは、この「経路ごとの差分が暗黙」の実害。
2. **推論方言 (thinking/effort, reasoning_effort) の置き場がない** — Claude の adaptive
   thinking + effort、Grok の `reasoning_effort` はプロバイダ固有 body であり、共有
   `ChatRequest` に生やすと他プロバイダへ漏れて 400 の芽になる（temperature 400 の再演）。
3. **互換層は機能の交差集合しか持たない** (#44 の一般化) — Gemini を OpenAI 互換
   エンドポイント経由で使う現状は動くが、ネイティブ固有の将来機能（thoughtConfig 等）へ
   到達する経路が構造的に無い。

## 決定（設計の核）

**採用するのは写経元の「canonical + adapter」だけ。Driver + ToolRegistry は採用しない（K1）。**
写経元 §5 の Driver ループは「LLM が複数ツールを選び、結果を返送して続きを話す」汎用
tool loop だが、Kataribe の構造は別物 — **単一ツール `emit_delta` を強制し、1 ターン 1 hop、
ループは harness `run_turn`（提案→裁定→却下理由還流→再生成）の専権**。ツール結果の返送
ターン（`ToolResult`）も存在しない。よって v1 で移植する価値があるのは §3（canonical）・
§4（adapter 契約）・§6（翻訳マトリクス）・§8a の decode 半分であり、§5/§8b は不要。
多ツール需要（memoria の tool 化 / AITuber 方向）が実際に出た時に Registry/Driver を
Phase 拡張する（canonical はその時そのまま使える形にしておく）。

### 写経元からの意図的逸脱（K 系 — 根拠つき）

- **K1 — Driver/Registry 不採用（上記）。** 逸脱ではなく部分採用。写経元は「参照実装であって
  drop-in でない」と自己宣言しており、これはその想定内の取捨。
- **K2 — `ToolChoice` に `Specific(name)` を追加。** 写経元は `Auto | Required | None` だが
  Kataribe の主経路は「特定ツールの強制」。3 方言への写像:
  OpenAI/Grok `{type:"function", function:{name}}` / Claude `{type:"tool", name}` /
  Gemini `functionCallingConfig: {mode:"ANY", allowedFunctionNames:[name]}`。
  （Auto/Required/None も canonical には持つが、v1 の利用者は Specific と None のみ。）
- **K3 — canonical `Message` は `{role, content: String}` を維持。** 写経元 §3 の
  `Assistant(tool_calls)` / `ToolResult` ターンは Kataribe に存在しない（却下理由は
  plain user メッセージで還流する既存設計）。系として **§8a の id 合成
  （id→name マップ）は v1 不要** — Gemini の難所は decode（`functionCall` → canonical
  `ToolCall`、args は最初からオブジェクト）だけに縮む。`ToolCall.id` は canonical 必須
  (写経元 D1) を保ち、Gemini adapter が合成して埋める（未消費でも型の一貫性を保つ —
  将来 Driver を足す時に型が変わらない）。**id の一意性（rev4・Must 4）**: 素朴な
  `call_<n>`（リクエスト毎リセット）は却下→再生成の同一ターン内で `call_0` が重複し、
  harness/ログが id で引くと衝突する。合成 id は **client 単位の単調カウンタ**
  （`conv_id` 生成と同じ AtomicU64 流儀、リセットしない）から採番し
  `call_{seq}_{index}` とする — 型を変えずに一意性を担保。
- **K4 — no-tools JSON モード (#29) は OpenAICompatAdapter の設定として存続。**
  写経元はツール対応プロバイダのみが対象で、no-tools はスコープ外。Kataribe の同人配布
  北極星（受領者は tool 非対応の安い/ローカルモデルを使う）由来の拡張であり、
  `use_tools=false` → tools を送らず `json_instruction` を積む現行動作を adapter 内へ移す。
  Anthropic / Gemini adapter では従来どおり無視（両者は tool_choice を確実に尊重する）。
- **K5 — Gemini 認証は query key でなく `x-goog-api-key` ヘッダ。** 写経元 §8a は
  `?key=` を使うが、API キーを URL に載せるとログ/プロキシ/エラーメッセージへ露出する。
  ヘッダ認証は Gemini API が公式にサポートする。live 確証は Phase E（万一通らなければ
  query key へフォールバックし、この行を訂正する）。
- **K6 — streaming は Phase F でスコープ内（rev2 で撤回・反転）。** rev1 は写経元 D4
  （tool 決定ターンは非ストリーミング）を「全ターン非ストリーミング」と読み替えて
  スコープ外にしたが、**Grok streaming の修理が本 spec の目的の一つ**（上記）なので反転。
  設計は写経元 §8b をそのまま採る: SSE の tool_calls 断片を index キーで蓄積し、
  `[DONE]` 後に**一度だけ** `json.loads` 相当を行う（断片ごとの parse が失敗の作り方）。
  **二重処理を明記（rev4・Should b）**: Grok/OpenAI は text delta と tool_calls delta が
  **同一ストリームに混在**する — text は到着順に流し（UI へ逐次）、tool_calls は蓄積して
  最後に組み立てる、の 2 トラックを assembler の契約にする。
  既定は現行どおり非ストリーミング（D4 の原則は保つ）— streaming は opt-in
  （narration の逐次表示という UI 価値と束で入れる）。
- **K7 — adapter の dispatch は enum。** Rust で `dyn` async trait を避ける
  （`enum AdapterKind { OpenAiCompat(..), Anthropic(..), Gemini(..) }` + match）。
  外部への trait 公開は不要（`LlmClient` の内側で閉じる）。

### 資産の保全（回帰の不変条件 — 全部 adapter の内側に残す）

写経元 D5 の Kataribe 実体。以下は**リファクタ後も観測可能に同一**であること:

| 資産 | 現在地 | 移設先 |
|---|---|---|
| Anthropic prompt caching (#44): system 末尾 `cache_control: ephemeral` | `anthropic.rs::build_request` | ClaudeAdapter encode |
| xAI sticky routing (#45): `x-grok-conv-id` ヘッダ | `client.rs::chat_once` | OpenAICompatAdapter の POST |
| キャッシュ計測 `CacheStat`（両経路常時記録・GUI ⚠ の一次ソース） | `chat_once` / `messages_once` に散在 | canonical `ChatResponse.usage{prompt, completion, cache_read}` へ**正規化**し、client 核が一元記録（抽出だけ adapter） |
| `[LLM_CACHE]` / `LLM_DEBUG` stderr 診断 | 両経路に重複実装 | client 核（送受信 hook）へ一元化 |
| Parse 失敗時の raw 保持 (#34) | `decode_chat_body` / `decode_messages_body` | 各 adapter decode（形式は共通ヘルパ） |
| 抽出の単一経路: `parse::extract`（tool_use 主経路 + フェンス JSON 救済 + `strip_reasoning_blocks` #30 + `json_objects`） | `parse.rs` | **不変** — canonical `ChatResponse` から従来どおり抽出 |
| schema 前処理: `inline_schema_defs` / `filter_authored_only_ops`（Grok 制約デコード対応） | `lib.rs` | **不変** |
| temperature は明示時のみ送る（None 既定） / max_tokens / 指数 backoff リトライ | `config.rs` / `client.rs` | **不変**（retry は client 核、body 詰めは adapter） |

### 新たに得る能力（写経元からの持ち込み）

1. **Gemini ネイティブ adapter**: `POST {base}/v1beta/models/{model}:generateContent`。
   `systemInstruction`（system を model ターンに畳まない — 写経元 D3）、
   `tools:[{functionDeclarations:[...]}]`、強制は K2 の `mode:ANY + allowedFunctionNames`。
   camelCase（v1beta は両様受理だが camelCase で統一）。decode は `candidates[0].content.parts[]`
   の text / functionCall。args は最初からオブジェクト（写経元 D2 — OpenAI 系だけが
   境界で文字列化/パースする）。
2. **Claude 推論方言（opt-in）**: 送る形は公式例に固定（rev4・Must 3）:
   `thinking: {"type": "adaptive"}` + `output_config: {"effort": "low"|"medium"|"high"|"xhigh"|"max"}`
   （effort は **output_config の中**、トップレベルでない。API 既定は high 相当）。
   `LLM_EFFORT` 未設定なら**どちらも送らない**=現行動作。注意 3 点:
   - **`max_tokens` は thinking+output の合算上限（combined）** — claude-api リファレンスが
     Opus 4.7/4.8・Sonnet 5 全てで支持（「xhigh/max では ≥64000、さもなくば思考の途中で
     切れる」「Sonnet 5 の max_tokens は total output (thinking + response text) の hard limit」）。
     査読の「Opus 4.8 は additive」説はリファレンスで確認できず**不採用**（Phase E の実測で
     反証されれば改訂）。実践則は採用: **effort 設定時は max_tokens ≥ 16000 を推奨**
     （xhigh/max は 64000 目安）、既定 4096 のままなら config 検証で警告。
   - **temperature の 400 はモデルレベル**（thinking との相互作用ではない）: Opus 4.8/4.7 は
     temperature/top_p/top_k 自体を常時 400、Sonnet 5 は非既定値を 400。現行の
     「None 既定・明示時のみ送る」がそのまま完全な防御。effort 実装時、Anthropic adapter で
     temperature 明示 + 対象モデルの組み合わせを警告する（送って 400 を貰うより早い）。
   - `budget_tokens` は送らない（Opus 4.8/Sonnet 5 で 400、adaptive が正）。
3. **Grok 推論方言（対象モデルには既定で送る — opt-in でなく opt-out）**:
   `reasoning_effort` は `grok-4.5` / `grok-4.3` のみ有効、`grok-4-1-fast-*` には送らない
   （モデル名分岐、adapter 内）。**未送出だと xAI 既定 `high` が適用される**ため、
   GM 用途（毎ターン 1 往復・レイテンシ直結）では対象モデルに **grok-4.3→`none` /
   grok-4.5→`low` を既定で明示送出**する（上流 repo と同判断）。`'none'` は grok-4.3
   のみ許可（grok-4.5 で希望した場合は `low` へ丸める）。`LLM_EFFORT` 明示があれば
   それを尊重（Claude と同じ env を共用、値語彙の写像は adapter 内）。
4. **empty-response 防御（rev4・Should c で具体化）**: 推論モデルが budget を全部思考に
   使い切って本文空、の対策（OpenRouter パターン）。判定条件を凍結:
   **text 空 かつ tool_calls 空 かつ `finish_reason == "length"`** の応答は
   `LlmError::EmptyResponse` を**一過性（is_transient=true）に昇格**し既存 backoff で
   リトライに乗せる（現状は非一過性で即失敗）。canonical `ChatResponse` に `finish` を
   運ぶのはこの判定のため。max_tokens 引き上げ・reasoning 除外等の能動防御は
   実測でリトライが不足と分かってから。

### プロバイダ判定（`Provider` 三値化 — 互換エンドポイント利用者を壊さない）

`Provider` を `OpenAiCompat | Anthropic | Gemini` に拡張。**罠**: Gemini を OpenAI 互換
エンドポイント（`.../v1beta/openai/`）で使う既存ユーザーの base_url にも
`generativelanguage.googleapis.com` が含まれる。自動判定は
「`generativelanguage.googleapis.com` を含み **かつ** `/openai` を含まない」時のみ
`Gemini`。明示 `LLM_PROVIDER=gemini` が最優先（`SUMMARY_LLM_PROVIDER` も同語彙に追従）。
既存の Anthropic 判定・`openai|openai_compat|compat` 語彙は不変。
**自動判定が届かないケース（rev4・Should a）**: 自前プロキシ（例 `my-gemini-proxy.example.com`）
は既知ホスト名を含まないため OpenAiCompat に落ちる — これは安全側（互換として動くか、
動かなければ明示指定へ誘導）。`.env.example` に「プロキシ経由の Gemini/Anthropic は
`LLM_PROVIDER` を明示せよ」の一文を添える。

## データ（data_contract 追記 — コードの前に凍結）

`data_contract.yaml` に `UnifiedToolLayer` 節: `Provider`（3 値 + 判定規則）/
canonical `ChatRequest{model, messages, tools, tool_choice, temperature?, max_tokens}` /
`ChatResponse{text?, tool_calls[], finish, usage?}`（`finish` は empty-response 防御=新能力 4 の
判定材料）/ `ToolCall{id, name, args:object}` / `ToolChoice{Specific(name) | Auto | Required | None}` /
`Usage{prompt, completion, cache_read}` / 環境変数（`LLM_PROVIDER` 3 語彙・`LLM_EFFORT`・
既存キー不変）。コメントとして Mistral vision の正形（未決 4）も凍結。

## 実装（Phase 分割、各 Phase Red→Green）

- **Phase A — canonical + seam + OpenAICompatAdapter（純リファクタ）**: canonical 型を
  新設し、現行 chat 経路（tool-use / no-tools / conv_id / キャッシュ計測）を
  OpenAICompatAdapter へ移設。`generate` / `generate_structured` は canonical を組んで
  adapter に渡すだけになる。**既存 PoC 30 本 green 維持が回帰証明**（挙動変更ゼロ）。
- **Phase B — ClaudeAdapter**: `anthropic.rs` を seam の裏へ（`build_request` の中身は
  ほぼ流用）。`LLM_EFFORT` opt-in + headroom 検査。PoC: encode の cache_control 位置 /
  effort 有無の body 固定。
- **Phase C — GeminiAdapter（新規）**: encode（systemInstruction / functionDeclarations /
  ANY+allowedFunctionNames / x-goog-api-key）+ decode（parts → canonical、id 合成）。
  `Provider::detect` の三値化と `/openai` 除外規則。PoC: ser/de 固定 + detect 境界
  （互換 URL は OpenAiCompat のまま）。
- **Phase D — Grok 方言 + 台帳追従**: `reasoning_effort` のモデル名分岐。
  `data_contract` / CLAUDE.md / `.env.example`（`LLM_PROVIDER=gemini` 例）/
  `failures.md`（実装中に踏んだ罠）。app 設定 UI のプロバイダ選択肢は要確認（未決 2）。
- **Phase E — 実 4 プロバイダ live 検証**: Claude（cache_read>0 維持）/ GPT / Gemini
  ネイティブ（K5 ヘッダ認証の確証 + 一発通しプレイ）/ Grok（cached 高率維持 +
  reasoning_effort）。`LLM_CACHE_DEBUG=1` の `[LLM_CACHE]` 行が計測装置。
- **Phase F — Grok tool-use 修理（本 spec の主目的の一つ、K6）**: 順序は仮説検証→修理。
  1. **筆頭仮説の検証**: 実 grok-4.3 + `LLM_USE_TOOLS=true` + `LLM_DEBUG=1` で
     (a) 現状再現（reasoning_effort 無し → 空/タイムアウトを観測 = Red）、
     (b) `reasoning_effort:"none"` を手挿しして同条件（即応 + tool_calls 到着 = Green 予告）。
     b が通れば真因確定、通らなければ wire-knowledge **Part 4（動作確認済み round-trip）**
     と送信 body の全差分列挙へ降りる（known-good が上流にあるのが今回の強み）。
  2. **修理**: 新能力 3（対象モデルへの reasoning_effort 既定送出）+ 必要なら
     新能力 4（empty-response 防御）。実ログを fixture にした Red→Green。
     完了判定は **grok-4.3 の通しプレイで tool-use 経路の goal 到達**（2026-06-28 と
     同じ判定基準）+ **narration 量の回復を機械判定（rev4・Should d）**: 同一シナリオ・
     同一台本 5 ターンの narration 平均文字数で `tool-use ≥ no-tools × 1.5` を目安に
     計測する（会話ログのテキスト保存機能がそのまま計測装置。1.5 は仮置きで、
     実測分布を見て閾値でなく報告値として扱ってもよい — 主観「増えた気がする」を
     数字にすることが目的）。
  3. **streaming assembler（§8b）は真因が streaming 絡みだった場合のみ**この Phase で
     実装（rev2 の主役想定から降格）。そうでなければ narration 逐次表示（UI 価値）と
     束ねて後続へ。

## 北極星との整合

三権分立は不変 — この層は「LLM が提案する」脚の**配管**であり、裁定・正本に触れない。
schema 機械生成（規格=実装の単一真実源）も不変で、canonical `ToolSpec.parameters` に
schemars 出力がそのまま流れる。同人配布の北極星（受領者ゼロ設定）は
`Provider::detect` の自動判定と opt-in 方言（未設定なら現行と同一の body）で守る。

## 未決

1. **Driver + ToolRegistry の導入時期** — memoria の tool 化 / AITuber 方向が具体化した時。
   canonical 型はその時に変えない（K3 の id 合成が効く）。
2. **app 設定 UI（AI モデルプロファイル）にプロバイダ選択を出すか** — Phase D で
   `app/src-tauri` を grep して `LLM_PROVIDER` を書いているか確認すれば即決（査読合意）。
   書いていれば `gemini` 追加、無ければ env 手書きのまま。
3. **`LLM_EFFORT` の既定** — v1 は「送らない」で確定（査読合意）。Grok だけ対象モデルへ
   既定送出する例外は「未送出でも xAI 側の既定が適用される」API 仕様のバグ回避であり、
   opt-in 原則の違反ではない。thinking がプレイ体感に見合うかは Phase E 実測後。
4. **Mistral vision** — 写経元スコープ外を踏襲（Kataribe は当面 vision 自体を送らない）。
   ただし正形は査読で確定済み（rev4・Should e）: `{"type":"image_url","image_url":{"url":...}}`
   の**オブジェクト形**が正、bare-string は上流 repo 実装の stale。将来の混乱防止に
   data_contract のコメントとして残す。
5. **Grok tool-use 失敗の再現材料** — 【rev3 で概ね解消】モデル = **grok-4.3**、症状 =
   空デルタまたはタイムアウト（ユーザー記憶）。筆頭仮説 = reasoning_effort 未送出 →
   xAI 既定 `high`（目的 2 参照）。残るのは Phase F-1 の live 検証のみ（当時の生ログは
   不要になった — 仮説が specific なので A/B 1 往復ずつで白黒がつく）。

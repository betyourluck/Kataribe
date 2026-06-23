# failures.md — Kataribe 罠台帳

> 実装中に踏んだ/予見した罠を 1 件 1 entry で残す。未来の自分への接地。
> 教訓 → 契約 → 実装の順序は silent degradation の温床なので、実装観察を一次資料にする。

## crates/llm_client (2026-06-23 移植時)

### 1. tool-use の `arguments` は「JSON オブジェクト」ではなく「JSON 文字列」
OpenAI 互換の tool_call は `function.arguments` を **文字列**で返す (`"{\"narration\":...}"`)。
ネストした object ではない。`ResponseMessage` をそのまま StateDelta に deserialize できると
誤設計すると壊れる。`arguments: String` で受けて **二段階パース** (`serde_json::from_str`) する。
→ wire.rs `FunctionCallResponse.arguments: String` / parse.rs `extract`。

### 2. `tool_choice` 強制を尊重しないサーバ/モデルがある
互換サーバ・一部モデルは `tool_choice: {function}` を無視して content に直接 JSON を吐く。
tool_calls だけを前提にすると NoStructuredOutput で死ぬ。
→ フォールバック: tool_calls 不在なら content をフェンス除去して再パース (Python generate_json 同型)。
parse.rs `extract` の二経路。

### 3. provider ごとに JSON Schema のサブセットが違う (未検証・watch)
schemars が生成する schema は `$defs` / `$ref` / `title` / `$schema` を含む。
OpenAI は概ね受けるが、厳格な structured-output モード (`strict: true`) や一部 provider は
`$ref` や `additionalProperties` の扱いで弾く可能性がある。**実クラウド通しプレイで要検証**。
弾かれたら: (a) schema を inline 展開する、(b) strict を外す、(c) tool description で補強。
現状は素の schemars 出力をそのまま渡している (PoC)。
✅ **解決 (2026-06-23)**: claude-opus-4-8 @ Anthropic OpenAI 互換層は schemars 出力
($defs/$ref/title 含む) を**そのまま受理**。密室脱出 通しプレイ成功 (4/4 一発合格・goal 到達)。
他 provider (OpenAI strict モード等) は未検証だが、少なくとも Anthropic 互換では inline 展開不要。

### 4. パース失敗時に raw を捨てると再生成できない
`adjudicate` の Reject も JSON パース失敗も、LLM に**戻して直させる**のが self_repair の核。
raw を握りつぶすと「何を直せばいいか」を LLM に伝えられない。
→ `LlmError::Parse { source, raw }` で raw を保持。これが GM ターンループ(次フェーズ)の燃料。

### 5. ネットワーク経路はテストで固められない (PoC スコープの線引き)
実 API は鍵必須 + 非決定的。`chat_once`/`chat_with_retry` は単体テスト対象外にした。
代わりに **壊れる ser/de 境界** (wire 整形 / parse 二経路 / schema 生成 / config / 一過性判定) を
決定論テストで固めた (9 件)。実 API 通しは「実クラウド通しプレイ」フェーズに分離。
教訓: 非決定的な外部 I/O と決定論的な変換ロジックを**型で分離**しておくと、PoC で何を
証明できて何を後回しにするかの線が引ける。

### 6. reqwest は rustls-tls を明示する (決定論ツールチェーン)
既定 features の native-tls は系 OpenSSL に依存しうる。
`default-features = false, features = ["json", "rustls-tls"]` で系非依存に倒した。
compiler_version_policy (再現可能なツールチェーン) と同精神。

## crates/harness (2026-06-23 GM ターンループ)

### 7. 実 LLM 直結だと「却下→再生成」ロジックをテストできない
ループが `LlmClient::generate_delta` を直接呼ぶ設計にすると、self_repair の核心
(嘘を却下し理由を戻して直させる) が実 API + 非決定的応答なしには検証できなくなる。
→ `DeltaProposer` trait で依存性逆転。本番=LlmClient, テスト=ScriptedProposer(台本付き fake)。
ループは trait に対して書き、却下→再生成を**実 API なしで決定論 Green** にできた (6 件)。
llm_client #5 と同じ「非決定的 I/O と決定論ロジックを型で分離」の再適用。

### 8. messages はターンごとに state から再構築する (履歴に古い状態を溜めない)
会話履歴を延々と積み増すと、過去ターンの古い state 記述が文脈に残り「忘れない GM」の逆になる。
→ run_turn は毎ターン `scenario_brief + state_brief(現在の正本)` を新規に組む。
却下→再生成の within-turn だけ assistant/user を積む (その範囲は同一 state なので一貫)。
state が唯一の真実、文脈はそのスナップショット、という北極星の prompt 層での具体化。

### 9. `apply().expect()` は adjudicate との結線前提 — 乖離したら panic
run_turn は `adjudicate == Accept` を確認してから `apply(...).expect("adjudicate 済みなら成功")`。
これは「apply は adjudicate を内部で再実行し、同じ判定を返す」という gm_core の不変条件に依存する。
将来 adjudicate と apply の検証ロジックが乖離すると expect が panic する = 早期に気付ける設計上の
アラーム (silent な不整合より good)。ただし両者の検証を**二重管理しない**規律が前提 (gm_core 側の掟)。

### 10. async fn in trait の警告は in-crate 限定で allow
`DeltaProposer::propose` を native async fn in trait にすると `async_fn_in_trait` 警告
(auto-trait 漏れ / dyn 化困難の注意)。本 trait は harness 内でしか実装/消費せず generic で受ける
ので dyn 不要 → `#[allow(async_fn_in_trait)]` で抑制。外部公開 API になるなら要再検討。

### 11. `Box<dyn Error>` 返り値は最初の具体 Box 構築に推論固定される
bin `play` の `main() -> Result<(), Box<dyn Error>>` で、`return Err(Box::new(io_err))` と
書いたら戻り値エラー型が `Box<io::Error>` に**推論固定**され、他の `?` (String / serde_yaml::Error
→ Box<dyn Error>) の From 変換が全滅 (E0277 連鎖 5 件)。
→ 具体エラーは `Box::new(e)` でなく `e.into()` で返す。`From<E> for Box<dyn Error>` が効いて
dyn に widening される。戻り値が dyn なら**全ての error 構築を .into() に統一**するのが安全。
症状(5件のFrom未実装)は派手だが根は1行。mandate_logical_friction_processing の実例。

## crates/llm_client (2026-06-23 実 API 初投入で判明)

### 12. 新しめのモデルは `temperature` を非対応にしており送ると 400
claude-opus-4-8 @ Anthropic 互換層は `temperature` パラメータを deprecated 扱いで拒否
(`400 "temperature is deprecated for this model"`)。LocalAI は常に temperature を送っていたが、
クラウドの新モデルでは弾かれる。
→ `ChatRequest.temperature: Option<f32>` + `skip_serializing_if`。`LlmConfig.temperature` も
Option にし、**`LLM_TEMPERATURE` 明示時のみ送る** (既定は省略 = provider 既定に委ねる)。
.env.example も既定でコメントアウト。tool_choice 強制が構造を保証するので温度固定は不要だった。
教訓: 互換 API でも「全 provider 共通の必須パラメータ」は思ったより少ない。未指定で provider
既定に委ねるのが最も壊れにくい。送る前提でなく省略を既定にする。

### 13. `from_env()` が `dotenvy::dotenv()` を呼ぶとテスト不能になる
「api_key 欠落で Config エラー」を検証するテストが、実 .env 存在時に失敗。理由: from_env 内の
dotenv が .env のキーをプロセス env に再注入し、`remove_var` を打ち消す。
→ **.env 読み込みはアプリ入口 (bin main) の責務に移す**。from_env は env を読むだけ (副作用なし)。
慣習的にも正しい分離 (lib は環境を勝手に書き換えない)。dotenvy 依存も llm_client → harness へ移動。
教訓: テスト不能は設計の臭い。グローバル副作用 (env 書き換え) を純粋な読み取りから分離する。

## context7 接地で判明 (2026-06-23, 公式 platform.claude.com docs)

### 14. parallel tool use は既定 ON — first tool_call だけ読むと残りを黙殺
native Messages API の tool_choice は `disable_parallel_tool_use` を持ち、**既定では複数の
tool_use ブロックを返しうる**。OpenAI 互換層でも response.tool_calls が複数になる可能性がある。
parse::extract は `tool_calls.first()` だけ採用するので、モデルが emit_delta を複数返すと
残りを**黙って捨てる** = 北極星「矛盾しない」に反する潜在バグ。
現状: 単一ツールを tool_choice 強制しており通しプレイでは 1 件のみ返った (未発火)。
将来対策案: (a) 複数 tool_call 検出時は明示エラー or 先頭採用をログ化、(b) OpenAI 互換層で
parallel 抑制を渡せるか確認 (native は disable_parallel_tool_use、互換層は extra_body 経由か要調査)。

### 15. 再生成は tool_use→tool_result プロトコルを意図的に回避している
公式: forced tool の assistant 応答の後、native では tool_use ブロックに対応する tool_result を
返すのが正規 (tool_use_id で対応、tool_result の後にテキストを置くと invalid)。
我々の push_rejection は **応答の tool_calls を保持せず**、提案を**プレーン assistant テキストで
echo** + 却下理由を user テキストで積む → 履歴に dangling tool_call が無いので tool_result 要求を
回避できる。これは設計判断として正しい (我々は「ツールの出力に反応させる」のでなく「再提案させる」
ため)。ただし**却下→再生成の実 API 挙動は未検証** (happy path で発火せず)。敵対プレイで初検証する。
注意: 将来 maintainer が「正規の tool_use+tool_result に直す」と、forced tool 後に tool_result が
必須になり、かえって複雑化する。現設計 (プレーン echo) は意図的選択であることを明記。
✅ **検証 (2026-06-23, 敵対プレイ)**: 複数ステップを束ねた行動で LLM が原子性違反デルタを提案
→ エンジン却下 → プレーン echo + 却下理由を user テキストで還流 → LLM が合法な部分手に修正
(attempts=2, 2 ターンで再現)。**再生成のメッセージ形は実 Anthropic API で通る**ことを実証。
副産物: LLM は scenario_brief の gate を読み、不可能な単独行動 (解錠前 move・幻 master_key) は
そもそも提案せず narration で拒否した (prompt 層接地が有効)。却下が発火したのは「欲張って束ねた」
時のみ = 正本の原子性が「一手ずつの正しい前進」を強制する設計が実 LLM で機能。

## crates/gm_core (2026-06-23 数値ステータス PoC)

### 17. Gate/StateOp に variant を足すと全 match 箇所がコンパイルエラー (= 機能、罠でない)
`Gate::StatAtLeast` 追加で `harness/prompt.rs::gate_brief` が non-exhaustive エラー (E0004)。
これは**バグでなく設計の利点** ── 網羅 match が「新条件を扱い忘れる」のを構造的に防ぐ
(北極星「矛盾しない」のコンパイラ強制版)。variant 追加時の更新箇所チェックリスト:
(a) gm_core engine.rs `adjudicate` (検証) + `apply` (適用), (b) spine.rs `Gate::eval`,
(c) harness prompt.rs `gate_brief` (LLM への日本語化), (d) llm_client schema テスト
(StateOp 追加時、schemars が自動で schema に載せるので**プロンプト変更は不要** = 機械生成の利点)。

### 16. (軽微) narration に二重エスケープ \n が混じることがある
敵対プレイ turn4 で narration に literal `\n\n` が出た。モデルが tool 引数 JSON に `\\n\\n`
(二重エスケープ) を書いたため、serde で 1 段戻しても `\n` が残った。我々のバグではなくモデル出力の癖。
UI 層を繋ぐ時は narration を表示前に正規化する (literal `\n` → 改行 or 除去) と良い。低優先。

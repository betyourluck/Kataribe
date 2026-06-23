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

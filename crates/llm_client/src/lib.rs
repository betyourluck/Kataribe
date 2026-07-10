//! # llm_client — クラウド LLM ナレーター脚
//!
//! 三権分立の「**LLM=提案**」脚。OpenAI 互換 chat/completions を `base_url`+`api_key` で抽象化し、
//! **tool-use 強制**で gm_core の [`StateDelta`](gm_core::StateDelta) を構造化出力させる。
//!
//! 正本 (gm_core) は LLM 非依存のまま。この crate は LLM の提案を **型に絞り込んで** 渡すだけで、
//! 裁定 (`adjudicate`) は一切しない。却下→再生成のループは上位 (harness) が担う。
//!
//! LocalAI `core_agent/src/llm_client.py` の移植:
//! - `generate`        → [`LlmClient::generate`]
//! - `generate_json`   → [`LlmClient::generate_structured`] (+ tool-use 強制)
//! - `LLMSettings`     → [`LlmConfig`]
//! - `_strip_code_fence` / フェンス除去 → [`parse::strip_code_fence`]

mod anthropic;
mod client;
mod config;
mod error;
mod parse;
mod wire;

pub use client::LlmClient;
pub use config::{LlmConfig, Provider};
pub use error::LlmError;
pub use wire::{ChatMessage, Role};

use gm_core::StateDelta;

/// LLM に StateDelta を出させる時のツール名。
pub const EMIT_DELTA_TOOL: &str = "emit_delta";

const EMIT_DELTA_DESCRIPTION: &str = "\
今ターンの語り (narration) と、世界状態への変更要求 (ops) を提出する。\
narration は情景・NPC 台詞を自由に書いてよい。\
ops は構造化された要求のみで、エンジンが全件検証する。\
存在しないアイテムの取得や、達成していない移動を ops に書いても却下されるので、嘘の状態変更を書いてはならない。";

/// gm_core の型から機械生成した [`StateDelta`] の JSON Schema を返す。
///
/// **手書きしない** ── 規格 (schema) と実装 (Rust 型) の乖離は北極星「矛盾しない」に反する。
/// schemars が serde 属性 (`#[serde(tag = "op")]` 等) を尊重して単一真実源から導出する。
///
/// **自己完結化** ([`inline_schema_defs`]): schemars は `ops` 要素を `#/definitions/StateOp` への
/// `$ref` で出すが、tool-call grammar へ schema をコンパイルするサーバ (xAI Grok 等) は `$ref`/
/// `definitions`/`$schema` を解決しない (docs に記載なし) → `ops` を制約できず空デルタになる。
/// `$ref` を実体に inline し `definitions`/`$schema` を落として**どのプロバイダでも自己完結**にする
/// (Anthropic は $ref を解決できるが、Grok/OpenAI 厳格系は自己完結を要する。互換性の上位互換)。
pub fn state_delta_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(StateDelta);
    let value = serde_json::to_value(schema).expect("schemars 生成スキーマは必ず JSON 化できる");
    let inlined = inline_schema_defs(&value);
    filter_authored_only_ops(inlined)
}

/// `ops` の oneOf から **authored 専権 op** ([`gm_core::AUTHORED_ONLY_OPS`]) を除く。
///
/// これらは LLM が提案しても `adjudicate` が必ず却下する (trigger 効果でのみ実行)。schema に残すと
/// LLM が使い続けて却下→再生成ループで詰まる (特に constrained decoding な Grok は grammar に含めて
/// しまう)。除外すれば **LLM はそもそも提案できない** (Grok でも grammar に出ない=構造的遮断)。
fn filter_authored_only_ops(mut schema: serde_json::Value) -> serde_json::Value {
    if let Some(variants) = schema
        .pointer_mut("/properties/ops/items/oneOf")
        .and_then(|v| v.as_array_mut())
    {
        variants.retain(|variant| {
            let op = variant
                .get("properties")
                .and_then(|p| p.get("op"))
                .and_then(|o| o.get("enum"))
                .and_then(|e| e.as_array())
                .and_then(|a| a.first())
                .and_then(|s| s.as_str());
            !matches!(op, Some(name) if gm_core::AUTHORED_ONLY_OPS.contains(&name))
        });
    }
    schema
}

/// JSON Schema の `$ref` (`#/definitions/X` / `#/$defs/X`) を実体に inline し、
/// `definitions`/`$defs`/`$schema` キーを除去して**自己完結スキーマ**にする。
///
/// tool-call grammar コンパイラ (Grok 等) が参照解決をしない問題への対処。`seen` で展開中の
/// 定義名を追い、循環参照は空 object に落として無限再帰を防ぐ (現状の gm_core 型は非再帰だが安全側)。
pub fn inline_schema_defs(schema: &serde_json::Value) -> serde_json::Value {
    let defs = schema
        .get("definitions")
        .or_else(|| schema.get("$defs"))
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    inline_value(schema, &defs, &mut Vec::new())
}

fn inline_value(
    value: &serde_json::Value,
    defs: &serde_json::Map<String, serde_json::Value>,
    seen: &mut Vec<String>,
) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => {
            // `$ref` は実体へ差し替え (循環は空 object で打ち切り)。
            if let Some(Value::String(r)) = map.get("$ref") {
                let name = r
                    .strip_prefix("#/definitions/")
                    .or_else(|| r.strip_prefix("#/$defs/"));
                if let Some(name) = name {
                    if seen.iter().any(|s| s == name) {
                        return Value::Object(serde_json::Map::new());
                    }
                    if let Some(def) = defs.get(name) {
                        seen.push(name.to_string());
                        let inlined = inline_value(def, defs, seen);
                        seen.pop();
                        return inlined;
                    }
                }
            }
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                // メタ/参照キーは自己完結スキーマから落とす。
                if matches!(k.as_str(), "$ref" | "definitions" | "$defs" | "$schema") {
                    continue;
                }
                out.insert(k.clone(), inline_value(v, defs, seen));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|v| inline_value(v, defs, seen)).collect()),
        other => other.clone(),
    }
}

impl LlmClient {
    /// 1 ターン分の [`StateDelta`] を LLM に提案させる便宜メソッド。
    ///
    /// `messages` は呼び出し側 (harness) が構築する ── GM ペルソナ・シナリオ要約・
    /// 直近の却下理由 (再生成時) を system/user に積む責務は上位レイヤにある。
    /// この crate は transport + 構造化出力の保証だけを担う。
    pub async fn generate_delta(
        &self,
        messages: Vec<ChatMessage>,
    ) -> Result<StateDelta, LlmError> {
        let delta = self
            .generate_structured::<StateDelta>(
                messages,
                EMIT_DELTA_TOOL,
                EMIT_DELTA_DESCRIPTION,
                state_delta_schema(),
            )
            .await?;
        // narration は非検証ゆえ、漏れた tool-call マークアップを提示前に掃除する
        // (ops は検証済の別フィールドで無改変)。
        Ok(StateDelta::new(
            parse::sanitize_narration(&delta.narration),
            delta.ops,
        ))
    }
}

// =============================================================================
// PoC: ナレーター脚の実証 (Red→Green)
// ネットワークは実キー必要で非決定的。壊れる ser/de 境界 (wire/parse/schema/config)
// を決定論的に固める。実 API 通しは「実クラウド通しプレイ」フェーズで行う。
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{
        ChatRequest, ChatResponse, Choice, FunctionCallResponse, ResponseMessage, ToolCallResponse,
    };
    use gm_core::StateOp;

    fn user_msgs() -> Vec<ChatMessage> {
        vec![ChatMessage::system("あなたはGM"), ChatMessage::user("引き出しを開ける")]
    }

    /// 【スキーマ機械生成】StateDelta schema が narration/ops を持ち、
    /// 全 StateOp バリアントの判別子 "op" を含む (規格=実装の単一真実源)。
    #[test]
    fn schema_is_generated_from_canonical_types() {
        let schema = state_delta_schema();
        let s = serde_json::to_string(&schema).unwrap();
        // StateDelta のフィールド
        assert!(s.contains("narration"), "narration プロパティが schema に無い");
        assert!(s.contains("ops"), "ops プロパティが schema に無い");
        // StateOp の内部タグと各 op 値 (serde rename_all=snake_case)
        for op in [
            "add_item", "remove_item", "set_flag", "move", "request_roll",
            "adjust_stat", "scale_stat",
        ] {
            assert!(s.contains(op), "op '{op}' が schema に無い (型と乖離)");
        }
        // 【自己完結 (Grok 対応)】$ref/definitions/$schema を残さない (tool-call grammar コンパイラが
        // 参照解決しないサーバでも ops を制約できるよう inline 済み)。
        assert!(!s.contains("$ref"), "$ref を inline で消す: {s}");
        assert!(!s.contains("definitions") && !s.contains("$defs"), "definitions を落とす");
        assert!(!s.contains("$schema"), "$schema メタを落とす");
    }

    /// 【$ref inline の健全性】inline_schema_defs が参照を実体へ展開し、ops 配列要素の中に
    /// 各 op の判別子が直接現れる ($ref 経由でなく自己完結)。
    #[test]
    fn inline_schema_defs_resolves_refs_self_contained() {
        let schema = state_delta_schema();
        // ops プロパティの items が $ref でなく実体 (oneOf の枝) を持つ。
        let ops_items = &schema["properties"]["ops"]["items"];
        assert!(ops_items.get("$ref").is_none(), "ops.items は $ref でなく実体");
        let dump = serde_json::to_string(ops_items).unwrap();
        assert!(dump.contains("add_item") && dump.contains("set_flag"), "op 実体が ops.items 内に inline");
    }

    /// 【authored 専権 op の除外】LLM 向け schema は set_presence/grant_skill 等を **提案肢に出さない**
    /// (露出すると LLM が使い続けて却下→再生成ループで詰まる。Grok の constrained decoding 対策の核心)。
    /// LLM が使える op (add_item 等) は残る。
    #[test]
    fn schema_excludes_authored_only_ops() {
        let schema = state_delta_schema();
        let variants = schema["properties"]["ops"]["items"]["oneOf"]
            .as_array()
            .expect("ops.items.oneOf は配列");
        let op_tags: Vec<String> = variants
            .iter()
            .filter_map(|v| v["properties"]["op"]["enum"][0].as_str().map(String::from))
            .collect();
        // authored 専権 op は1つも露出しない。
        for banned in gm_core::AUTHORED_ONLY_OPS {
            assert!(!op_tags.iter().any(|t| t == banned), "authored 専権 op '{banned}' を schema から除外");
        }
        // LLM が使える代表 op は残る。
        for keep in ["add_item", "set_flag", "move", "adjust_stat", "check", "attempt_challenge"] {
            assert!(op_tags.iter().any(|t| t == keep), "提案可能な op '{keep}' は残す");
        }
    }

    /// 【リクエスト整形】generate_structured が tool を載せ、tool_choice で強制する。
    #[test]
    fn request_forces_the_emit_delta_tool() {
        let req = ChatRequest {
            model: "m".into(),
            messages: user_msgs(),
            temperature: Some(0.1),
            max_tokens: 256,
            tools: vec![wire::Tool {
                kind: wire::ToolKind::Function,
                function: wire::FunctionDef {
                    name: EMIT_DELTA_TOOL.into(),
                    description: "d".into(),
                    parameters: state_delta_schema(),
                },
            }],
            tool_choice: Some(wire::ToolChoice::force(EMIT_DELTA_TOOL)),
        };
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["tool_choice"]["type"], "function");
        assert_eq!(body["tool_choice"]["function"]["name"], EMIT_DELTA_TOOL);
        assert_eq!(body["tools"][0]["function"]["name"], EMIT_DELTA_TOOL);
        // f32→JSON は精度差が出るので近似比較 (明示時は temperature を送る)。
        assert!(
            (body["temperature"].as_f64().unwrap() - 0.1).abs() < 1e-3,
            "明示時は temperature を送る"
        );
        // ツール無しの generate ではキーごと消える (skip_serializing_if)。
        let plain = ChatRequest {
            model: "m".into(),
            messages: user_msgs(),
            temperature: None,
            max_tokens: 256,
            tools: vec![],
            tool_choice: None,
        };
        let pbody = serde_json::to_value(&plain).unwrap();
        assert!(pbody.get("tools").is_none(), "ツール無しなら tools は出さない");
        assert!(pbody.get("tool_choice").is_none());
        // temperature 未設定 (None) なら **キーごと送らない** (新しめモデルは送ると 400)。
        assert!(
            pbody.get("temperature").is_none(),
            "temperature None なら省略する (claude-opus-4-8 等は temperature 非対応)"
        );
    }

    fn response_with_tool_args(args: &str) -> ChatResponse {
        ChatResponse {
            usage: None,
            choices: vec![Choice {
                message: ResponseMessage {
                    content: None,
                    tool_calls: vec![ToolCallResponse {
                        function: FunctionCallResponse {
                            arguments: args.into(),
                        },
                    }],
                },
            }],
        }
    }

    /// 【主経路】tool_calls の arguments (JSON 文字列) を StateDelta に解決する。
    #[test]
    fn parses_state_delta_from_tool_call() {
        let resp = response_with_tool_args(
            r#"{"narration":"古い引き出しが軋む","ops":[{"op":"set_flag","key":"drawer_opened","value":true}]}"#,
        );
        let delta: StateDelta = parse::extract(resp.first_message().unwrap()).unwrap();
        assert_eq!(delta.narration, "古い引き出しが軋む");
        assert_eq!(delta.ops.len(), 1);
        assert!(matches!(
            &delta.ops[0],
            StateOp::SetFlag { key, value: true } if key == "drawer_opened"
        ));
    }

    /// 【フォールバック】tool_calls 不在でも content のフェンス JSON から解決する
    /// (tool_choice を尊重しないサーバ/モデルへの保険、Python generate_json 同型)。
    #[test]
    fn falls_back_to_fenced_json_in_content() {
        let resp = ChatResponse {
            usage: None,
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some(
                        "```json\n{\"narration\":\"扉を調べる\",\"ops\":[]}\n```".into(),
                    ),
                    tool_calls: vec![],
                },
            }],
        };
        let delta: StateDelta = parse::extract(resp.first_message().unwrap()).unwrap();
        assert_eq!(delta.narration, "扉を調べる");
        assert!(delta.ops.is_empty());
    }

    /// 【no-tools モード】tool_calls 無しで content が素の JSON / prose 包みでも StateDelta に解決する
    /// (さくら AI Engine 等 tool_choice 非対応サーバの経路。LLM_USE_TOOLS=false)。
    #[test]
    fn parses_state_delta_from_plain_and_prose_content() {
        // 素の JSON (コードフェンス無し)。さくら gpt-oss-120b の実観測形。
        let raw = ChatResponse {
            usage: None,
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some(r#"{"narration":"教室に入る","ops":[]}"#.into()),
                    tool_calls: vec![],
                },
            }],
        };
        let d: StateDelta = parse::extract(raw.first_message().unwrap()).unwrap();
        assert_eq!(d.narration, "教室に入る");
        assert!(d.ops.is_empty());

        // prose に前後を包まれても first '{'..last '}' で救済。
        let prose = ChatResponse {
            usage: None,
            choices: vec![Choice {
                message: ResponseMessage {
                    content: Some("はい:\n{\"narration\":\"了解\",\"ops\":[]} 以上".into()),
                    tool_calls: vec![],
                },
            }],
        };
        let d2: StateDelta = parse::extract(prose.first_message().unwrap()).unwrap();
        assert_eq!(d2.narration, "了解");
    }

    /// 【推論モデルの no-tools 救済 (#30)】Gemma 等は `<thought>...</thought>` で CoT を吐き、
    /// その中に JSON 断片 (`{"op":...}`) を書いてから ```json フェンスで本体を出す。旧コードは
    /// (a) フェンスが先頭に無く strip_code_fence が効かない (b) first '{' が thought 内の断片に
    /// 釣られる の二重欠陥で parse 失敗していた。推論ブロック除去で解決する。
    #[test]
    fn reasoning_block_then_fenced_json_resolves() {
        let raw = "<thought>* Player asks for math materials.\n\
                   * Ops: `[{\"op\": \"adjust_stat\", \"entity\": \"moka\", \"key\": \"好感度\", \"delta\": 1}]`\
                   </thought>```json\n\
                   { \"narration\": \"モカは教材棚を顎で指した。\", \"ops\": [ { \"op\": \"adjust_stat\", \"entity\": \"moka\", \"key\": \"好感度\", \"delta\": 1 } ] }\n\
                   ```";
        let resp = ChatResponse {
            usage: None,
            choices: vec![Choice {
                message: ResponseMessage { content: Some(raw.into()), tool_calls: vec![] },
            }],
        };
        let d: StateDelta = parse::extract(resp.first_message().unwrap()).unwrap();
        assert_eq!(d.narration, "モカは教材棚を顎で指した。");
        assert_eq!(d.ops.len(), 1, "thought 内の断片でなく本体の ops を拾う");
    }

    /// 【最後の object を採る (#30)】StateDelta は narration/ops が serde(default) なので無関係な
    /// 断片 `{...}` すら空デルタとして parse 成功する。タグ無しで前置き断片が混じっても、
    /// 「答えは推論の後に来る」原則で **最後の balanced object** を採り本体を拾う (first '{' では空を拾う罠)。
    #[test]
    fn last_balanced_json_object_wins() {
        let raw = "consider {\"op\":\"x\"} then answer:\n{\"narration\":\"本体\",\"ops\":[]}";
        let resp = ChatResponse {
            usage: None,
            choices: vec![Choice {
                message: ResponseMessage { content: Some(raw.into()), tool_calls: vec![] },
            }],
        };
        let d: StateDelta = parse::extract(resp.first_message().unwrap()).unwrap();
        assert_eq!(d.narration, "本体", "前置き断片でなく最後の object を採る");
    }

    /// 【JSON モード指示】json_instruction は schema (narration/ops) と「JSON だけ出せ」旨を含む。
    #[test]
    fn json_instruction_carries_schema_and_directive() {
        let s = crate::client::json_instruction(&state_delta_schema());
        assert!(s.contains("JSON"), "JSON 出力指示を含む");
        assert!(s.contains("narration") && s.contains("ops"), "schema (narration/ops) を含む");
    }

    /// 【既定 tool-use】LlmConfig::new / 未設定時は use_tools=true (OpenAI/Anthropic 既定経路)。
    #[test]
    fn config_defaults_to_tool_use() {
        assert!(LlmConfig::new("u", "k", "m").use_tools, "既定は tool-use ON");
    }

    /// 【ops が文字列に化ける崩れの救済 (#40)】Gemini 実プレイで観測: `"ops": "\n"` (配列で
    /// あるべき場所に文字列) を出し、パース失敗で 9 ターン中 4 ターンが丸ごと蒸発した。
    /// 決定論的に救済する — 空白のみの文字列 → 空配列、JSON 配列の二重エンコード → その配列。
    #[test]
    fn ops_as_string_is_rescued() {
        let resp = response_with_tool_args(r#"{"narration":"夜が更ける","ops":"\n"}"#);
        let d: StateDelta = parse::extract(resp.first_message().unwrap()).unwrap();
        assert_eq!(d.narration, "夜が更ける");
        assert!(d.ops.is_empty(), "空白のみの ops 文字列は空配列として救済");

        let resp = response_with_tool_args(
            r#"{"narration":"n","ops":"[{\"op\":\"set_flag\",\"key\":\"k\",\"value\":true}]"}"#,
        );
        let d: StateDelta = parse::extract(resp.first_message().unwrap()).unwrap();
        assert_eq!(d.ops.len(), 1, "二重エンコードされた ops は配列として救済");
    }

    /// 【再生成の燃料】壊れた JSON は raw を保持した Parse エラーになる
    /// (却下→再生成ループが raw を LLM に戻せること = self_repair 同型の前提)。
    #[test]
    fn malformed_json_keeps_raw_for_repair() {
        let resp = response_with_tool_args(r#"{"narration":"壊れた,"ops":["#);
        let err = parse::extract::<StateDelta>(resp.first_message().unwrap()).unwrap_err();
        match err {
            LlmError::Parse { raw, .. } => assert!(raw.contains("壊れた"), "raw を再生成用に保持すべき"),
            other => panic!("Parse エラーであるべき: {other:?}"),
        }
    }

    /// 【200 なのに形が合わない応答は本文を保持する (#34)】Gemini 実プレイで観測:
    /// 長セッション中に HTTP 200 で `choices[0]` に `message` が無い応答が返り、
    /// `resp.json()` 直はデコード失敗時に**本文を捨てる**ため
    /// 「missing field `message` at line 1 column 76」だけが残り真因 (content filter /
    /// 長さ切れ / quota 系の変形応答) が診断不能だった。text→parse に統一し raw を保持する。
    #[test]
    fn decode_chat_body_keeps_raw_on_shape_mismatch() {
        let body = r#"{"choices":[{"finish_reason":"content_filter","index":0}],"created":1}"#;
        let err = client::decode_chat_body(body.to_string()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("content_filter"), "応答本文 (真因) が surface される: {msg}");
        assert!(msg.contains("message"), "何が欠けたか (missing field) も出る: {msg}");
    }

    /// 【narration 掃除】モデルが narration 本文に漏らした tool-call マークアップを除去する
    /// (実プレイで観測: `</narration>` / `<parameter name="ops">` が語りに混入)。
    #[test]
    fn sanitizes_leaked_tool_markup_from_narration() {
        // 観測された実例: narration の末尾に閉じタグ + ops の format token が漏れた。
        let leaked = "けらけら、と笑う。夕日の中で小さく光っていた。</narration>\n<parameter name=\"ops\">[{\"op\":\"set_flag\"}]";
        let clean = parse::sanitize_narration(leaked);
        assert_eq!(clean, "けらけら、と笑う。夕日の中で小さく光っていた。");
        assert!(!clean.contains("</narration>") && !clean.contains("<parameter"));

        // 先頭の開きタグも剥がす。
        assert_eq!(parse::sanitize_narration("<narration>本文だけ</narration>"), "本文だけ");
        // Anthropic XML 関数呼び出しタグも切る。
        assert!(!parse::sanitize_narration("語り<function_calls><invoke name=\"emit_delta\">").contains('<'));
        // 正常な narration は無改変。
        assert_eq!(parse::sanitize_narration("普通の語り。"), "普通の語り。");
    }

    /// 【症状と修正の分離】tool_call は valid JSON なので extract はタグ混入を素通りさせる
    /// (= 症状)。sanitize で初めて掃除される。ops は別フィールドで無改変。
    #[test]
    fn extract_passes_leaked_tags_sanitize_cleans_them() {
        let args = r#"{"narration":"語り。</narration><parameter name=\"ops\">[]","ops":[{"op":"set_flag","key":"f","value":true}]}"#;
        let resp = response_with_tool_args(args);
        let delta: StateDelta = parse::extract(resp.first_message().unwrap()).unwrap();
        assert!(delta.narration.contains("</narration>"), "extract 単体ではタグが残る (症状)");
        assert_eq!(delta.ops.len(), 1, "ops は valid な別フィールドで正常");
        assert_eq!(parse::sanitize_narration(&delta.narration), "語り。", "sanitize で掃除");
    }

    /// 【空応答】tool_calls も content も無ければ NoStructuredOutput。
    #[test]
    fn empty_message_is_no_structured_output() {
        let resp = ChatResponse {
            usage: None,
            choices: vec![Choice {
                message: ResponseMessage { content: None, tool_calls: vec![] },
            }],
        };
        let err = parse::extract::<StateDelta>(resp.first_message().unwrap()).unwrap_err();
        assert!(matches!(err, LlmError::NoStructuredOutput));
    }

    /// 【フェンス除去】```json / ``` の両形を剥がす。
    #[test]
    fn strips_code_fences() {
        assert_eq!(parse::strip_code_fence("```json\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(parse::strip_code_fence("```\n{\"a\":1}\n```"), "{\"a\":1}");
        assert_eq!(parse::strip_code_fence("{\"a\":1}"), "{\"a\":1}");
    }

    /// 【config】環境変数から構築。api_key 欠落は Config エラー。
    #[test]
    fn config_requires_api_key() {
        // 既存環境を汚さないよう、このテスト内だけで設定/復元。
        std::env::remove_var("LLM_API_KEY");
        let err = LlmConfig::from_env().unwrap_err();
        assert!(matches!(err, LlmError::Config(_)), "api_key 欠落は Config エラー");

        let cfg = LlmConfig::new("https://api.example.com/v1/", "sk-test", "gpt-4o-mini");
        assert_eq!(cfg.chat_endpoint(), "https://api.example.com/v1/chat/completions");
    }

    /// 【一過性判定】5xx/429 はリトライ対象、4xx (除 429) はしない。
    #[test]
    fn transient_classification() {
        assert!(LlmError::Api { status: 503, body: String::new() }.is_transient());
        assert!(LlmError::Api { status: 429, body: String::new() }.is_transient());
        assert!(!LlmError::Api { status: 400, body: String::new() }.is_transient());
        assert!(!LlmError::NoStructuredOutput.is_transient());
    }

    // --- Anthropic ネイティブ経路 (prompt caching, #44) --------------------------

    /// 【provider 自動判定】api.anthropic.com はネイティブ Messages API (キャッシュの効く経路)、
    /// それ以外は従来の OpenAI 互換。LLM_PROVIDER 明示が無ければ base_url から決まる。
    #[test]
    fn provider_autodetects_anthropic_from_base_url() {
        let native = LlmConfig::new("https://api.anthropic.com/v1", "sk", "claude-opus-4-8");
        assert_eq!(native.provider, Provider::Anthropic);
        assert_eq!(native.messages_endpoint(), "https://api.anthropic.com/v1/messages");

        let compat = LlmConfig::new("https://api.example.com/v1/", "sk", "gpt-4o-mini");
        assert_eq!(compat.provider, Provider::OpenAiCompat);
    }

    /// 【cache_control の配置】ネイティブリクエストは安定プレフィックス (tools→system) の
    /// **末尾 = 最後の system ブロック**にだけ ephemeral breakpoint を置く。可変な user
    /// メッセージには置かない (毎ターン別内容 → 読まれないキャッシュ書込 1.25× の無駄)。
    #[test]
    fn anthropic_request_places_cache_control_on_system_tail() {
        let req = anthropic::build_request(
            "claude-opus-4-8",
            4096,
            None,
            user_msgs(),
            Some((EMIT_DELTA_TOOL.into(), "d".into(), state_delta_schema())),
        );
        let body = serde_json::to_value(&req).unwrap();

        // system は配列で、末尾ブロックに cache_control: ephemeral。
        let system = body["system"].as_array().expect("system はブロック配列");
        assert_eq!(system.last().unwrap()["cache_control"]["type"], "ephemeral");
        // messages 側には cache_control を置かない (どのブロックにも無い)。
        let msgs = serde_json::to_string(&body["messages"]).unwrap();
        assert!(!msgs.contains("cache_control"), "可変部に breakpoint を置かない: {msgs}");
        // system は messages に混ざらない (ネイティブは先頭 system 専用フィールド)。
        assert!(!msgs.contains("system"), "system ロールは messages に出さない");
        // tools はネイティブ形 (input_schema)、tool_choice は {type:tool, name} で強制。
        assert_eq!(body["tools"][0]["name"], EMIT_DELTA_TOOL);
        assert!(body["tools"][0]["input_schema"].is_object(), "schema は input_schema キー");
        assert_eq!(body["tool_choice"]["type"], "tool");
        assert_eq!(body["tool_choice"]["name"], EMIT_DELTA_TOOL);
        // temperature 未設定なら送らない (claude-opus-4-8 は temperature 非対応)。
        assert!(body.get("temperature").is_none());
        assert_eq!(body["max_tokens"], 4096);
    }

    /// 【ネイティブ応答の解決】tool_use ブロックの input (JSON オブジェクト) を OpenAI 形へ
    /// 写して既存 parse::extract に合流 → StateDelta。usage のキャッシュ計数もパースされる
    /// (検証は usage.cache_read_input_tokens > 0 = 本修正の Green 判定)。
    #[test]
    fn anthropic_response_resolves_tool_use_and_usage() {
        let body = r#"{
            "content": [
                {"type": "text", "text": "考え中"},
                {"type": "tool_use", "id": "tu_1", "name": "emit_delta",
                 "input": {"narration": "扉が軋む", "ops": [{"op":"set_flag","key":"door_open","value":true}]}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 321, "output_tokens": 55,
                      "cache_creation_input_tokens": 0, "cache_read_input_tokens": 8021}
        }"#;
        let resp = client::decode_messages_body(body.to_string()).unwrap();
        let usage = resp.usage.clone().expect("usage をパースする");
        assert_eq!(usage.cache_read_input_tokens, 8021, "キャッシュ読取が計上される");
        assert_eq!(usage.input_tokens, 321);

        let message = resp.into_response_message();
        let delta: StateDelta = parse::extract(&message).unwrap();
        assert_eq!(delta.narration, "扉が軋む");
        assert_eq!(delta.ops.len(), 1);
    }

    /// 【壊れた応答は raw を保持 (#34 同型)】2xx なのに形が合わない Messages 応答も
    /// 本文を保持した Parse エラーになる (真因診断 + 再生成の燃料)。
    #[test]
    fn anthropic_decode_keeps_raw_on_shape_mismatch() {
        let err = client::decode_messages_body(r#"{"content": "not-an-array"}"#.into()).unwrap_err();
        match err {
            LlmError::Parse { raw, .. } => assert!(raw.contains("not-an-array")),
            other => panic!("Parse エラーであるべき: {other:?}"),
        }
    }

    // --- OpenAI 互換経路のキャッシュ計測 + xAI sticky routing (#45) ------------------

    /// 【互換 usage のパース】OpenAI/xAI/Gemini 互換の `usage.prompt_tokens_details.cached_tokens`
    /// を読める (xAI の Green 判定 = cached > 0)。usage が無い/形が違うサーバでも壊れない。
    #[test]
    fn compat_usage_cached_tokens_parse() {
        let body = r#"{
            "choices": [{"message": {"content": "了解"}}],
            "usage": {"prompt_tokens": 9000, "completion_tokens": 120,
                      "prompt_tokens_details": {"cached_tokens": 8100}}
        }"#;
        let resp = client::decode_chat_body(body.to_string()).unwrap();
        let usage = resp.usage.expect("usage をパースする");
        assert_eq!(usage.prompt_tokens_details.unwrap().cached_tokens, 8100);
        assert_eq!(usage.prompt_tokens, 9000);

        // usage 無し (ローカル互換サーバ等) でも従来どおり decode できる。
        let plain = client::decode_chat_body(
            r#"{"choices":[{"message":{"content":"ok"}}]}"#.to_string(),
        )
        .unwrap();
        assert!(plain.usage.is_none());
        assert_eq!(plain.first_message().unwrap().content.as_deref(), Some("ok"));
    }

    /// 【会話 ID】クライアント毎に一意な conv_id を持つ (xAI のキャッシュはサーバ単位 →
    /// x-grok-conv-id で同一サーバに sticky routing しないと同一プレフィックスでも miss)。
    #[test]
    fn conv_id_is_unique_per_client_and_stable_within() {
        let cfg = LlmConfig::new("https://api.x.ai/v1", "sk", "grok-4");
        let a = LlmClient::new(cfg.clone()).unwrap();
        let b = LlmClient::new(cfg).unwrap();
        assert!(!a.conv_id().is_empty());
        assert_ne!(a.conv_id(), b.conv_id(), "クライアント (=セッション) 毎に別 ID");
        assert_eq!(a.conv_id(), a.conv_id(), "同一クライアント内では不変");
    }

    /// 【stray system の降格】ネイティブは先頭 system のみ対応 → 先頭以外の system
    /// (no-tools の json_instruction 等が万一混じった場合) は user に落として壊さない。
    #[test]
    fn anthropic_demotes_non_leading_system_to_user() {
        let msgs = vec![
            ChatMessage::system("GM人格"),
            ChatMessage::system("盤面"),
            ChatMessage::user("行動"),
            ChatMessage::system("後付け指示"),
        ];
        let req = anthropic::build_request("m", 256, None, msgs, None);
        let body = serde_json::to_value(&req).unwrap();
        assert_eq!(body["system"].as_array().unwrap().len(), 2, "先頭連続 system は system ブロックへ");
        let roles: Vec<&str> = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["role"].as_str().unwrap())
            .collect();
        assert_eq!(roles, vec!["user", "user"], "先頭以外の system は user に降格");
    }
}

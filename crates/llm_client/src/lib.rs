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

mod client;
mod config;
mod error;
mod parse;
mod wire;

pub use client::LlmClient;
pub use config::LlmConfig;
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
pub fn state_delta_schema() -> serde_json::Value {
    let schema = schemars::schema_for!(StateDelta);
    serde_json::to_value(schema).expect("schemars 生成スキーマは必ず JSON 化できる")
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
}

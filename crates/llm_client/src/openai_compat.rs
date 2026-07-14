//! OpenAI 互換 adapter (GPT / Grok / さくら / ローカル互換サーバ) — spec 12 Phase A。
//!
//! canonical ⇄ wire の **encode/decode 純関数**。HTTP・リトライ・認証・sticky ヘッダ
//! (`x-grok-conv-id` #45) は client 核が担い、ここは形の翻訳だけを持つ
//! (壊れるのは ser/de なので PoC で固める)。

use crate::canonical::{ChatRequest, ChatResponse, Finish, ToolCall, ToolChoice, Usage};
use crate::error::LlmError;
use crate::wire;

/// canonical → OpenAI 互換 wire。
///
/// `use_tools=false` (tool_choice を実装しないサーバ #29 — さくら AI Engine / ローカル互換) は
/// tools を送らず、schema を載せた [`json_instruction`] を **messages 末尾の system** として積む
/// (従来 `generate_structured` にあった no-tools 分岐の移設。K4)。
pub(crate) fn encode(req: &ChatRequest, use_tools: bool) -> wire::ChatRequest {
    let mut messages = req.messages.clone();
    let (tools, tool_choice) = if req.tools.is_empty() {
        (Vec::new(), None)
    } else if use_tools {
        let tools = req
            .tools
            .iter()
            .map(|t| wire::Tool {
                kind: wire::ToolKind::Function,
                function: wire::FunctionDef {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect();
        // v1 の利用は Specific (単一ツール強制) のみ。Auto/Required は送らない (= サーバ既定)。
        let choice = match &req.tool_choice {
            ToolChoice::Specific(name) => Some(wire::ToolChoice::force(name.clone())),
            _ => None,
        };
        (tools, choice)
    } else {
        messages.push(wire::ChatMessage::system(json_instruction(
            &req.tools[0].parameters,
        )));
        (Vec::new(), None)
    };
    wire::ChatRequest {
        model: req.model.clone(),
        messages,
        temperature: req.temperature,
        max_tokens: req.max_tokens,
        tools,
        tool_choice,
    }
}

/// OpenAI 互換 wire → canonical。
///
/// tool_calls の arguments (**JSON 文字列**) はここで **1 回だけ** parse して以後は
/// オブジェクトとして運ぶ (写経元 D2 — 二重エンコード/未パースの取り違えを境界で殺す)。
/// 壊れた arguments は **raw を保持した** Parse エラー (#34 同型・再生成の燃料)。
pub(crate) fn decode(resp: wire::ChatResponse) -> Result<ChatResponse, LlmError> {
    let usage = resp
        .usage
        .as_ref()
        .map(|u| Usage {
            prompt: u.prompt_tokens,
            completion: u.completion_tokens,
            cache_read: u
                .prompt_tokens_details
                .as_ref()
                .map(|d| d.cached_tokens)
                .unwrap_or(0),
        })
        .unwrap_or_default();

    let Some(choice) = resp.choices.into_iter().next() else {
        return Ok(ChatResponse { text: None, tool_calls: Vec::new(), finish: Finish::Other, usage });
    };

    let finish = match choice.finish_reason.as_deref() {
        Some("stop") => Finish::Stop,
        Some("tool_calls") => Finish::ToolUse,
        Some("length") => Finish::Length,
        _ => Finish::Other,
    };

    let mut tool_calls = Vec::new();
    for call in choice.message.tool_calls {
        let raw = call.function.arguments;
        let args = serde_json::from_str(&raw)
            .map_err(|source| LlmError::Parse { source, raw: raw.clone() })?;
        tool_calls.push(ToolCall {
            id: call.id.unwrap_or_default(),
            name: call.function.name.unwrap_or_default(),
            args,
        });
    }

    Ok(ChatResponse { text: choice.message.content, tool_calls, finish, usage })
}

/// no-tools モードで「schema に従う JSON だけを出力せよ」と指示する system メッセージ本文。
/// tool_choice 非対応サーバ (さくら AI Engine / ローカル OpenAI 互換) 向け。schema は単一真実源。
pub(crate) fn json_instruction(schema: &serde_json::Value) -> String {
    format!(
        "重要: このサーバはツール呼び出し (function calling) に対応していません。\
        応答は次の JSON Schema に厳密に従う JSON オブジェクトを **1つだけ** 出力し、\
        前置き・説明・コードフェンスのラベル等、余計なテキストを一切含めないでください。\n\
        JSON Schema:\n{}",
        serde_json::to_string(schema).unwrap_or_default()
    )
}

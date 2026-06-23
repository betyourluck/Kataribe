//! HTTP クライアント本体。OpenAI 互換 chat/completions を叩き、指数 backoff でリトライする。
//!
//! `AsyncOpenAI` (Python) の Rust 版。ネットワーク経路は実キーが要るため単体テスト対象外
//! (壊れる ser/de は wire.rs / parse.rs 側で固める)。実 API 通しは「実クラウド通しプレイ」フェーズ。

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::config::LlmConfig;
use crate::error::LlmError;
use crate::parse;
use crate::wire::{
    ChatMessage, ChatRequest, ChatResponse, FunctionDef, Tool, ToolChoice, ToolKind,
};

/// クラウド LLM ナレーター脚。
pub struct LlmClient {
    http: reqwest::Client,
    config: LlmConfig,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Result<Self, LlmError> {
        let http = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()?;
        Ok(Self { http, config })
    }

    pub fn config(&self) -> &LlmConfig {
        &self.config
    }

    /// プレーンなテキスト生成 (`generate`)。ツール無し。
    pub async fn generate(&self, messages: Vec<ChatMessage>) -> Result<String, LlmError> {
        let req = ChatRequest {
            model: self.config.model.clone(),
            messages,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            tools: Vec::new(),
            tool_choice: None,
        };
        let resp = self.chat_with_retry(&req).await?;
        resp.first_message()
            .and_then(|m| m.content.clone())
            .filter(|c| !c.trim().is_empty())
            .ok_or(LlmError::EmptyResponse)
    }

    /// 構造化出力 (`generate_json` の Rust 版)。
    ///
    /// 単一ツール `tool_name` を `tool_choice` で強制し、`parameters` schema に沿わせる。
    /// 応答は tool_calls もしくはフェンス JSON から `T` に解決する。
    pub async fn generate_structured<T: DeserializeOwned>(
        &self,
        messages: Vec<ChatMessage>,
        tool_name: &str,
        tool_description: &str,
        parameters: serde_json::Value,
    ) -> Result<T, LlmError> {
        let tool = Tool {
            kind: ToolKind::Function,
            function: FunctionDef {
                name: tool_name.to_string(),
                description: tool_description.to_string(),
                parameters,
            },
        };
        let req = ChatRequest {
            model: self.config.model.clone(),
            // temperature は config 任せ (未設定なら送らない)。tool_choice 強制が構造を保証する。
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
            messages,
            tools: vec![tool],
            tool_choice: Some(ToolChoice::force(tool_name)),
        };
        let resp = self.chat_with_retry(&req).await?;
        let message = resp.first_message().ok_or(LlmError::EmptyResponse)?;
        parse::extract::<T>(message)
    }

    /// chat/completions を 1 回叩く (リトライ無し)。
    async fn chat_once(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let resp = self
            .http
            .post(self.config.chat_endpoint())
            .bearer_auth(&self.config.api_key)
            .json(req)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                body,
            });
        }
        Ok(resp.json::<ChatResponse>().await?)
    }

    /// 指数 backoff 付きで chat を叩く。一過性エラーのみリトライ (tenacity 同型)。
    async fn chat_with_retry(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.chat_once(req).await {
                Ok(resp) => return Ok(resp),
                Err(e) => {
                    if attempt >= self.config.max_retries || !e.is_transient() {
                        return Err(e);
                    }
                    // 1s, 2s, 4s ... 上限 10s (wait_exponential(min=1, max=10) 同型)。
                    let secs = (1u64 << (attempt - 1)).min(10);
                    tokio::time::sleep(Duration::from_secs(secs)).await;
                }
            }
        }
    }
}

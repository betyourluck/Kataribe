//! 環境変数からの設定ロード。LocalAI `config.py::LLMSettings` の Rust 版。
//!
//! クラウド LLM キーは `.env` (gitignore 済) に置く。`.env.example` を雛形にする。

use std::time::Duration;

use crate::error::LlmError;

/// LLM 接続設定。`base_url` + `api_key` 抽象でベンダーロックインを避ける。
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// 例: `https://api.openai.com/v1` / `https://api.anthropic.com/v1` / ローカル互換サーバ。
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 明示設定時のみ送る (`None` なら provider 既定)。新しめのモデルは temperature 非対応。
    pub temperature: Option<f32>,
    pub max_tokens: u32,
    pub request_timeout: Duration,
    /// chat 呼び出しの最大試行回数 (指数 backoff)。tenacity `stop_after_attempt` 同型。
    pub max_retries: u32,
}

impl LlmConfig {
    /// 環境変数から構築する。`LLM_BASE_URL` / `LLM_API_KEY` / `LLM_MODEL` は必須でないが、
    /// `api_key` が空の場合のみ `Config` エラーにする (キー無しでは API を叩けないため)。
    ///
    /// プロセス env を読むだけ (副作用なし・テスト可能)。`.env` の読み込みは呼び出し側
    /// (アプリ入口) の責務 — bin で `dotenvy::dotenv().ok()` を先に呼ぶ。
    ///
    /// 既定値:
    /// - `LLM_BASE_URL` = `https://api.openai.com/v1`
    /// - `LLM_MODEL`    = `gpt-4o-mini`
    /// - `LLM_TEMPERATURE` = **未設定なら送らない** (provider 既定に委ねる)
    /// - `LLM_MAX_TOKENS` = `4096`
    /// - `LLM_REQUEST_TIMEOUT_SECS` = `120`, `LLM_MAX_RETRIES` = `3`
    pub fn from_env() -> Result<Self, LlmError> {
        let api_key = env_opt("LLM_API_KEY").unwrap_or_default();
        if api_key.trim().is_empty() {
            return Err(LlmError::Config(
                "LLM_API_KEY が未設定です (.env に設定してください)".into(),
            ));
        }

        // temperature は明示設定時のみ Some。新しめのモデルは送ると 400 になるため。
        let temperature = match env_opt("LLM_TEMPERATURE") {
            None => None,
            Some(raw) => Some(raw.parse::<f32>().map_err(|_| {
                LlmError::Config(format!("環境変数 LLM_TEMPERATURE の値 '{raw}' を解釈できません"))
            })?),
        };

        Ok(Self {
            base_url: env_opt("LLM_BASE_URL")
                .unwrap_or_else(|| "https://api.openai.com/v1".into()),
            api_key,
            model: env_opt("LLM_MODEL").unwrap_or_else(|| "gpt-4o-mini".into()),
            temperature,
            max_tokens: env_parse("LLM_MAX_TOKENS", 4096)?,
            request_timeout: Duration::from_secs(env_parse("LLM_REQUEST_TIMEOUT_SECS", 120)?),
            max_retries: env_parse("LLM_MAX_RETRIES", 3)?,
        })
    }

    /// テスト・明示構築用。`base_url` と `api_key` を与えて残りは既定値。
    pub fn new(base_url: impl Into<String>, api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            temperature: None,
            max_tokens: 4096,
            request_timeout: Duration::from_secs(120),
            max_retries: 3,
        }
    }

    /// `{base_url}/chat/completions` を組み立てる (末尾スラッシュを正規化)。
    pub fn chat_endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn env_parse<T>(key: &str, default: T) -> Result<T, LlmError>
where
    T: std::str::FromStr,
{
    match env_opt(key) {
        None => Ok(default),
        Some(raw) => raw
            .parse::<T>()
            .map_err(|_| LlmError::Config(format!("環境変数 {key} の値 '{raw}' を解釈できません"))),
    }
}

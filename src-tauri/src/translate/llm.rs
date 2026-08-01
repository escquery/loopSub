//! OpenAI 兼容 chat completions 客户端（DeepSeek / 通义 / 本地 vLLM 均可接入）

use serde_json::json;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LlmError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api error: {0}")]
    Api(String),
}

/// 聊天能力抽象：编排器依赖此 trait，测试可注入 mock
pub trait Chat: Send + Sync {
    fn chat(
        &self,
        system: &str,
        user: &str,
        temperature: f32,
    ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send;
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
}

impl LlmClient {
    pub fn new(base_url: &str, model: &str, api_key: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(300))
                .build()
                .unwrap_or_default(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            api_key: api_key.to_string(),
        }
    }
}

impl Chat for LlmClient {
    async fn chat(&self, system: &str, user: &str, temperature: f32) -> Result<String, LlmError> {
        let resp = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&json!({
                "model": self.model,
                "messages": [
                    { "role": "system", "content": system },
                    { "role": "user", "content": user },
                ],
                "temperature": temperature,
            }))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let cut = body.len().min(300);
            return Err(LlmError::Api(format!("{status}: {}", &body[..cut])));
        }
        let v: serde_json::Value = resp.json().await?;
        v.pointer("/choices/0/message/content")
            .and_then(|c| c.as_str())
            .map(String::from)
            .ok_or_else(|| LlmError::Api("响应缺少 choices[0].message.content".into()))
    }
}

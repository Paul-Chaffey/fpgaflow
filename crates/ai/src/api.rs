//! Anthropic Messages API backend (raw HTTPS; Rust has no official SDK).
//!
//! Request shape, per the Claude API reference for this API version:
//! `POST /v1/messages`, `anthropic-version: 2026-10-21`, streaming,
//! adaptive thinking with summarised display, `output_config.effort` and a
//! JSON-schema `output_config.format`, and server-side refusal fallbacks
//! (`fallbacks: "default"`, beta `server-side-fallback-2026-07-01`).

use std::time::Duration;

use fpgaflow_core::AiConfig;
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::sse::{Assembler, Delta, SseParser};
use crate::{output_schema, parse_answer, AiError, AiEvent, Generated};

const URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Streaming, so a large output budget doesn't risk HTTP timeouts.
const MAX_TOKENS: u32 = 64_000;
/// Upper bound for one request; long design tasks can take minutes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// `ANTHROPIC_API_KEY`, else the output of `[ai] api_key_command`.
pub fn api_key(cfg: &AiConfig) -> Result<String, AiError> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.trim().is_empty() {
            return Ok(k.trim().to_string());
        }
    }
    let cmd = cfg.api_key_command.as_deref().ok_or(AiError::NoApiKey)?;
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", cmd])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| AiError::Other(anyhow::anyhow!("api_key_command: {e}")))?;
    let key = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !key.is_empty() {
        Ok(key)
    } else {
        Err(AiError::NoApiKey)
    }
}

/// The JSON request body (separate so it can be tested).
pub fn request_body(cfg: &AiConfig, system: &str, prompt: &str) -> serde_json::Value {
    json!({
        "model": cfg.model,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "system": system,
        "messages": [{"role": "user", "content": prompt}],
        "thinking": {"type": "adaptive", "display": "summarized"},
        "output_config": {
            "effort": cfg.effort,
            "format": {"type": "json_schema", "schema": output_schema()}
        },
        "fallbacks": "default"
    })
}

pub async fn ask(
    cfg: &AiConfig,
    system: &str,
    prompt: &str,
    events: &UnboundedSender<AiEvent>,
    cancel: &CancellationToken,
) -> Result<Generated, AiError> {
    let key = api_key(cfg)?;
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| AiError::Other(e.into()))?;
    let body = serde_json::to_vec(&request_body(cfg, system, prompt))
        .map_err(|e| AiError::Other(e.into()))?;
    let send = client
        .post(URL)
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .header("anthropic-beta", FALLBACK_BETA)
        .header("content-type", "application/json")
        .body(body)
        .send();
    let mut resp = tokio::select! {
        r = send => r.map_err(|e| AiError::Other(e.into()))?,
        _ = cancel.cancelled() => return Err(AiError::Cancelled),
    };
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or(text);
        return Err(AiError::Api {
            status: status.as_u16(),
            message,
        });
    }

    let mut parser = SseParser::default();
    let mut asm = Assembler::default();
    loop {
        let chunk = tokio::select! {
            c = resp.chunk() => c.map_err(|e| AiError::Other(e.into()))?,
            _ = cancel.cancelled() => return Err(AiError::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        for (ev, data) in parser.push(&chunk) {
            match asm.apply(&ev, &data) {
                Some(Delta::Text(t)) => {
                    let _ = events.send(AiEvent::Text(t));
                }
                Some(Delta::Thinking(t)) => {
                    let _ = events.send(AiEvent::Thinking(t));
                }
                Some(Delta::Restarted) => {
                    let _ = events.send(AiEvent::Status("a fallback model took over".into()));
                }
                None => {}
            }
        }
        if asm.done {
            break;
        }
    }
    if let Some(e) = asm.error {
        return Err(AiError::Api {
            status: 200,
            message: e,
        });
    }
    match asm.stop_reason.as_deref() {
        Some("refusal") => {
            let cat = asm
                .stop_details
                .as_ref()
                .and_then(|d| d["category"].as_str())
                .unwrap_or("no category")
                .to_string();
            Err(AiError::Refused(cat))
        }
        Some("max_tokens") => Err(AiError::Truncated),
        _ => parse_answer(&asm.text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_matches_the_documented_shape() {
        let b = request_body(&AiConfig::default(), "sys", "hello");
        assert_eq!(b["model"], "claude-opus-5-5");
        assert_eq!(b["stream"], true);
        assert_eq!(b["thinking"]["type"], "adaptive");
        assert!(
            b.get("temperature").is_none(),
            "sampling params 400 on this API version"
        );
        assert_eq!(b["output_config"]["effort"], "high");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(b["fallbacks"], "default");
        assert_eq!(b["messages"][0]["role"], "user");
    }

    #[test]
    fn key_command_is_used_when_env_is_unset() {
        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            return; // can't test the fallback path in this environment
        }
        let cfg = AiConfig {
            api_key_command: Some("echo test-key".into()),
            ..Default::default()
        };
        assert_eq!(api_key(&cfg).unwrap(), "test-key");
        let none = AiConfig {
            api_key_command: None,
            ..Default::default()
        };
        assert!(matches!(api_key(&none), Err(AiError::NoApiKey)));
    }
}

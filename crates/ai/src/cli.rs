//! Claude Code CLI backend: `claude -p` with a JSON schema, using the
//! user's existing Claude Code login.
//!
//! Flags were taken from `claude --help` (2.1.330). The session is
//! non-persistent and limited to read-only tools, so it can look at the
//! project but never change it; fpgaflow writes only accepted files.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use fpgaflow_core::{tools, AiConfig, UserConfig};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::{output_schema, parse_answer, AiError, AiEvent, Generated};

const TIMEOUT: Duration = Duration::from_secs(20 * 60);
const ALLOWED_TOOLS: &str = "Read Glob Grep";

pub fn args(cfg: &AiConfig, system: &str) -> Vec<String> {
    vec![
        "-p".into(),
        "--output-format".into(),
        "json".into(),
        "--json-schema".into(),
        output_schema().to_string(),
        "--model".into(),
        cfg.model.clone(),
        "--effort".into(),
        cfg.effort.clone(),
        "--no-session-persistence".into(),
        "--permission-mode".into(),
        "dontAsk".into(),
        "--allowedTools".into(),
        ALLOWED_TOOLS.into(),
        "--append-system-prompt".into(),
        system.into(),
    ]
}

/// Pull the structured answer out of `claude -p --output-format json`.
///
/// Prefers a `structured_output` object; otherwise parses the `result` text.
pub fn parse_cli_output(stdout: &str) -> Result<Generated, AiError> {
    let v: Value = serde_json::from_str(stdout.trim())
        .map_err(|e| AiError::Format(format!("claude output is not JSON: {e}")))?;
    if v["is_error"] == true {
        let msg = v["result"].as_str().unwrap_or("unknown error").to_string();
        return Err(AiError::Api {
            status: 0,
            message: msg,
        });
    }
    if v["structured_output"].is_object() {
        return parse_answer(&v["structured_output"].to_string());
    }
    match v["result"].as_str() {
        Some(text) => parse_answer(text),
        None => Err(AiError::Format(
            "no structured_output or result in claude output".into(),
        )),
    }
}

pub async fn ask(
    cfg: &AiConfig,
    project_root: &Path,
    system: &str,
    prompt: &str,
    events: &UnboundedSender<AiEvent>,
    cancel: &CancellationToken,
) -> Result<Generated, AiError> {
    let claude = tools::resolve("claude", &UserConfig::load().unwrap_or_default())
        .ok_or_else(|| AiError::Other(anyhow::anyhow!("the claude CLI is not installed")))?;
    let mut child = tokio::process::Command::new(claude)
        .args(args(cfg, system))
        .current_dir(project_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| AiError::Other(anyhow::anyhow!("cannot start claude: {e}")))?;
    let _ = events.send(AiEvent::Status(
        "Claude Code is working (no streaming in this mode)".into(),
    ));

    // The prompt goes on stdin: project sources can exceed the 128 KiB argv limit.
    let mut stdin = child.stdin.take().expect("piped");
    stdin
        .write_all(prompt.as_bytes())
        .await
        .map_err(|e| AiError::Other(e.into()))?;
    drop(stdin);
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
    let read = async {
        let mut out = String::new();
        let mut err = String::new();
        let _ = tokio::join!(
            stdout.read_to_string(&mut out),
            stderr.read_to_string(&mut err)
        );
        let status = child.wait().await;
        (out, err, status)
    };
    let (out, err, status) = tokio::select! {
        r = read => r,
        _ = cancel.cancelled() => return Err(AiError::Cancelled),
        _ = tokio::time::sleep(TIMEOUT) => return Err(AiError::Other(anyhow::anyhow!("claude timed out"))),
    };
    let status = status.map_err(|e| AiError::Other(e.into()))?;
    if !status.success() && out.trim().is_empty() {
        return Err(AiError::Api {
            status: status.code().unwrap_or(-1) as u16,
            message: err.trim().to_string(),
        });
    }
    parse_cli_output(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_tools_and_schema() {
        let a = args(&AiConfig::default(), "sys");
        let i = a.iter().position(|x| x == "--allowedTools").unwrap();
        assert_eq!(a[i + 1], "Read Glob Grep");
        let j = a.iter().position(|x| x == "--json-schema").unwrap();
        assert!(serde_json::from_str::<Value>(&a[j + 1]).is_ok());
        assert!(!a
            .iter()
            .any(|x| x.contains("Write") || x.contains("Edit") || x.contains("Bash")));
    }

    #[test]
    fn output_shapes() {
        let s = r#"{"type":"result","is_error":false,"result":"done","structured_output":{"files":[],"summary":"ok"}}"#;
        assert_eq!(parse_cli_output(s).unwrap().summary, "ok");
        let r = r#"{"type":"result","is_error":false,"result":"{\"files\":[],\"summary\":\"from text\"}"}"#;
        assert_eq!(parse_cli_output(r).unwrap().summary, "from text");
        let e = r#"{"type":"result","is_error":true,"result":"Not logged in"}"#;
        assert!(matches!(parse_cli_output(e), Err(AiError::Api { .. })));
    }
}

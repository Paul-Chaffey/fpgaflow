//! Server-sent events for the Messages API stream.
//!
//! [`SseParser`] turns raw bytes into `(event, data)` pairs; [`Assembler`]
//! folds the Messages API events into the answer text, the summarised
//! thinking and the stop reason. Both are pure, so they are tested against
//! recorded streams with no network.

use serde_json::Value;

#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    /// Feed bytes; returns every complete `(event name, data)` pair.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<(String, String)> {
        self.buf.push_str(&String::from_utf8_lossy(bytes));
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line: String = self.buf.drain(..=nl).collect();
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() || self.event.is_some() {
                    out.push((
                        self.event.take().unwrap_or_else(|| "message".into()),
                        self.data.join("\n"),
                    ));
                    self.data.clear();
                }
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("data:") {
                self.data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            }
            // ":" comments and other fields are ignored.
        }
        out
    }
}

/// A piece of progress for the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum Delta {
    Text(String),
    Thinking(String),
    /// A fallback model took over; text so far was discarded.
    Restarted,
}

#[derive(Debug, Default)]
pub struct Assembler {
    /// Answer text, from `text` content blocks.
    pub text: String,
    pub stop_reason: Option<String>,
    /// `stop_details` when the stop reason is `refusal`.
    pub stop_details: Option<Value>,
    pub model: Option<String>,
    /// An `error` event's message.
    pub error: Option<String>,
    pub done: bool,
}

impl Assembler {
    /// Apply one SSE event; returns progress to show, if any.
    pub fn apply(&mut self, event: &str, data: &str) -> Option<Delta> {
        let v: Value = serde_json::from_str(data).ok()?;
        match event {
            "message_start" => {
                self.model = v["message"]["model"].as_str().map(String::from);
                None
            }
            "content_block_start" => {
                // A server-side fallback inserts a `fallback` block and the
                // next model starts the answer again.
                if v["content_block"]["type"] == "fallback" {
                    self.text.clear();
                    return Some(Delta::Restarted);
                }
                None
            }
            "content_block_delta" => match v["delta"]["type"].as_str() {
                Some("text_delta") => {
                    let t = v["delta"]["text"].as_str().unwrap_or("");
                    self.text.push_str(t);
                    Some(Delta::Text(t.to_string()))
                }
                Some("thinking_delta") => {
                    let t = v["delta"]["thinking"].as_str().unwrap_or("");
                    (!t.is_empty()).then(|| Delta::Thinking(t.to_string()))
                }
                _ => None,
            },
            "message_delta" => {
                if let Some(r) = v["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(r.to_string());
                }
                if !v["delta"]["stop_details"].is_null() {
                    self.stop_details = Some(v["delta"]["stop_details"].clone());
                }
                None
            }
            "message_stop" => {
                self.done = true;
                None
            }
            "error" => {
                self.error = Some(
                    v["error"]["message"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| data.to_string()),
                );
                None
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream shaped like the documented Messages API events, split at
    /// awkward byte boundaries.
    const STREAM: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"model\":\"claude-opus-5-5\"}}\n\n",
        ": ping comment\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"Plan the FSM.\"}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"{\\\"files\\\":[],\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"text_delta\",\"text\":\"\\\"summary\\\":\\\"ok\\\"}\"}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":12}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    fn run(chunks: &[&[u8]]) -> (Assembler, Vec<Delta>) {
        let mut p = SseParser::default();
        let mut a = Assembler::default();
        let mut d = Vec::new();
        for c in chunks {
            for (ev, data) in p.push(c) {
                d.extend(a.apply(&ev, &data));
            }
        }
        (a, d)
    }

    #[test]
    fn assembles_text_thinking_and_stop_reason() {
        let bytes = STREAM.as_bytes();
        // Split into 7-byte chunks to exercise buffering across boundaries.
        let chunks: Vec<&[u8]> = bytes.chunks(7).collect();
        let (a, d) = run(&chunks);
        assert_eq!(a.text, r#"{"files":[],"summary":"ok"}"#);
        assert_eq!(a.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(a.model.as_deref(), Some("claude-opus-5-5"));
        assert!(a.done);
        assert!(d.contains(&Delta::Thinking("Plan the FSM.".into())));
        assert!(crate::parse_answer(&a.text).is_ok());
    }

    #[test]
    fn refusal_and_error_events() {
        let s = "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"type\":\"refusal\",\"category\":\"cyber\"}}}\n\nevent: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let (a, _) = run(&[s.as_bytes()]);
        assert_eq!(a.stop_reason.as_deref(), Some("refusal"));
        assert_eq!(a.stop_details.as_ref().unwrap()["category"], "cyber");
        assert_eq!(a.error.as_deref(), Some("Overloaded"));
    }

    #[test]
    fn fallback_block_restarts_text() {
        let s = concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"fallback\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":2,\"delta\":{\"type\":\"text_delta\",\"text\":\"fresh\"}}\n\n",
        );
        let (a, d) = run(&[s.as_bytes()]);
        assert_eq!(a.text, "fresh");
        assert!(d.contains(&Delta::Restarted));
    }
}

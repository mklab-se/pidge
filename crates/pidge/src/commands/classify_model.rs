//! The AI seam for classification. `LabelModel` abstracts the chat call so
//! batch/dispatch logic is testable with a fake. `AilloyModel` is the
//! production implementation. `build_input` assembles the text shown to the
//! model from a message (pure, unit-tested).

use anyhow::Result;

/// Maximum characters of body text fed to the model (token budget guard).
const MAX_BODY_CHARS: usize = 4000;

#[allow(async_fn_in_trait)]
pub trait LabelModel {
    /// Return the model's raw text response for `prompt` applied to `input`.
    async fn classify(&self, prompt: &str, input: &str) -> Result<String>;
}

/// Build the text block describing a message for the model: subject, sender,
/// and a length-capped plain-text body.
pub fn build_input(subject: &str, from: &str, body_text: &str) -> String {
    let body: String = body_text.chars().take(MAX_BODY_CHARS).collect();
    format!("Subject: {subject}\nFrom: {from}\n\n{body}")
}

/// Assemble the final user message sent to the model: the user's prompt,
/// then the message block.
pub fn assemble_user_message(prompt: &str, input: &str) -> String {
    format!("{prompt}\n\n---\n{input}")
}

/// Production `LabelModel` backed by the user's configured ailloy chat node.
pub struct AilloyModel {
    client: ailloy::Client,
}

impl AilloyModel {
    /// Build from the configured `chat` capability. Errors if AI isn't set up.
    pub fn new() -> Result<Self> {
        let client = ailloy::Client::for_capability("chat")
            .map_err(|e| anyhow::anyhow!("AI not configured ({e}). Run `pidge ai config`."))?;
        Ok(Self { client })
    }
}

impl LabelModel for AilloyModel {
    async fn classify(&self, prompt: &str, input: &str) -> Result<String> {
        let msg = assemble_user_message(prompt, input);
        let resp = self
            .client
            .chat(&[ailloy::types::Message::user(&msg)])
            .await
            .map_err(|e| anyhow::anyhow!("AI classify failed: {e}"))?;
        Ok(resp.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_input_caps_body_length() {
        let body = "x".repeat(MAX_BODY_CHARS + 500);
        let out = build_input("Sub", "a@b.c", &body);
        assert!(out.contains("Subject: Sub"));
        assert!(out.contains("From: a@b.c"));
        let body_part = out.split("\n\n").nth(1).unwrap();
        assert_eq!(body_part.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn assemble_user_message_includes_prompt_and_input() {
        let m = assemble_user_message("Classify it", "Subject: Hi");
        assert!(m.starts_with("Classify it"));
        assert!(m.contains("Subject: Hi"));
    }

    struct FakeModel(&'static str);
    impl LabelModel for FakeModel {
        async fn classify(&self, _p: &str, _i: &str) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[tokio::test]
    async fn fake_model_returns_canned_content() {
        let m = FakeModel("receipt, ticket");
        assert_eq!(m.classify("p", "i").await.unwrap(), "receipt, ticket");
    }

    #[allow(dead_code, unused_imports)]
    fn _assert_fullmessage_type(_m: &pidge_core::FullMessage) {}
}

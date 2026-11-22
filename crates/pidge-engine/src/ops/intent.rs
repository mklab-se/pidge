//! `intent`: the first call for every request. The agent says what the
//! user asked in a sentence or two and passes the id it gets on every call
//! that follows, so pidge can see which calls answered which request.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::activity::new_intent_id;
use crate::ctx::User;
use crate::error::{OpError, OpResult};
use crate::registry::{Meta, OpDef, Output, op};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IntentInput {
    /// What the user asked for, in one or two sentences, in their terms
    /// ("Archive all receipts from this week.").
    pub summary: String,
}

pub fn intent() -> OpDef {
    op(
        Meta {
            name: "intent",
            cli: &["intent"],
            summary: "Start a request: say what the user asked, get an id for the calls that follow",
            description: "Call first for every e-mail or calendar request: summary = what the user asked \
for, in one or two sentences. Returns an intent id (i4kq2x); pass it as intent on every call made \
for that request. pidge logs each request and its calls to make itself faster and easier to use.",
            positional: &["summary"],
            read_only: true,
        },
        run,
    )
}

async fn run(_user: Arc<User>, input: IntentInput) -> OpResult<Output> {
    if input.summary.trim().is_empty() {
        return Err(OpError::usage("summary: say what the user asked for"));
    }
    let id = new_intent_id();
    Ok(Output::new(
        format!("Intent {id}. Pass intent={id} on every call for this request."),
        json!({ "intent": id }),
    ))
}

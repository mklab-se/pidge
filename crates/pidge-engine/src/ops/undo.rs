//! `undo`: take back what was asked for, done or not, and perhaps do
//! something else instead.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;

use crate::ctx::User;
use crate::error::{OpError, OpResult};
use crate::ops::mail_act::{Action, allowed, carry_out, checked, said};
use crate::registry::{Meta, OpDef, Output, op};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UndoInput {
    /// What to take back: the id a change answered with (q7). The latest
    /// when absent.
    pub id: Option<String>,
    /// For mail: do this with the messages instead (`archive`, `flag`,
    /// `move` with `to`, ...).
    pub instead: Option<String>,
    /// With `instead=move`: the folder.
    pub to: Option<String>,
}

pub fn undo() -> OpDef {
    op(
        Meta {
            name: "undo",
            cli: &["undo"],
            summary: "Take back a change to mail or the calendar, or mail on its way",
            description: "Take back something pidge was asked to do: \"no wait\", \"undo that\", \"archive \
instead of deleting\" (instead=archive), \"stop that mail\". id=q7 is the id the change answered \
with; without one, the latest. Works for changes to mail, calendar changes and mail on its way \
out, whether or not they have been carried out. What cannot be taken back (sent mail, a \
cancellation others have seen) says so.",
            positional: &["id"],
            read_only: false,
        },
        run,
    )
}

async fn run(user: Arc<User>, input: UndoInput) -> OpResult<Output> {
    let instead = match input.instead.filter(|a| !a.trim().is_empty()) {
        // Checked first: an undo must not be half of what was asked.
        Some(action) => Some(checked(Action {
            action,
            to: input.to,
            ..Default::default()
        })?),
        None => None,
    };
    if let Some(a) = &instead
        && !allowed(&user, a, 1, true).await?
    {
        return Err(OpError::usage(format!(
            "the user's policy asks before {}: undo first, then mail_act",
            a.action
        )));
    }
    let (mut out, targets) = crate::queue::undo(&user, input.id.as_deref()).await?;
    if let Some(a) = instead {
        let done = carry_out(&user, &a, &targets).await?;
        out.text = format!("{} {}", out.text, said(&a, &done));
        out.data["instead"] = serde_json::json!({
            "action": a.action, "count": done.count, "failed": done.failed, "ids": done.ids,
        });
    }
    Ok(out)
}

//! `pidge trust ...` — manage the trusted-senders list.

use anyhow::Result;

use crate::cli::TrustCommands;

#[allow(dead_code)]
pub async fn run(_command: TrustCommands, _json: bool) -> Result<()> {
    unimplemented!("trust is implemented in a later task")
}

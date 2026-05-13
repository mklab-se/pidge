//! `pidge auth ...` dispatcher.

use anyhow::Result;

use crate::cli::AuthCommands;

pub async fn run(_command: AuthCommands) -> Result<()> {
    unimplemented!("auth dispatcher is implemented in a later task")
}

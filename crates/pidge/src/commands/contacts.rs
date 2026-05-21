//! Top-level dispatch for `pidge contacts <subcommand>`.

use anyhow::Result;

use crate::cli::ContactsCommands;

pub async fn run(command: ContactsCommands, json: bool) -> Result<()> {
    match command {
        ContactsCommands::Refresh { days, account } => {
            crate::commands::contacts_refresh::run(days, account, json).await
        }
        ContactsCommands::Find { query, limit } => {
            crate::commands::contacts_find::run(query, limit, json).await
        }
    }
}

//! `pidge auth ...` dispatcher.

use anyhow::Result;

use crate::cli::AuthCommands;
use crate::commands::{auth_default, auth_list, auth_login, auth_logout, auth_status};

pub async fn run(command: AuthCommands, json: bool) -> Result<()> {
    match command {
        AuthCommands::Login => auth_login::run().await,
        AuthCommands::List => auth_list::run(json),
        AuthCommands::Status => auth_status::run(json),
        AuthCommands::Logout { account, all, yes } => auth_logout::run(account, all, yes),
        AuthCommands::Default { send, calendar } => auth_default::run(send, calendar),
    }
}

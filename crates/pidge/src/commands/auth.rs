//! `pidge auth ...` dispatcher.

use anyhow::Result;

use crate::cli::AuthCommands;
use crate::commands::{
    auth_default, auth_list, auth_login, auth_logout, auth_migrate, auth_status,
};

pub async fn run(command: AuthCommands, json: bool) -> Result<()> {
    match command {
        AuthCommands::Login { store } => auth_login::run(store.into()).await,
        AuthCommands::List => auth_list::run(json),
        AuthCommands::Status => auth_status::run(json),
        AuthCommands::Logout { account, all, yes } => auth_logout::run(account, all, yes),
        AuthCommands::Default { send, calendar } => auth_default::run(send, calendar),
        AuthCommands::MigrateStorage { email, to } => auth_migrate::run(email, to.into()),
    }
}

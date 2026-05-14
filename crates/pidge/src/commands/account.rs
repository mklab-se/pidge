//! `pidge account ...` dispatcher.

use anyhow::Result;

use crate::cli::{AccountCommands, DefaultCommands};
use crate::commands::{
    account_add, account_default, account_list, account_migrate, account_remove,
};

pub async fn run(command: AccountCommands, json: bool) -> Result<()> {
    match command {
        AccountCommands::Add { store } => account_add::run(store.into()).await,
        AccountCommands::List => account_list::run(json),
        AccountCommands::Remove { email, all, yes } => account_remove::run(email, all, yes),
        AccountCommands::Default { command } => match command {
            None => account_default::print_current(json),
            Some(DefaultCommands::EMail { email }) => account_default::set_email(&email),
            Some(DefaultCommands::Calendar { email }) => account_default::set_calendar(&email),
        },
        AccountCommands::MigrateStorage { email, to } => account_migrate::run(email, to.into()),
    }
}

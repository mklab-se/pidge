//! `pidge account ...` dispatcher.

use anyhow::Result;
use pidge_core::provider::ProviderKind;

use crate::cli::{AccountCommands, ProviderArg};
use crate::commands::{account_add, account_list, account_migrate, account_remove};

pub async fn run(command: AccountCommands, json: bool) -> Result<()> {
    match command {
        AccountCommands::Add { provider, store } => {
            let provider = match provider {
                ProviderArg::Microsoft => ProviderKind::Microsoft,
                ProviderArg::Google => ProviderKind::Google,
            };
            account_add::run(provider, store.into()).await
        }
        AccountCommands::List => account_list::run(json),
        AccountCommands::Remove { email, all, yes } => account_remove::run(email, all, yes),
        AccountCommands::MigrateStorage { email, to } => account_migrate::run(email, to.into()),
    }
}

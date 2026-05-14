//! `pidge account remove` — sign out and delete an account's tokens.

use anyhow::{Result, anyhow};
use colored::Colorize;
use inquire::{Confirm, Select};

use pidge_client::auth::TokenStore;
use pidge_core::{Config, TokenStorage};

pub fn run(email: Option<String>, all: bool, yes: bool) -> Result<()> {
    let mut config = Config::load()?;

    if config.accounts.is_empty() {
        println!("No accounts signed in.");
        return Ok(());
    }

    if all {
        if !yes {
            let confirmed =
                Confirm::new(&format!("Remove all {} accounts?", config.accounts.len()))
                    .with_default(false)
                    .prompt()?;
            if !confirmed {
                println!("Aborted.");
                return Ok(());
            }
        }
        let entries: Vec<(String, TokenStorage)> = config
            .accounts
            .iter()
            .map(|a| (a.email.clone(), a.storage))
            .collect();
        for (e, storage) in &entries {
            TokenStore::delete(e, *storage)?;
            config.remove_account(e);
        }
        config.save()?;
        println!("{} Removed {} accounts.", "✔".green(), entries.len());
        return Ok(());
    }

    // Resolve which email to remove. With one account: that one. With multiple
    // and no positional argument: interactive picker.
    let email = match email {
        Some(e) => e,
        None => {
            if config.accounts.len() == 1 {
                config.accounts[0].email.clone()
            } else {
                let options: Vec<String> =
                    config.accounts.iter().map(|a| a.email.clone()).collect();
                Select::new("Which account to remove?", options).prompt()?
            }
        }
    };

    let storage = config
        .find(&email)
        .map(|a| a.storage)
        .ok_or_else(|| anyhow!("not signed in to {email}"))?;

    if !yes {
        let confirmed = Confirm::new(&format!("Remove {email}?"))
            .with_default(false)
            .prompt()?;
        if !confirmed {
            println!("Aborted.");
            return Ok(());
        }
    }

    TokenStore::delete(&email, storage)?;
    config.remove_account(&email);
    config.save()?;
    println!("{} Removed {email}.", "✔".green());
    Ok(())
}

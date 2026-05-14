//! `pidge auth logout` — remove tokens and account entry from pidge.

use anyhow::{Result, anyhow};
use colored::Colorize;
use inquire::{Confirm, Select};

use pidge_client::auth::TokenStore;
use pidge_core::{Config, TokenStorage};

pub fn run(account: Option<String>, all: bool, yes: bool) -> Result<()> {
    let mut config = Config::load()?;

    if config.accounts.is_empty() {
        println!("No accounts signed in.");
        return Ok(());
    }

    if all {
        if !yes {
            let confirmed = Confirm::new(&format!(
                "Sign out of all {} accounts?",
                config.accounts.len()
            ))
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
        for (email, storage) in &entries {
            TokenStore::delete(email, *storage)?;
            config.remove_account(email);
        }
        config.save()?;
        println!("{} Signed out of {} accounts.", "✔".green(), entries.len());
        return Ok(());
    }

    // Resolve which email to log out
    let email = match account {
        Some(e) => e,
        None => {
            if config.accounts.len() == 1 {
                config.accounts[0].email.clone()
            } else {
                let options: Vec<String> =
                    config.accounts.iter().map(|a| a.email.clone()).collect();
                Select::new("Which account to sign out?", options).prompt()?
            }
        }
    };

    let storage = config
        .find(&email)
        .map(|a| a.storage)
        .ok_or_else(|| anyhow!("not signed in to {email}"))?;

    if !yes {
        let confirmed = Confirm::new(&format!("Sign out of {email}?"))
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
    println!("{} Signed out of {email}.", "✔".green());
    Ok(())
}

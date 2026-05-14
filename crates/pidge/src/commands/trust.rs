//! `pidge trust ...` — manage the trusted-senders list.

use anyhow::Result;
use colored::Colorize;

use pidge_core::Config;

use crate::cli::TrustCommands;

pub async fn run(command: TrustCommands, json: bool) -> Result<()> {
    let mut config = Config::load()?;
    match command {
        TrustCommands::List => render_list(&config, json),
        TrustCommands::Add { email } => {
            config.add_trusted_sender(&email);
            config.save()?;
            if !json {
                println!("{} Added {email} to trust list.", "✔".green());
            }
            Ok(())
        }
        TrustCommands::Remove { email } => {
            let removed = config.remove_trusted_sender(&email);
            config.save()?;
            if !json {
                if removed {
                    println!("{} Removed {email} from trust list.", "✔".green());
                } else {
                    println!("{email} was not in the trust list.");
                }
            }
            Ok(())
        }
    }
}

fn render_list(config: &Config, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&config.trusted_senders)?);
        return Ok(());
    }
    if config.trusted_senders.is_empty() {
        println!("No trusted senders. Use `pidge trust add <email>` to add one.");
        return Ok(());
    }
    for s in &config.trusted_senders {
        println!("{s}");
    }
    Ok(())
}

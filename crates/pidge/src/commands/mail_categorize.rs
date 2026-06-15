//! `pidge categorize` — manage native Outlook categories on a message.

use anyhow::Result;
use colored::Colorize;

use pidge_client::{AuthClient, GraphClient};

use crate::cli::CategorizeCommands;
use crate::commands::mail_fragment::{purge_from_cache, resolve};

pub async fn run(command: CategorizeCommands) -> Result<()> {
    match command {
        CategorizeCommands::Show { fragment } => show(fragment).await,
        CategorizeCommands::Set { fragment, labels } => set(fragment, labels).await,
        CategorizeCommands::Add { fragment, labels } => add(fragment, labels).await,
        CategorizeCommands::Clear { fragment } => set(fragment, vec![]).await,
    }
}

async fn show(fragment: String) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let g = GraphClient::new(AuthClient::from_env()?)?;
    let cats = g.get_categories(&msg.account, &msg.graph_id).await?;
    if cats.is_empty() {
        println!("{} no categories", short.dimmed());
    } else {
        println!("{} {}", short.dimmed(), cats.join(", ").cyan());
    }
    Ok(())
}

async fn set(fragment: String, labels: Vec<String>) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let g = GraphClient::new(AuthClient::from_env()?)?;
    g.set_categories(&msg.account, &msg.graph_id, &labels)
        .await?;
    let _ = purge_from_cache(&short);
    if labels.is_empty() {
        println!("{} cleared categories on {}", "✔".green(), short.dimmed());
    } else {
        println!(
            "{} set categories on {} → {}",
            "✔".green(),
            short.dimmed(),
            labels.join(", ").cyan()
        );
    }
    Ok(())
}

async fn add(fragment: String, labels: Vec<String>) -> Result<()> {
    let (short, msg) = resolve(&fragment)?;
    let g = GraphClient::new(AuthClient::from_env()?)?;
    let mut cats = g.get_categories(&msg.account, &msg.graph_id).await?;
    for l in labels {
        if !cats.iter().any(|c| c.eq_ignore_ascii_case(&l)) {
            cats.push(l);
        }
    }
    g.set_categories(&msg.account, &msg.graph_id, &cats).await?;
    println!(
        "{} categories on {} → {}",
        "✔".green(),
        short.dimmed(),
        cats.join(", ").cyan()
    );
    Ok(())
}

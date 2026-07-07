//! `pidge calendar delete <hash>` — silent removal (no attendee notice).

use anyhow::Result;
use std::io::Write;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Config;

use crate::commands::calendar_fragment;

pub async fn run(fragment: &str, yes: bool, series: bool, json: bool) -> Result<()> {
    let gate = crate::guardrail::gate(
        crate::guardrail::GuardrailAction::Delete,
        &format!("delete event {fragment}"),
    )?;
    if gate == crate::guardrail::Gate::DryRun {
        return Ok(());
    }

    let (hash, r) = calendar_fragment::resolve(fragment)?;
    let _config = Config::load()?;
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let mut event_id = r.event_id.clone();
    if series {
        let e = graph.get_event(&r.account, &event_id).await?;
        if let Some(m) = e.series_master_id {
            event_id = m;
        }
    }
    if !yes {
        print!("Delete event {hash}? [y/N] ");
        std::io::stdout().flush()?;
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        if !s.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }
    graph.delete_event(&r.account, &event_id).await?;
    calendar_fragment::purge_from_cache(&hash)?;
    if json {
        println!("{}", serde_json::json!({ "ok": true }));
    } else {
        println!("Deleted.");
    }
    Ok(())
}

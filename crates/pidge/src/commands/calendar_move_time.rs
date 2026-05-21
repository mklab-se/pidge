//! `pidge calendar move-time <hash> --start ... [--end ...]` — reschedule.

use anyhow::Result;
use chrono::Utc;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Config;

use crate::commands::calendar_fragment;
use crate::commands::time::parse_when;
use crate::output::resolve_tz;

#[allow(clippy::too_many_arguments)]
pub async fn run(
    fragment: &str,
    start_s: &str,
    end_s: Option<&str>,
    notify: bool,
    no_notify: bool,
    series: bool,
    json: bool,
) -> Result<()> {
    let (_hash, r) = calendar_fragment::resolve(fragment)?;
    let _config = Config::load()?;
    let auth = AuthClient::from_env()?;
    let graph = GraphClient::new(auth)?;
    let cur = graph.get_event(&r.account, &r.event_id).await?;

    let mut event_id = r.event_id.clone();
    if series {
        if let Some(m) = &cur.series_master_id {
            event_id = m.clone();
        }
    }
    let tz = resolve_tz(Some(&cur.start.tz));
    let new_start = parse_when(start_s, &tz, Utc::now(), None)?;
    let new_end = match end_s {
        Some(s) => parse_when(s, &tz, Utc::now(), Some(new_start))?,
        None => new_start + (cur.end.at - cur.start.at),
    };
    if new_end <= new_start {
        anyhow::bail!("end must be after start");
    }
    graph
        .move_time(&r.account, &event_id, new_start, new_end, &cur.start.tz)
        .await?;
    let _ = (notify, no_notify);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "ok": true, "id": event_id,
                "start": new_start.to_rfc3339(), "end": new_end.to_rfc3339()
            })
        );
    } else {
        println!(
            "Moved \"{}\" to {}.",
            cur.subject,
            new_start.with_timezone(&tz).format("%a %d %b %H:%M")
        );
    }
    Ok(())
}

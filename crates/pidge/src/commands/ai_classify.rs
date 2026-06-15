//! `pidge ai classify` — compute label(s) for an e-mail (or literal text)
//! using the configured AI provider.

use anyhow::{Result, anyhow};
use serde::Serialize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Config;

use crate::cli::ClassifyArgs;
use crate::commands::classify_cache::{ClassifyCache, cache_key};
use crate::commands::classify_model::{AilloyModel, LabelModel, build_input};
use crate::commands::classify_parse::{parse_labels, validate_labels};
use crate::commands::mail_fragment::resolve;
use futures::stream::{self, StreamExt};

#[derive(Serialize)]
struct ClassifyOut {
    hash: Option<String>,
    from: Option<String>,
    classification: Vec<String>,
}

/// Resolve the effective prompt from flags then config. Pure + tested.
pub fn resolve_prompt(
    prompt: Option<String>,
    prompt_file: Option<String>,
    config_prompt: Option<String>,
) -> Result<String> {
    if let Some(p) = prompt {
        return Ok(p);
    }
    if let Some(f) = prompt_file {
        let s = if f == "-" {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s)?;
            s
        } else {
            std::fs::read_to_string(&f)?
        };
        return Ok(s);
    }
    config_prompt.ok_or_else(|| {
        anyhow!("no prompt: pass --prompt/--prompt-file or set one with `pidge config set classify.prompt`")
    })
}

pub async fn run(args: ClassifyArgs, json: bool) -> Result<()> {
    if !crate::commands::ai::is_ai_active() {
        return Err(anyhow!(
            "AI is not active for pidge. Run `pidge ai enable` and `pidge ai config` to set up a provider."
        ));
    }
    let config = Config::load()?;
    let prompt = resolve_prompt(
        args.prompt.clone(),
        args.prompt_file.clone(),
        config.classify.prompt.clone(),
    )?;
    let allowed = if args.labels.is_empty() {
        config.classify.labels.clone()
    } else {
        args.labels.clone()
    };

    // Text mode: no mailbox.
    if let Some(text) = args.text.clone() {
        let model = AilloyModel::new()?;
        let raw = model.classify(&prompt, &text).await?;
        let labels = validate_labels(parse_labels(&raw), &allowed);
        emit_single(None, None, labels, json)?;
        return Ok(());
    }

    // Single mode.
    if let Some(fragment) = args.fragment.clone() {
        let (short, msg) = resolve(&fragment)?;
        let g = GraphClient::new(AuthClient::from_env()?)?;
        let full = g.get_message(&msg.account, &msg.graph_id).await?;
        let input = build_input(&full.subject, &full.from.address, &body_text(&full));
        let model = AilloyModel::new()?;
        let labels = validate_labels(
            parse_labels(&model.classify(&prompt, &input).await?),
            &allowed,
        );
        if args.set_category && worth_categorizing(&labels) {
            g.set_categories(&msg.account, &msg.graph_id, &labels)
                .await?;
        }
        emit_single(Some(short), Some(full.from.address), labels, json)?;
        return Ok(());
    }

    // Batch mode — implemented in the next task.
    run_batch(args, &prompt, &allowed, json).await
}

#[allow(clippy::type_complexity)]
pub(crate) async fn run_batch(
    args: ClassifyArgs,
    prompt: &str,
    allowed: &[String],
    json: bool,
) -> Result<()> {
    let config = Config::load()?;
    let parallel = args
        .parallel
        .or(config.classify.parallel)
        .unwrap_or(4)
        .max(1);
    let use_cache = !args.no_cache && config.classify.cache.unwrap_or(true);

    let g = GraphClient::new(AuthClient::from_env()?)?;
    let messages = select_messages(&g, &args).await?;

    let model = AilloyModel::new()?;
    let mut cache = if use_cache {
        ClassifyCache::load()
    } else {
        ClassifyCache::default()
    };

    // Snapshot cache hits up front (immutable borrow of cache), then classify
    // misses concurrently. Writing back happens after the concurrent block.
    let prepared: Vec<(String, String, String, String, String, Option<Vec<String>>)> = messages
        .into_iter()
        .map(|(account, id, short, from)| {
            let key = cache_key(&id, prompt);
            let hit = if use_cache { cache.get(&key) } else { None };
            (account, id, short, from, key, hit)
        })
        .collect();

    let set_category = args.set_category;
    let tasks = prepared
        .into_iter()
        .map(|(account, id, short, from, key, hit)| {
            let g = &g;
            let model = &model;
            async move {
                let (labels, fresh) = match hit {
                    Some(l) => (l, false),
                    None => match classify_one(g, model, &account, &id, prompt, allowed).await {
                        Ok(l) => (l, true),
                        Err(e) => {
                            eprintln!("  ! {short}: {e}");
                            (vec!["unknown".to_string()], false)
                        }
                    },
                };
                if set_category && worth_categorizing(&labels) {
                    if let Err(e) = g.set_categories(&account, &id, &labels).await {
                        eprintln!("  ! {short}: set-category failed: {e}");
                    }
                }
                (short, from, key, labels, fresh)
            }
        });

    let results: Vec<(String, String, String, Vec<String>, bool)> = stream::iter(tasks)
        .buffer_unordered(parallel)
        .collect()
        .await;

    let mut out = Vec::new();
    for (short, from, key, labels, fresh) in results {
        if use_cache && fresh {
            cache.put(key, labels.clone());
        }
        out.push(ClassifyOut {
            hash: Some(short),
            from: Some(from),
            classification: labels,
        });
    }
    if use_cache {
        cache.save();
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        for o in &out {
            println!(
                "{}  {}  {}",
                o.hash.as_deref().unwrap_or(""),
                o.from.as_deref().unwrap_or(""),
                o.classification.join(", ")
            );
        }
    }
    Ok(())
}

async fn classify_one<M: LabelModel>(
    g: &GraphClient,
    model: &M,
    account: &str,
    id: &str,
    prompt: &str,
    allowed: &[String],
) -> Result<Vec<String>> {
    let full = g.get_message(account, id).await?;
    let input = build_input(&full.subject, &full.from.address, &body_text(&full));
    let raw = model.classify(prompt, &input).await?;
    Ok(validate_labels(parse_labels(&raw), allowed))
}

/// Resolve the filters to a list of (account, graph_id, short_hash, from_addr).
/// Sender mode searches all folders per sender; folder mode lists that folder;
/// otherwise the Inbox. `--older-than` filters by received time; `--limit`
/// caps the total.
#[allow(clippy::type_complexity)]
async fn select_messages(
    g: &GraphClient,
    args: &ClassifyArgs,
) -> Result<Vec<(String, String, String, String)>> {
    use pidge_core::short_hash;

    let config = Config::load()?;
    if config.accounts.is_empty() {
        return Err(anyhow!("No accounts signed in. Run `pidge account add`."));
    }
    let target_emails: Vec<String> = if args.account.is_empty() {
        config.accounts.iter().map(|a| a.email.clone()).collect()
    } else {
        for f in &args.account {
            if config.find(f).is_none() {
                return Err(anyhow!("not signed in to {f}"));
            }
        }
        args.account.clone()
    };

    let cutoff = args
        .older_than
        .as_deref()
        .map(crate::commands::mail_delete::parse_older_than)
        .transpose()?;
    let fetch = args.limit.unwrap_or(100).max(50);

    let mut out = Vec::new();
    for email in &target_emails {
        let msgs: Vec<pidge_core::Message> = if !args.from.is_empty() {
            let mut v = Vec::new();
            for sender in &args.from {
                let found = g
                    .search_messages(email, &format!("from:{sender}"), 1000)
                    .await
                    .unwrap_or_default();
                let s = sender.to_ascii_lowercase();
                v.extend(
                    found
                        .into_iter()
                        .filter(|m| m.from.address.to_ascii_lowercase() == s),
                );
            }
            v
        } else if let Some(folder) = &args.folder {
            match crate::commands::mail_folders::resolve_folder_path(g, email, folder).await? {
                Some(id) => g
                    .list_folder(email, &id, fetch, 0, false)
                    .await
                    .map(|p| p.messages)
                    .unwrap_or_default(),
                None => {
                    eprintln!("  ! no folder '{folder}' in {email}");
                    Vec::new()
                }
            }
        } else {
            g.list_inbox(email, fetch, 0, false)
                .await
                .map(|p| p.messages)
                .unwrap_or_default()
        };

        for m in msgs {
            if let Some(c) = cutoff {
                if m.received_at >= c {
                    continue;
                }
            }
            out.push((
                email.clone(),
                m.id.clone(),
                short_hash(&m.id),
                m.from.address.clone(),
            ));
        }
    }

    if let Some(limit) = args.limit {
        out.truncate(limit);
    }
    Ok(out)
}

pub(crate) fn body_text(m: &pidge_core::FullMessage) -> String {
    use pidge_core::BodyContentType;
    match m.body_content_type {
        BodyContentType::Text => m.body_content.clone(),
        BodyContentType::Html => html2text::from_read(m.body_content.as_bytes(), 100),
    }
}

/// Whether a label set is worth writing as categories. A lone `unknown`
/// (failed/empty/out-of-set classification) is not.
fn worth_categorizing(labels: &[String]) -> bool {
    !(labels.len() == 1 && labels[0] == "unknown")
}

fn emit_single(
    hash: Option<String>,
    from: Option<String>,
    labels: Vec<String>,
    json: bool,
) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&ClassifyOut {
                hash,
                from,
                classification: labels
            })?
        );
    } else {
        for l in labels {
            println!("{l}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prompt_prefers_flag() {
        assert_eq!(
            resolve_prompt(Some("flag".into()), None, Some("cfg".into())).unwrap(),
            "flag"
        );
    }
    #[test]
    fn resolve_prompt_falls_back_to_config() {
        assert_eq!(
            resolve_prompt(None, None, Some("cfg".into())).unwrap(),
            "cfg"
        );
    }
    #[test]
    fn resolve_prompt_errors_when_unset() {
        assert!(resolve_prompt(None, None, None).is_err());
    }

    #[test]
    fn worth_categorizing_rejects_lone_unknown() {
        assert!(!worth_categorizing(&["unknown".to_string()]));
        assert!(worth_categorizing(&["receipt".to_string()]));
        assert!(worth_categorizing(&[
            "unknown".to_string(),
            "receipt".to_string()
        ]));
    }
}

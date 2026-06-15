//! `pidge ai classify` — compute label(s) for an e-mail (or literal text)
//! using the configured AI provider.

use anyhow::{Result, anyhow};
use serde::Serialize;

use pidge_client::{AuthClient, GraphClient};
use pidge_core::Config;

use crate::cli::ClassifyArgs;
use crate::commands::classify_model::{AilloyModel, LabelModel, build_input};
use crate::commands::classify_parse::{parse_labels, validate_labels};
use crate::commands::mail_fragment::resolve;

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
        if args.set_category {
            g.set_categories(&msg.account, &msg.graph_id, &labels)
                .await?;
        }
        emit_single(Some(short), Some(full.from.address), labels, json)?;
        return Ok(());
    }

    // Batch mode — implemented in the next task.
    run_batch(args, &prompt, &allowed, json).await
}

pub(crate) async fn run_batch(
    _args: ClassifyArgs,
    _prompt: &str,
    _allowed: &[String],
    _json: bool,
) -> Result<()> {
    Err(anyhow!(
        "batch classification is implemented in a later step"
    ))
}

pub(crate) fn body_text(m: &pidge_core::FullMessage) -> String {
    use pidge_core::BodyContentType;
    match m.body_content_type {
        BodyContentType::Text => m.body_content.clone(),
        BodyContentType::Html => html2text::from_read(m.body_content.as_bytes(), 100),
    }
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
}

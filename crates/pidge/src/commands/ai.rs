//! `pidge ai`: the optional AI key pidge uses for sorting mail and checking
//! sends, configured through Ailloy (shared with rigg, mdeck, cosq).

use anyhow::Result;

use ailloy::config::Config;
use ailloy::config_tui;

use crate::cli::AiCommands;

/// The capabilities pidge asks Ailloy for: `eval` (typed judgements).
const CAPS: &[&str] = &["eval"];

pub async fn run(cmd: Option<AiCommands>) -> Result<()> {
    match cmd {
        None | Some(AiCommands::Status) => config_tui::print_ai_status("pidge", CAPS),
        Some(AiCommands::Test { message }) => config_tui::run_test_chat("pidge", message).await,
        Some(AiCommands::Enable) => config_tui::enable_ai("pidge"),
        Some(AiCommands::Disable) => config_tui::disable_ai("pidge"),
        Some(AiCommands::Config) => {
            let mut config = Config::load_global()?;
            config_tui::run_interactive_config(&mut config, CAPS).await?;
            Ok(())
        }
    }
}

/// The user's AI client, when they set one up for pidge.
pub fn client() -> Option<ailloy::Client> {
    if !config_tui::is_ai_active("pidge") {
        return None;
    }
    ailloy::Client::for_capability("eval").ok()
}

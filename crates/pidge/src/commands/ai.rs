//! AI feature management
//!
//! `pidge ai`         — show status
//! `pidge ai test`    — test AI connection
//! `pidge ai enable`  — enable AI for pidge
//! `pidge ai disable` — disable AI for pidge
//! `pidge ai config`  — interactive AI node configuration

use anyhow::Result;

use ailloy::config::Config;
use ailloy::config_tui;

use crate::cli::AiCommands;

pub async fn run(cmd: Option<AiCommands>) -> Result<()> {
    match cmd {
        None => config_tui::print_ai_status("pidge", &["chat"]),
        Some(AiCommands::Test { message }) => config_tui::run_test_chat("pidge", message).await,
        Some(AiCommands::Enable) => config_tui::enable_ai("pidge"),
        Some(AiCommands::Disable) => config_tui::disable_ai("pidge"),
        Some(AiCommands::Config) => {
            let mut config = Config::load_global()?;
            config_tui::run_interactive_config(&mut config, &["chat"]).await?;
            Ok(())
        }
        Some(AiCommands::Status) => config_tui::print_ai_status("pidge", &["chat"]),
        Some(AiCommands::Skill { emit, from_source }) => {
            crate::commands::skill::run(emit, from_source)
        }
    }
}

/// Check if AI features are active (configured via ailloy + enabled for this tool).
#[allow(dead_code)]
pub fn is_ai_active() -> bool {
    config_tui::is_ai_active("pidge")
}

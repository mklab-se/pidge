//! `pidge skill`: print a SKILL.md that wires pidge into an AI agent,
//! generated from the same operations and playbooks as the MCP tools.

use anyhow::{Context, Result};
use pidge_engine::playbooks::{SkillPrefix, skill};

pub fn run(from_source: bool) -> Result<()> {
    let prefix = if from_source {
        let cwd = std::env::current_dir().context("could not resolve the current directory")?;
        let dir = cwd.to_string_lossy();
        SkillPrefix {
            invocation: format!("(cd {dir} && cargo run --quiet --)"),
            setup_note: format!(
                "This skill runs pidge from the source checkout at `{dir}` with `cargo run`. \
                 When pidge itself is the problem (a missing operation, a confusing answer), \
                 improve it there; if you are working in another repository, ask the user first."
            ),
        }
    } else {
        SkillPrefix {
            invocation: "pidge".into(),
            setup_note: "This skill uses the installed `pidge` binary. If `pidge` is missing, ask the \
                         user to install it (`brew install mklab-se/tap/pidge` or `cargo install pidge`) \
                         and sign in with `pidge account add`."
                .into(),
        }
    };
    print!("{}", skill(&pidge_engine::registry::all(), &prefix));
    Ok(())
}

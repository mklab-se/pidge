//! `pidge config` — read/write pidge's own settings.

use anyhow::{Result, anyhow};
use colored::Colorize;
use pidge_core::Config;

use crate::cli::ConfigCommands;

pub fn run(command: ConfigCommands) -> Result<()> {
    match command {
        ConfigCommands::Show => show(),
        ConfigCommands::Get { key } => get(&key),
        ConfigCommands::Set { key, value, file } => set(&key, value, file),
        ConfigCommands::Unset { key } => unset(&key),
    }
}

fn show() -> Result<()> {
    let config = Config::load()?;
    for key in Config::KNOWN_KEYS {
        let val = config.get_key(key).unwrap_or_else(|| "(unset)".to_string());
        println!("{} = {}", key.cyan(), val.dimmed());
    }
    Ok(())
}

fn get(key: &str) -> Result<()> {
    let config = Config::load()?;
    match config.get_key(key) {
        Some(v) => println!("{v}"),
        None => {
            return Err(anyhow!(
                "'{key}' is unset (or unknown). See `pidge config show`."
            ));
        }
    }
    Ok(())
}

fn read_value(value: Option<String>, file: Option<String>) -> Result<String> {
    match (value, file) {
        (Some(_), Some(_)) => Err(anyhow!("pass either an inline value or --file, not both")),
        (Some(v), None) => Ok(v),
        (None, Some(f)) => {
            if f == "-" {
                use std::io::Read;
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                Ok(s.trim_end_matches('\n').to_string())
            } else {
                Ok(std::fs::read_to_string(&f)?
                    .trim_end_matches('\n')
                    .to_string())
            }
        }
        (None, None) => Err(anyhow!(
            "provide a value, --file <path>, or `--file -` for stdin"
        )),
    }
}

fn set(key: &str, value: Option<String>, file: Option<String>) -> Result<()> {
    let v = read_value(value, file)?;
    let mut config = Config::load()?;
    config.set_key(key, &v)?;
    config.save()?;
    println!("{} set {}", "✔".green(), key.cyan());
    Ok(())
}

fn unset(key: &str) -> Result<()> {
    let mut config = Config::load()?;
    config.unset_key(key)?;
    config.save()?;
    println!("{} unset {}", "✔".green(), key.cyan());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_value_rejects_both_sources() {
        assert!(read_value(Some("x".into()), Some("f".into())).is_err());
    }
    #[test]
    fn read_value_inline() {
        assert_eq!(read_value(Some("x".into()), None).unwrap(), "x");
    }
    #[test]
    fn read_value_requires_a_source() {
        assert!(read_value(None, None).is_err());
    }
}

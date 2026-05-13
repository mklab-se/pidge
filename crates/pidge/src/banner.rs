//! ASCII art banner for pidge CLI

use colored::Colorize;

const LOGO: &str = r#"
██████╗ ██╗██████╗  ██████╗ ███████╗
██╔══██╗██║██╔══██╗██╔════╝ ██╔════╝
██████╔╝██║██║  ██║██║  ███╗█████╗
██╔═══╝ ██║██║  ██║██║   ██║██╔══╝
██║     ██║██████╔╝╚██████╔╝███████╗
╚═╝     ╚═╝╚═════╝  ╚═════╝ ╚══════╝"#;

/// Print the pidge ASCII art banner.
pub fn print_banner() {
    for line in LOGO.lines() {
        println!("{}", line.bold());
    }
}

/// Print the banner with version and subtitle.
pub fn print_banner_with_version() {
    print_banner();
    println!(
        " {} {}",
        "A fast CLI for e-mail and calendar".dimmed(),
        format!("v{}", env!("CARGO_PKG_VERSION")).dimmed(),
    );
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logo_is_not_empty() {
        assert!(!LOGO.is_empty());
    }

    #[test]
    fn logo_has_six_visible_lines() {
        let lines: Vec<&str> = LOGO.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 6, "Logo should have 6 lines of block letters");
    }
}

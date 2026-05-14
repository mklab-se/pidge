//! OSC 8 hyperlink helper.
//!
//! Modern terminals interpret the OSC 8 escape sequence as a clickable hyperlink:
//!     ESC ] 8 ; ; URL ST TEXT ESC ] 8 ; ; ST
//! where ST is the String Terminator (ESC \).
//!
//! Terminals that don't understand OSC 8 strip the escape sequences and render
//! only the visible `text`, so there's no visual breakage on legacy terminals.
//!
//! Reference: https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda

const OSC8_START: &str = "\x1b]8;;";
const ST: &str = "\x1b\\";

/// Wrap `text` so clicking it in a supporting terminal opens `url`.
pub fn hyperlink(url: &str, text: &str) -> String {
    format!("{OSC8_START}{url}{ST}{text}{OSC8_START}{ST}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyperlink_wraps_text_with_url() {
        let out = hyperlink("https://example.com", "click me");
        assert!(out.contains("https://example.com"));
        assert!(out.contains("click me"));
        assert!(out.starts_with("\x1b]8;;"));
        assert!(out.ends_with("\x1b\\"));
    }

    #[test]
    fn hyperlink_url_appears_before_text() {
        let out = hyperlink("https://example.com", "click me");
        let url_pos = out.find("https://example.com").unwrap();
        let text_pos = out.find("click me").unwrap();
        assert!(url_pos < text_pos);
    }
}

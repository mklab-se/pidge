//! The pictures at the top of the pages a person sees (connect a mailbox,
//! sign an app in, set the AI key, done, something went wrong), with the
//! names laid over them as HTML text: escaped like every other string on
//! the page, sharp at any size, and selectable. The images are templates
//! with blank labels, built into the binary and served from `/art/…`.

use axum::extract::Path;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

/// One template: its file, aspect ratio, and the label areas in it.
pub struct Picture {
    name: &'static str,
    bytes: &'static [u8],
    width: u32,
    height: u32,
    /// The small object on the left (the envelope's card, the robot's
    /// chest, the key tag); `None` for the mascot-only pictures.
    card: Option<Area>,
    /// The server's front panel.
    panel: Area,
}

/// A label area, in percent of the picture, rotated to follow the surface.
#[derive(Clone, Copy)]
struct Area {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    /// Degrees, clockwise.
    tilt: f32,
    /// Font size of the main line, in percent of the picture's width.
    size: f32,
}

pub const CONNECT: Picture = Picture {
    name: "connect",
    bytes: include_bytes!("../../assets/connect.webp"),
    width: 1100,
    height: 550,
    card: Some(Area {
        left: 12.1,
        top: 46.0,
        width: 21.6,
        height: 10.4,
        tilt: -7.5,
        size: 2.4,
    }),
    panel: Area {
        left: 62.6,
        top: 65.6,
        width: 18.6,
        height: 13.2,
        tilt: 1.8,
        size: 2.1,
    },
};

pub const SIGN_IN: Picture = Picture {
    name: "sign-in",
    bytes: include_bytes!("../../assets/sign-in.webp"),
    width: 1100,
    height: 550,
    card: Some(Area {
        left: 17.6,
        top: 68.4,
        width: 13.6,
        height: 8.4,
        tilt: -9.0,
        size: 2.2,
    }),
    panel: Area {
        left: 61.7,
        top: 67.8,
        width: 19.4,
        height: 13.4,
        tilt: 1.8,
        size: 2.1,
    },
};

pub const AI_KEY: Picture = Picture {
    name: "ai-key",
    bytes: include_bytes!("../../assets/ai-key.webp"),
    width: 1100,
    height: 550,
    card: Some(Area {
        left: 22.4,
        top: 54.0,
        width: 17.6,
        height: 10.4,
        tilt: -18.0,
        size: 2.6,
    }),
    panel: Area {
        left: 61.7,
        top: 66.8,
        width: 19.4,
        height: 13.4,
        tilt: 1.8,
        size: 2.1,
    },
};

pub const DONE: Picture = Picture {
    name: "done",
    bytes: include_bytes!("../../assets/done.webp"),
    width: 1100,
    height: 842,
    card: None,
    panel: Area {
        left: 24.0,
        top: 68.2,
        width: 38.6,
        height: 17.0,
        tilt: 2.8,
        size: 3.6,
    },
};

pub const ERROR: Picture = Picture {
    name: "error",
    bytes: include_bytes!("../../assets/error.webp"),
    width: 1100,
    height: 842,
    card: None,
    panel: Area {
        left: 27.4,
        top: 69.0,
        width: 35.6,
        height: 16.0,
        tilt: 2.8,
        size: 3.4,
    },
};

const ALL: [&Picture; 5] = [&CONNECT, &SIGN_IN, &AI_KEY, &DONE, &ERROR];

/// `GET /art/{file}`: one of the pictures, cacheable (they never change
/// within a release); anything else is a plain 404.
pub async fn serve(Path(file): Path<String>) -> Response {
    let Some(p) = file
        .strip_suffix(".webp")
        .and_then(|name| ALL.iter().find(|p| p.name == name))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (header::CONTENT_TYPE, "image/webp"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        p.bytes,
    )
        .into_response()
}

/// What goes on the picture: the card's text, and the panel's caption and
/// value. Plain text; [`Picture::html`] escapes it.
pub struct Labels<'a> {
    pub card: Option<&'a str>,
    pub caption: &'a str,
    pub value: &'a str,
}

impl Picture {
    /// The picture with its labels, as an HTML fragment for a page.
    pub fn html(&self, labels: &Labels<'_>, describe: &str) -> String {
        let mut out = format!(
            r#"<figure class="art{}" role="img" aria-label="{}"><img src="/art/{}.webp" width="{}" height="{}" alt="">"#,
            if self.card.is_none() { " mascot" } else { "" },
            escape(describe),
            self.name,
            self.width,
            self.height
        );
        if let (Some(area), Some(text)) = (self.card, labels.card) {
            out.push_str(&format!(
                r#"<div class="lbl card" style="{}">{}</div>"#,
                area.style(),
                lines(area, text, "")
            ));
        }
        let a = self.panel;
        out.push_str(&format!(
            r#"<div class="lbl panel" style="{}"><span class="k" style="font-size:{:.2}cqw">{}</span>{}</div>"#,
            a.style(),
            a.size * 0.62,
            escape(labels.caption),
            lines(a, labels.value, "v")
        ));
        out.push_str("</figure>");
        out
    }
}

impl Area {
    fn style(&self) -> String {
        format!(
            "left:{:.2}%;top:{:.2}%;width:{:.2}%;height:{:.2}%;transform:rotate({:.1}deg)",
            self.left, self.top, self.width, self.height, self.tilt
        )
    }
}

/// Longer than this, an address goes on two lines, split before the `@`,
/// so it can be set larger than one long line would allow.
const ONE_LINE: usize = 14;

/// `text` as one or two spans (split before the `@` when long), sized so
/// the longest line fits the area.
fn lines(area: Area, text: &str, class: &str) -> String {
    let parts: Vec<&str> = match text.find('@') {
        Some(at) if text.chars().count() > ONE_LINE && at > 0 => vec![&text[..at], &text[at..]],
        _ => vec![text],
    };
    let longest = parts.iter().map(|p| p.chars().count()).max().unwrap_or(1);
    let size = fit(area, longest);
    parts
        .iter()
        .map(|p| {
            format!(
                r#"<span{} style="font-size:{size:.2}cqw">{}</span>"#,
                if class.is_empty() {
                    String::new()
                } else {
                    format!(r#" class="{class}""#)
                },
                escape(p)
            )
        })
        .collect()
}

/// The main line's size: the area's own size, smaller for a long line so
/// it still fits (about 0.66 em per character of bold text, with room).
fn fit(area: Area, chars: usize) -> f32 {
    let chars = chars.max(1) as f32;
    area.size.min(area.width * 0.9 / (0.66 * chars))
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The stylesheet for [`Picture::html`]; part of every page's inline CSS.
pub const CSS: &str = "\
.art{position:relative;container-type:inline-size;margin:0 0 1rem;width:100%}\
.art img{display:block;width:100%;height:auto}\
.art.mascot{max-width:22rem;margin-left:auto;margin-right:auto}\
.lbl{position:absolute;display:flex;flex-direction:column;justify-content:center;transform-origin:center;overflow:hidden;white-space:nowrap;line-height:1.15}\
.lbl span{overflow:hidden;text-overflow:ellipsis}\
.card{align-items:center;text-align:center;color:#1b2140;font-weight:700}\
.panel{align-items:flex-start;padding-left:3%;box-sizing:border-box}\
.panel .k{color:#a9a3ff;font-weight:600}\
.panel .v{color:#fff;font-weight:700}";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_escaped_and_long_ones_shrink() {
        let html = CONNECT.html(
            &Labels {
                card: Some("<b>evil</b>@example.com"),
                caption: "Pidge account",
                value: "jane@example.com",
            },
            "A mailbox joins a pidge account",
        );
        assert!(html.contains(">&lt;b&gt;evil&lt;/b&gt;<"), "{html}");
        assert!(!html.contains("<b>"), "{html}");
        let short = fit(CONNECT.panel, 6);
        let long = fit(CONNECT.panel, 40);
        assert!(long < short, "{short} {long}");
        assert_eq!(short, CONNECT.panel.size);
        // A long address goes on two lines, split before the `@`.
        let two = lines(CONNECT.panel, "kristofer.liljeblad@live.com", "v");
        assert_eq!(two.matches("<span").count(), 2, "{two}");
        assert!(two.contains(">@live.com<"), "{two}");
        assert_eq!(
            lines(CONNECT.panel, "a@b.se", "v").matches("<span").count(),
            1
        );
    }

    #[tokio::test]
    async fn every_picture_is_served_and_nothing_else() {
        for p in ALL {
            let r = serve(Path(format!("{}.webp", p.name))).await;
            assert_eq!(r.status(), StatusCode::OK);
            assert_eq!(r.headers()[header::CONTENT_TYPE], "image/webp");
            // The bytes really are a WebP image.
            assert_eq!(&p.bytes[0..4], b"RIFF");
            assert_eq!(&p.bytes[8..12], b"WEBP");
        }
        for bad in ["connect.png", "../main.rs", "x.webp", "connect"] {
            assert_eq!(
                serve(Path(bad.into())).await.status(),
                StatusCode::NOT_FOUND
            );
        }
    }
}

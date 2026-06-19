//! Render Markdown (parsed by `kiro_core::markdown`) into GPUI elements.
//!
//! Block structure drives distinct styling: headings are larger/bold, fenced
//! code blocks use a monospaced block with a distinct background, lists get
//! bullet/number prefixes, and paragraphs are plain wrapped text.

use gpui::{div, prelude::*, px, AnyElement, SharedString};
use kiro_core::highlight::TokenKind;
use kiro_core::markdown::{self, Block};

use crate::theme::{self, c};

/// Map a syntax token kind to a theme color.
fn token_color(kind: TokenKind) -> u32 {
    match kind {
        TokenKind::Keyword => theme::MAUVE,
        TokenKind::Str => theme::GREEN,
        TokenKind::Comment => theme::OVERLAY,
        TokenKind::Number => theme::YELLOW,
        TokenKind::Ident => theme::TEXT,
        TokenKind::Punct => theme::TEAL,
        TokenKind::Plain => theme::TEXT,
    }
}

/// Render a Markdown string into a vertical stack of styled elements.
pub fn render(text: &str) -> AnyElement {
    let blocks = markdown::parse(text);
    let mut container = div().flex().flex_col().gap_2().w_full();

    for block in blocks {
        container = container.child(render_block(block));
    }

    container.into_any_element()
}

fn render_block(block: Block) -> AnyElement {
    match block {
        Block::Heading { level, text } => {
            let size = match level {
                1 => px(20.0),
                2 => px(18.0),
                3 => px(16.0),
                _ => px(14.0),
            };
            div()
                .text_size(size)
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(c(theme::TEXT))
                .child(SharedString::from(text))
                .into_any_element()
        }
        Block::Paragraph { text } => div()
            .text_color(c(theme::TEXT))
            .child(SharedString::from(text))
            .into_any_element(),
        Block::Code { language, code } => {
            let header = language.clone().unwrap_or_else(|| "code".into());
            let lines = kiro_core::highlight::highlight(&code, language.as_deref());

            let mut body = div().flex().flex_col().p_2().bg(c(theme::CRUST)).text_sm();
            for line in lines {
                let mut row = div().flex().flex_row().font_family("monospace");
                if line.is_empty() {
                    // Preserve blank lines with a zero-width space placeholder.
                    row = row.child(div().child(SharedString::from(" ")));
                }
                for span in line {
                    row = row.child(
                        div()
                            .text_color(c(token_color(span.kind)))
                            .child(SharedString::from(span.text)),
                    );
                }
                body = body.child(row);
            }

            div()
                .flex()
                .flex_col()
                .w_full()
                .rounded_md()
                .overflow_hidden()
                .border_1()
                .border_color(c(theme::SURFACE0))
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .bg(c(theme::SURFACE0))
                        .text_xs()
                        .text_color(c(theme::SUBTEXT))
                        .child(SharedString::from(header)),
                )
                .child(body)
                .into_any_element()
        }
        Block::List { ordered, items } => {
            let mut list = div().flex().flex_col().gap_1().w_full().pl_2();
            for (i, item) in items.into_iter().enumerate() {
                let prefix = if ordered {
                    format!("{}. ", i + 1)
                } else {
                    "• ".to_string()
                };
                list = list.child(
                    div()
                        .flex()
                        .flex_row()
                        .gap_1()
                        .text_color(c(theme::TEXT))
                        .child(div().text_color(c(theme::OVERLAY)).child(prefix))
                        .child(div().flex_1().child(SharedString::from(item))),
                );
            }
            list.into_any_element()
        }
    }
}

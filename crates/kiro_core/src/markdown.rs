//! A small block-level Markdown parser built on `pulldown-cmark`.
//!
//! It lives in `kiro_core` (free of any GPUI types) so the parsing logic can be
//! unit-tested headlessly. The `kiro_ui` crate maps the resulting [`Block`]s to
//! GPUI elements.
//!
//! Inline emphasis (bold/italic) is flattened into the text while inline code
//! spans are preserved as text; block structure (headings, paragraphs, fenced
//! code blocks, and lists) is modeled explicitly — that's what drives distinct
//! rendering.

use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};

/// A block-level Markdown element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A heading with its level (1-6) and text.
    Heading {
        /// Heading level, 1–6.
        level: u8,
        /// Flattened text.
        text: String,
    },
    /// A paragraph of (inline-flattened) text.
    Paragraph {
        /// Flattened text.
        text: String,
    },
    /// A fenced or indented code block.
    Code {
        /// Optional language tag.
        language: Option<String>,
        /// Raw code (without the fences).
        code: String,
    },
    /// A bullet or ordered list; each entry is one item's flattened text.
    List {
        /// Whether the list is ordered.
        ordered: bool,
        /// Item texts.
        items: Vec<String>,
    },
}

/// Parse Markdown into a flat list of [`Block`]s.
pub fn parse(markdown: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let parser = Parser::new(markdown);

    // Accumulators for the block currently being built.
    let mut text = String::new();
    let mut heading_level: Option<u8> = None;
    let mut in_paragraph = false;
    let mut code: Option<(Option<String>, String)> = None;

    // List state.
    let mut list_ordered: Option<bool> = None;
    let mut list_items: Vec<String> = Vec::new();
    let mut item_text = String::new();
    let mut in_item = false;

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                heading_level = Some(level as u8);
                text.clear();
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some(level) = heading_level.take() {
                    blocks.push(Block::Heading {
                        level,
                        text: std::mem::take(&mut text),
                    });
                }
            }
            Event::Start(Tag::Paragraph) => {
                in_paragraph = true;
                text.clear();
            }
            Event::End(TagEnd::Paragraph) => {
                in_paragraph = false;
                let t = std::mem::take(&mut text);
                if !t.trim().is_empty() {
                    blocks.push(Block::Paragraph { text: t });
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(s) if !s.is_empty() => Some(s.to_string()),
                    _ => None,
                };
                code = Some((lang, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((language, mut body)) = code.take() {
                    if body.ends_with('\n') {
                        body.pop();
                    }
                    blocks.push(Block::Code {
                        language,
                        code: body,
                    });
                }
            }
            Event::Start(Tag::List(first)) => {
                list_ordered = Some(first.is_some());
                list_items.clear();
            }
            Event::End(TagEnd::List(_)) => {
                blocks.push(Block::List {
                    ordered: list_ordered.take().unwrap_or(false),
                    items: std::mem::take(&mut list_items),
                });
            }
            Event::Start(Tag::Item) => {
                in_item = true;
                item_text.clear();
            }
            Event::End(TagEnd::Item) => {
                in_item = false;
                list_items.push(std::mem::take(&mut item_text));
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, body)) = code.as_mut() {
                    body.push_str(&t);
                } else if in_item {
                    item_text.push_str(&t);
                } else if in_paragraph || heading_level.is_some() {
                    text.push_str(&t);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some((_, body)) = code.as_mut() {
                    body.push('\n');
                } else if in_item {
                    item_text.push(' ');
                } else if in_paragraph {
                    text.push(' ');
                }
            }
            _ => {}
        }
    }

    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_heading_and_paragraph() {
        let blocks = parse("# Title\n\nHello **world**.");
        assert_eq!(
            blocks,
            vec![
                Block::Heading {
                    level: 1,
                    text: "Title".into()
                },
                Block::Paragraph {
                    text: "Hello world.".into()
                },
            ]
        );
    }

    #[test]
    fn fenced_code_block_yields_code_with_language() {
        let blocks = parse("```rust\nfn main() {}\n```");
        assert_eq!(
            blocks,
            vec![Block::Code {
                language: Some("rust".into()),
                code: "fn main() {}".into(),
            }]
        );
    }

    #[test]
    fn bullet_list_yields_items() {
        let blocks = parse("- one\n- two\n- three");
        assert_eq!(
            blocks,
            vec![Block::List {
                ordered: false,
                items: vec!["one".into(), "two".into(), "three".into()],
            }]
        );
    }

    #[test]
    fn ordered_list_detected() {
        let blocks = parse("1. first\n2. second");
        match &blocks[0] {
            Block::List { ordered, items } => {
                assert!(ordered);
                assert_eq!(items.len(), 2);
            }
            other => panic!("expected list, got {other:?}"),
        }
    }

    #[test]
    fn inline_code_preserved_as_text() {
        let blocks = parse("use the `cargo build` command");
        assert_eq!(
            blocks,
            vec![Block::Paragraph {
                text: "use the cargo build command".into()
            }]
        );
    }

    #[test]
    fn mixed_document_block_count() {
        let md = "# H\n\npara\n\n```\ncode\n```\n\n- a\n- b";
        let blocks = parse(md);
        assert_eq!(blocks.len(), 4);
        assert!(matches!(blocks[0], Block::Heading { .. }));
        assert!(matches!(blocks[1], Block::Paragraph { .. }));
        assert!(matches!(blocks[2], Block::Code { .. }));
        assert!(matches!(blocks[3], Block::List { .. }));
    }
}

//! One Markdown interpretation shared by desktop IPC and authenticated remote clients.
//!
//! The wire model deliberately contains text and presentation intent rather than HTML.  A phone
//! can therefore render it with native widgets without trusting a WebView, while the desktop can
//! still request sanitized HTML from the same parser for its existing preview surface.

use comrak::nodes::{ListType, Node, NodeValue, TableAlignment};
use comrak::{parse_document, Arena, Options};
use serde::Serialize;

pub const MAX_MARKDOWN_BYTES: usize = 512 * 1024;
const MAX_BLOCKS: usize = 4096;
const MAX_SPANS: usize = 32_768;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MarkdownDocument {
    pub blocks: Vec<MarkdownBlock>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MarkdownBlock {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ordered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub depth: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<MarkdownSpan>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<MarkdownRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub align: Vec<String>,
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MarkdownRow {
    pub header: bool,
    pub cells: Vec<Vec<MarkdownSpan>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MarkdownSpan {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub code: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

#[derive(Clone, Default)]
struct Marks {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    href: Option<String>,
    image: Option<String>,
}

pub fn parse(source: &str) -> Result<MarkdownDocument, &'static str> {
    if source.len() > MAX_MARKDOWN_BYTES {
        return Err("markdown.too_large");
    }
    let arena = Arena::new();
    let options = markdown_options();
    let root = parse_document(&arena, source, &options);
    let mut output = MarkdownDocument::default();
    for child in root.children() {
        visit_block(child, 0, false, &mut output)?;
    }
    let span_count: usize = output
        .blocks
        .iter()
        .map(|block| {
            block.spans.len()
                + block
                    .rows
                    .iter()
                    .flat_map(|row| &row.cells)
                    .map(Vec::len)
                    .sum::<usize>()
        })
        .sum();
    if output.blocks.len() > MAX_BLOCKS || span_count > MAX_SPANS {
        return Err("markdown.too_complex");
    }
    Ok(output)
}

pub fn to_safe_html(source: &str) -> Result<String, String> {
    if source.len() > MAX_MARKDOWN_BYTES {
        return Err("Markdown preview is larger than 512 KB".into());
    }
    Ok(comrak::markdown_to_html(source, &markdown_options()))
}

#[tauri::command]
pub async fn render_markdown(source: String) -> Result<String, String> {
    crate::run_blocking(move || to_safe_html(&source)).await
}

fn markdown_options<'a>() -> Options<'a> {
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.autolink = true;
    options.extension.footnotes = true;
    // Raw project HTML is never passed through. Links and images remain ordinary attributes in
    // desktop output, and remote clients receive them as bounded strings in the typed model.
    options.render.r#unsafe = false;
    options
}

fn visit_block(
    node: Node<'_>,
    depth: u8,
    quoted: bool,
    output: &mut MarkdownDocument,
) -> Result<(), &'static str> {
    if output.blocks.len() >= MAX_BLOCKS {
        return Err("markdown.too_complex");
    }
    match &node.data().value {
        NodeValue::Paragraph => output.blocks.push(text_block(
            if quoted { "quote" } else { "paragraph" },
            node,
            None,
            depth,
        )),
        NodeValue::Heading(heading) => {
            output
                .blocks
                .push(text_block("heading", node, Some(heading.level), depth))
        }
        NodeValue::CodeBlock(code) => output.blocks.push(MarkdownBlock {
            kind: "code".into(),
            language: code
                .info
                .split_whitespace()
                .next()
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            depth,
            spans: vec![MarkdownSpan {
                text: code.literal.trim_end_matches('\n').to_owned(),
                code: true,
                ..Default::default()
            }],
            ..Default::default()
        }),
        NodeValue::ThematicBreak => output.blocks.push(MarkdownBlock {
            kind: "rule".into(),
            ..Default::default()
        }),
        NodeValue::BlockQuote | NodeValue::MultilineBlockQuote(_) | NodeValue::Alert(_) => {
            for child in node.children() {
                visit_block(child, depth, true, output)?;
            }
        }
        NodeValue::List(list) => {
            let ordered = list.list_type == ListType::Ordered;
            let mut number = u64::try_from(list.start).unwrap_or(1);
            for item in node.children() {
                visit_list_item(item, depth, ordered, number, output)?;
                number = number.saturating_add(1);
            }
        }
        NodeValue::Table(table) => {
            let mut rows = Vec::new();
            for row in node.children() {
                let header = matches!(row.data().value, NodeValue::TableRow(true));
                let cells = row.children().map(inline_spans).collect();
                rows.push(MarkdownRow { header, cells });
            }
            output.blocks.push(MarkdownBlock {
                kind: "table".into(),
                rows,
                align: table
                    .alignments
                    .iter()
                    .map(|alignment| {
                        match alignment {
                            TableAlignment::Center => "center",
                            TableAlignment::Right => "right",
                            _ => "left",
                        }
                        .to_owned()
                    })
                    .collect(),
                ..Default::default()
            });
        }
        // Preserve unsupported block content as readable text rather than dropping it.
        NodeValue::HtmlBlock(html) => output.blocks.push(MarkdownBlock {
            kind: "code".into(),
            spans: vec![MarkdownSpan {
                text: html.literal.clone(),
                code: true,
                ..Default::default()
            }],
            ..Default::default()
        }),
        _ => {
            for child in node.children() {
                visit_block(child, depth, quoted, output)?;
            }
        }
    }
    Ok(())
}

fn visit_list_item(
    item: Node<'_>,
    depth: u8,
    ordered: bool,
    number: u64,
    output: &mut MarkdownDocument,
) -> Result<(), &'static str> {
    let checked = item.descendants().find_map(|node| match node.data().value {
        NodeValue::TaskItem(task) => Some(task.symbol.is_some()),
        _ => None,
    });
    let content = item.children().find(|child| {
        matches!(
            child.data().value,
            NodeValue::Paragraph | NodeValue::TaskItem(_)
        )
    });
    if let Some(content) = content {
        output.blocks.push(MarkdownBlock {
            kind: "list_item".into(),
            ordered,
            number: ordered.then_some(number),
            checked,
            depth,
            spans: inline_spans(content),
            ..Default::default()
        });
    }
    for child in item.children() {
        if matches!(child.data().value, NodeValue::List(_)) {
            visit_block(child, depth.saturating_add(1).min(12), false, output)?;
        }
    }
    Ok(())
}

fn text_block(kind: &str, node: Node<'_>, level: Option<u8>, depth: u8) -> MarkdownBlock {
    MarkdownBlock {
        kind: kind.into(),
        level,
        depth,
        spans: inline_spans(node),
        ..Default::default()
    }
}

fn inline_spans(node: Node<'_>) -> Vec<MarkdownSpan> {
    let mut spans = Vec::new();
    collect_inline(node, &Marks::default(), &mut spans);
    spans
}

fn collect_inline(node: Node<'_>, marks: &Marks, output: &mut Vec<MarkdownSpan>) {
    let mut next = marks.clone();
    match &node.data().value {
        NodeValue::Text(text) => push_span(output, text, &next),
        NodeValue::Code(code) => {
            next.code = true;
            push_span(output, &code.literal, &next);
        }
        NodeValue::SoftBreak => push_span(output, " ", &next),
        NodeValue::LineBreak => push_span(output, "\n", &next),
        NodeValue::Emph => next.italic = true,
        NodeValue::Strong => next.bold = true,
        NodeValue::Strikethrough => next.strike = true,
        NodeValue::Link(link) => next.href = bounded_target(&link.url),
        NodeValue::Image(link) => next.image = bounded_target(&link.url),
        NodeValue::HtmlInline(html) => push_span(output, html, &next),
        NodeValue::TaskItem(_) => {}
        _ => {}
    }
    for child in node.children() {
        collect_inline(child, &next, output);
    }
}

fn bounded_target(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 4096).then(|| value.to_owned())
}

fn push_span(output: &mut Vec<MarkdownSpan>, text: &str, marks: &Marks) {
    if text.is_empty() || output.len() >= MAX_SPANS {
        return;
    }
    let span = MarkdownSpan {
        text: text.to_owned(),
        bold: marks.bold,
        italic: marks.italic,
        strike: marks.strike,
        code: marks.code,
        href: marks.href.clone(),
        image: marks.image.clone(),
    };
    if let Some(previous) = output.last_mut() {
        if previous.bold == span.bold
            && previous.italic == span.italic
            && previous.strike == span.strike
            && previous.code == span.code
            && previous.href == span.href
            && previous.image == span.image
        {
            previous.text.push_str(&span.text);
            return;
        }
    }
    output.push(span);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_native_blocks_for_gfm() {
        let document = parse("# Notes\n\n- [x] done\n- **bold**\n\n| A | B |\n| :- | -: |\n| 1 | 2 |\n\n```rust\nfn main() {}\n```\n").unwrap();
        assert_eq!(document.blocks[0].kind, "heading");
        assert_eq!(document.blocks[0].level, Some(1));
        assert_eq!(document.blocks[1].checked, Some(true));
        assert!(document.blocks[2].spans.iter().any(|span| span.bold));
        assert_eq!(document.blocks[3].kind, "table");
        assert_eq!(document.blocks[3].rows.len(), 2);
        assert_eq!(document.blocks[4].language.as_deref(), Some("rust"));
    }

    #[test]
    fn raw_html_is_not_rendered_by_desktop() {
        let html = to_safe_html("<script>alert(1)</script>\n\n**safe**").unwrap();
        assert!(!html.contains("<script>"));
        assert!(html.contains("<strong>safe</strong>"));
    }
}

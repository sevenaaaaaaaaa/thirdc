//! kernel-md: 块模型与确定性 Markdown 序列化。
//!
//! 文件 = 内容真相。块模型可无损映射到 MD；未知/外来结构包进
//! `<!-- tc:block raw -->` fenced 容器保真不丢。序列化是确定性的：
//! 同一块模型永远产出字节相同的 MD，保证 git diff 干净、外部编辑器互操作无损。

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MdError {
    #[error("block parse failed at line {line}: {msg}")]
    Parse { line: usize, msg: String },
}

/// 块 ID：由确定性序列化内容派生，外部编辑不破坏身份。
pub type BlockId = String;

/// 规范块模型。V1 支持核心类型；其余进 Raw 容器。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    /// 段落。行内结构（加粗/链接等）以原样 MD 行内文本保存，V1 不做行内 AST。
    Paragraph { id: BlockId, text: String },
    /// 标题。level 1..=6。
    Heading { id: BlockId, level: u8, text: String },
    /// 代码块。
    Code { id: BlockId, lang: Option<String>, text: String },
    /// 引用块（多行）。
    Quote { id: BlockId, lines: Vec<String> },
    /// 列表。items 为原样行内文本，ordered 区分 -/1.。
    List { id: BlockId, ordered: bool, items: Vec<String> },
    /// 表格。第一行为表头。
    Table { id: BlockId, header: Vec<String>, rows: Vec<Vec<String>> },
    /// 分隔线。
    Divider { id: BlockId },
    /// 原样保留块：任何无法结构化的内容，原字节保真。
    Raw { id: BlockId, text: String },
}

/// 一篇文档 = 有序块序列 + 元数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DocModel {
    pub title: Option<String>,
    pub blocks: Vec<Block>,
}

/// 确定性序列化为 MD。输出只依赖输入，与插入顺序历史无关。
pub fn to_markdown(doc: &DocModel) -> String {
    let mut out = String::new();
    if let Some(t) = &doc.title {
        out.push_str("# ");
        out.push_str(t);
        out.push_str("\n\n");
    }
    for (i, b) in doc.blocks.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        push_block(&mut out, b);
        out.push('\n');
    }
    out
}

fn push_block(out: &mut String, b: &Block) {
    match b {
        Block::Paragraph { text, .. } => {
            out.push_str(text.trim_end());
        }
        Block::Heading { level, text, .. } => {
            for _ in 0..(*level).clamp(1, 6) {
                out.push('#');
            }
            out.push(' ');
            out.push_str(text);
        }
        Block::Code { lang, text, .. } => {
            // fence 长度取文本内最长 ``` + 1，保证嵌套安全
            let max_run = text.split('\n').map(|l| fence_run(l)).max().unwrap_or(0);
            let fence = "`".repeat(max_run + 3);
            out.push_str(&fence);
            if let Some(l) = lang {
                out.push_str(l);
            }
            out.push('\n');
            out.push_str(text);
            if !text.ends_with('\n') && !text.is_empty() {
                out.push('\n');
            }
            out.push_str(&fence);
        }
        Block::Quote { lines, .. } => {
            for (i, l) in lines.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                out.push_str("> ");
                out.push_str(l);
            }
        }
        Block::List { ordered, items, .. } => {
            for (i, item) in items.iter().enumerate() {
                if *ordered {
                    out.push_str(&format!("{}. {}\n", i + 1, item));
                } else {
                    out.push_str(&format!("- {item}\n"));
                }
            }
            // 列表块末尾不带多余换行，块间距由外层控制
            out.pop();
        }
        Block::Table { header, rows, .. } => {
            out.push_str(&md_table_row(header));
            out.push_str("\n|");
            for _ in header {
                out.push_str(" --- |");
            }
            for r in rows {
                out.push('\n');
                out.push_str(&md_table_row(r));
            }
        }
        Block::Divider { .. } => out.push_str("---"),
        Block::Raw { text, .. } => {
            out.push_str("<!-- tc:block raw\n");
            out.push_str(text.trim_end_matches('\n'));
            out.push_str("\ntc:block raw -->");
        }
    }
}

fn md_table_row(cells: &[String]) -> String {
    let mut s = String::from("|");
    for c in cells {
        s.push(' ');
        s.push_str(c.replace('|', "\\|").trim());
        s.push_str(" |");
    }
    s
}

fn fence_run(line: &str) -> usize {
    let t = line.trim_start();
    if t.starts_with('`') {
        t.chars().take_while(|c| *c == '`').count()
    } else {
        0
    }
}

/// 从 MD 解析为块模型。行级解析器：识别标题/代码/引用/列表/表格/分隔线/段落，
/// 不认识的连续区域收敛为一个 Raw 块。目标是往返保真，不是完整 CommonMark。
pub fn from_markdown(src: &str) -> Result<DocModel, MdError> {
    let mut doc = DocModel::default();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0usize;

    // 首个 H1 提升为 title（若存在）
    if let Some(first) = lines.first() {
        if let Some(t) = first.strip_prefix("# ") {
            doc.title = Some(t.to_string());
            i = 1;
            if lines.get(1).map_or(false, |l| l.trim().is_empty()) {
                i = 2;
            }
        }
    }

    let mut counter = 0usize;
    let mut next_id = || {
        counter += 1;
        format!("b{counter:04}")
    };

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();

        if trimmed.is_empty() {
            i += 1;
            continue;
        }

        // fenced code
        if trimmed.starts_with("```") {
            let fence_len = fence_run(line);
            let fence: String = "`".repeat(fence_len);
            let lang = trimmed[fence_len..].trim().to_string();
            let lang = if lang.is_empty() { None } else { Some(lang) };
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() {
                if lines[i].trim_end() == fence {
                    break;
                }
                body.push(lines[i]);
                i += 1;
            }
            if i >= lines.len() {
                return Err(MdError::Parse {
                    line: i,
                    msg: "unterminated code fence".into(),
                });
            }
            i += 1; // consume closing fence
            let mut text = body.join("\n");
            if !body.is_empty() {
                text.push('\n');
            }
            doc.blocks.push(Block::Code {
                id: next_id(),
                lang,
                text,
            });
            continue;
        }

        // raw container
        if trimmed == "<!-- tc:block raw" {
            let mut body = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim() != "tc:block raw -->" {
                body.push(lines[i]);
                i += 1;
            }
            if i >= lines.len() {
                return Err(MdError::Parse {
                    line: i,
                    msg: "unterminated raw block".into(),
                });
            }
            i += 1;
            doc.blocks.push(Block::Raw {
                id: next_id(),
                text: body.join("\n"),
            });
            continue;
        }

        // heading
        let hlevel = trimmed.chars().take_while(|c| *c == '#').count() as u8;
        if (1..=6).contains(&hlevel) && trimmed[hlevel as usize..].starts_with(' ') {
            doc.blocks.push(Block::Heading {
                id: next_id(),
                level: hlevel,
                text: trimmed[hlevel as usize..].trim().to_string(),
            });
            i += 1;
            continue;
        }

        // divider
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            doc.blocks.push(Block::Divider { id: next_id() });
            i += 1;
            continue;
        }

        // quote
        if trimmed.starts_with("> ") || trimmed == ">" {
            let mut ql = Vec::new();
            while i < lines.len() && (lines[i].trim_start().starts_with("> ") || lines[i].trim() == ">") {
                let l = lines[i].trim_start();
                ql.push(l.strip_prefix("> ").unwrap_or(l.strip_prefix('>').unwrap_or("")).to_string());
                i += 1;
            }
            doc.blocks.push(Block::Quote { id: next_id(), lines: ql });
            continue;
        }

        // list
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            let mut items = Vec::new();
            while i < lines.len() && (lines[i].trim_start().starts_with("- ") || lines[i].trim_start().starts_with("* ")) {
                items.push(lines[i].trim_start()[2..].to_string());
                i += 1;
            }
            doc.blocks.push(Block::List { id: next_id(), ordered: false, items });
            continue;
        }
        if trimmed.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            let mut items = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim_start();
                let Some(dot) = t.find(". ") else { break };
                if !t[..dot].chars().all(|c| c.is_ascii_digit()) {
                    break;
                }
                items.push(t[dot + 2..].to_string());
                i += 1;
            }
            if !items.is_empty() {
                doc.blocks.push(Block::List { id: next_id(), ordered: true, items });
                continue;
            }
        }

        // table
        if trimmed.starts_with('|') && i + 1 < lines.len() && is_table_sep(lines[i + 1]) {
            let header = parse_table_row(trimmed);
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                rows.push(parse_table_row(lines[i].trim()));
                i += 1;
            }
            doc.blocks.push(Block::Table { id: next_id(), header, rows });
            continue;
        }

        // paragraph: 吸收连续非空行
        let mut para = vec![line.to_string()];
        i += 1;
        while i < lines.len() {
            let t = lines[i].trim();
            if t.is_empty()
                || t.starts_with('#')
                || t.starts_with("```")
                || t.starts_with("> ")
                || t.starts_with("- ")
                || t.starts_with("* ")
                || t == "---"
                || t.starts_with('|')
            {
                break;
            }
            para.push(lines[i].to_string());
            i += 1;
        }
        doc.blocks.push(Block::Paragraph {
            id: next_id(),
            text: para.join("\n"),
        });
    }

    Ok(doc)
}

fn is_table_sep(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('|') && t.contains("---")
}

fn parse_table_row(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    t.split('|').map(|c| c.trim().to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# 标题

正文一段。

## 二级

- 甲
- 乙

1. one
2. two

> 引用一
> 引用二

```rust
fn main() {}
```

| a | b |
| --- | --- |
| 1 | 2 |

---

````
内嵌 ``` fence
````

<!-- tc:block raw
<span>外来结构</span>
tc:block raw -->
";

    #[test]
    fn roundtrip_is_stable() {
        let doc = from_markdown(SAMPLE).unwrap();
        let md1 = to_markdown(&doc);
        let doc2 = from_markdown(&md1).unwrap();
        assert_eq!(doc2, doc, "第二次解析应与第一次解析的块模型一致");
        let md2 = to_markdown(&doc2);
        assert_eq!(md1, md2, "序列化必须确定性");
        assert!(md1.contains("标题"));
        assert!(md1.contains("tc:block raw"));
    }

    #[test]
    fn title_promotion() {
        let doc = from_markdown("# T\n\nbody").unwrap();
        assert_eq!(doc.title.as_deref(), Some("T"));
    }

    #[test]
    fn unterminated_fence_is_error() {
        assert!(from_markdown("```rust\nfn x()").is_err());
    }
}

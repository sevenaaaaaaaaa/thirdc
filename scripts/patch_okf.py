import re

# ═══ kernel-md：frontmatter + ^锚点（Open Knowledge Format 基座） ═══
p = 'kernel/crates/kernel-md/src/lib.rs'
s = open(p, encoding='utf-8').read()

# DocModel 加 meta + anchors
s = s.replace('''#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DocModel {
    pub title: Option<String>,
    pub blocks: Vec<Block>,
}''', '''#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DocModel {
    pub title: Option<String>,
    pub blocks: Vec<Block>,
    /// YAML frontmatter（OKF：机器可读的元数据层）
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub meta: std::collections::BTreeMap<String, String>,
    /// `^锚点`（按块顺序；物化时按序回填，全局唯一块引用的根）
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<String>,
}''')

# to_markdown：frontmatter 头 + 锚点回填
s = s.replace('''pub fn to_markdown(doc: &DocModel) -> String {
    let mut out = String::new();''', '''pub fn to_markdown(doc: &DocModel) -> String {
    let mut out = String::new();
    if !doc.meta.is_empty() {
        out.push_str("---\\n");
        for (k, v) in &doc.meta {
            out.push_str(&format!("{k}: {v}\\n"));
        }
        out.push_str("---\\n\\n");
    }''')
s = s.replace('''    for (i, b) in doc.blocks.iter().enumerate() {
        if i > 0 {
            out.push('\\n');
        }
        push_block(&mut out, b);
        out.push('\\n');
    }
    out
}''', '''    for (i, b) in doc.blocks.iter().enumerate() {
        if i > 0 {
            out.push('\\n');
        }
        push_block(&mut out, b);
        // 锚点回填：块级末尾 ^id（Obsidian 兼容写法）
        if let Some(a) = doc.anchors.get(i) {
            out.push_str(&format!(" ^{a}"));
        }
        out.push('\\n');
    }
    out
}''')

# from_markdown：frontmatter 解析 + ^锚点抽取
s = s.replace('''pub fn from_markdown(src: &str) -> Result<DocModel, MdError> {
    let mut doc = DocModel::default();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0usize;''', '''pub fn from_markdown(src: &str) -> Result<DocModel, MdError> {
    let mut doc = DocModel::default();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0usize;

    // ── YAML frontmatter ──
    if lines.first().map_or(false, |l| l.trim() == "---") {
        i = 1;
        while i < lines.len() && lines[i].trim() != "---" {
            if let Some((k, v)) = lines[i].split_once(':') {
                let key = k.trim().to_string();
                let val = v.trim().trim_matches('"').trim_matches('\\'').to_string();
                if !key.is_empty() {
                    doc.meta.insert(key, val);
                }
            }
            i += 1;
        }
        i += 1; // 收尾 ---
        if lines.get(i).map_or(false, |l| l.trim().is_empty()) {
            i += 1;
        }
    }''')

# 块结束处抽取 ^锚点（每个 push 的段落/标题等都会经过这里 → 统一在 push 前处理太散，
# 改为：解析完后按块扫描文本中的尾部 ^锚点）
s = s.replace('''    assign_stable_ids(&mut doc);
    Ok(doc)
}''', '''    // ── ^锚点：从块文本尾部抽取（每种块类型最后一行）──
    for b in doc.blocks.iter_mut() {
        let extracted = extract_anchor(b);
        if let Some(a) = extracted {
            doc.anchors.push(a);
        }
    }
    assign_stable_ids(&mut doc);
    Ok(doc)
}

/// 抽取块末尾的 `^锚点`（写在文本里时），返回锚点名并把文本剥离。
fn extract_anchor(b: &mut Block) -> Option<String> {
    let text_mut = match b {
        Block::Paragraph { text, .. } | Block::Heading { text, .. } => text,
        Block::Code { text, .. } | Block::Raw { text, .. } => text,
        _ => return None,
    };
    let t = text_mut.trim_end();
    if let Some(sp) = t.rfind(" ^") {
        let name = &t[sp + 2..];
        let ok = !name.is_empty()
            && name.len() <= 40
            && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_');
        if ok {
            let a = name.to_string();
            *text_mut = t[..sp].trim_end().to_string();
            return Some(a);
        }
    }
    None
}''')

# 测试
s = s.replace('''    #[test]
    fn html_roundtrip_is_stable() {''', '''    #[test]
    fn okf_frontmatter_and_anchors_roundtrip() {
        let src = "---\\ntitle: 知识包\\nsource: example.com\\n---\\n\\n# 标题\\n\\n正文段落。 ^core-1\\n\\n第二段\\n";
        let doc = from_markdown(src).unwrap();
        assert_eq!(doc.meta.get("title").map(|s| s.as_str()), Some("知识包"));
        assert_eq!(doc.meta.get("source").map(|s| s.as_str()), Some("example.com"));
        assert_eq!(doc.anchors, vec!["core-1"]);
        // 往返：frontmatter 与锚点都保留
        let md = to_markdown(&doc);
        assert!(md.starts_with("---\\ntitle: 知识包\\nsource: example.com\\n---"));
        assert!(md.contains("正文段落。 ^core-1"));
        let back = from_markdown(&md).unwrap();
        assert_eq!(back.meta.get("title").map(|s| s.as_str()), Some("知识包"));
        assert_eq!(back.anchors, vec!["core-1"]);
        // frontmatter 不再被当成 Divider
        assert!(!doc.blocks.iter().any(|b| matches!(b, Block::Divider { .. })));
    }

    #[test]
    fn html_roundtrip_is_stable() {''')
open(p, 'w', encoding='utf-8').write(s)
print('okf base done')

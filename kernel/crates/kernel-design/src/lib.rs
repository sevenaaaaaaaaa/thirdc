//! kernel-design: 把设计规范（design.md / SKILL.md / tokens.css / 页面 HTML）
//! 解析成 DesignProfile，并存为可重建的 sidecar 对象。
//!
//! 设计契约是内容，不是配置：导入后可被检索、被 agent 读取，并直接决定渲染结果。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DesignError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("empty profile: no tokens/fonts/rules found in {0}")]
    Empty(String),
    #[error("not found: {0}")]
    NotFound(String),
}

/// 组件零件（类名 + 用途 + 可选样式片段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Archetype {
    pub name: String,
    pub usage: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub css: String,
}

/// 一份设计规范。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DesignProfile {
    pub name: String,
    /// design-md | css | html-digest | skill-dir
    pub kind: String,
    /// CSS 变量：基础作用域（通常为亮色）
    pub tokens: BTreeMap<String, String>,
    /// 暗色作用域变量（`[data-theme="dark"]` / `.dark`）
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tokens_dark: BTreeMap<String, String>,
    /// 字体族，按出现顺序
    pub fonts: Vec<String>,
    /// 硬规则（零 hex、图标规格、圆角档位…）
    pub rules: Vec<String>,
    /// 组件零件
    pub archetypes: Vec<Archetype>,
    /// 来源路径或 URL
    pub source: String,
}

impl DesignProfile {
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty() && self.fonts.is_empty() && self.rules.is_empty()
    }

    /// 生成注入用的 CSS 变量块（`:root{...}` + 暗色作用域 + 组件片段）。
    pub fn to_css(&self) -> String {
        let mut css = String::from(":root{\n");
        for (k, v) in &self.tokens {
            css.push_str(&format!("  {k}: {v};\n"));
        }
        if let Some(f) = self.fonts.first() {
            css.push_str(&format!("  --of-profile-font: {f};\n"));
        }
        css.push_str("}\n");
        if !self.tokens_dark.is_empty() {
            css.push_str("[data-theme=\"dark\"]{\n");
            for (k, v) in &self.tokens_dark {
                css.push_str(&format!("  {k}: {v};\n"));
            }
            css.push_str("}\n");
        }
        for a in &self.archetypes {
            if !a.css.is_empty() {
                css.push_str(&a.css);
                if !a.css.ends_with('\n') {
                    css.push('\n');
                }
            }
        }
        css
    }

    /// 摘要（给 agent / UI 看）。
    pub fn summary(&self) -> String {
        let mut s = format!("设计规范「{}」（{}）\n", self.name, self.kind);
        if !self.fonts.is_empty() {
            s.push_str(&format!("字体：{}\n", self.fonts.join(" / ")));
        }
        s.push_str(&format!("token {} 个，组件 {} 个，规则 {} 条\n", self.tokens.len(), self.archetypes.len(), self.rules.len()));
        for a in self.archetypes.iter().take(24) {
            s.push_str(&format!("- {} — {}\n", a.name, a.usage));
        }
        for r in self.rules.iter().take(24) {
            s.push_str(&format!("· {r}\n"));
        }
        s
    }
}


/// token 里名含 font 的值，抽出字体族（tokens.css 常见写法）。
pub fn harvest_font_tokens(tokens: &BTreeMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (k, v) in tokens {
        if !k.to_lowercase().contains("font") {
            continue;
        }
        let first = v.split(',').next().unwrap_or(v).trim().trim_matches('"').trim_matches('\'').trim();
        if !first.is_empty()
            && !first.starts_with("var(")
            && !first.chars().all(|c| c.is_ascii_digit())
            && !out.iter().any(|x: &String| x.eq_ignore_ascii_case(first))
        {
            out.push(first.to_string());
        }
    }
    out
}

/// 组件数量上限：避免把整个 CSS 文件的每条选择器都收进来。
pub const MAX_ARCHETYPES: usize = 200;

fn cap_archetypes(mut list: Vec<Archetype>) -> Vec<Archetype> {
    list.truncate(MAX_ARCHETYPES);
    list
}

/// 去除块注释。必须按字节游标推进：注释里可能有中文，
/// 用「字节偏移」去跳「字符数」会多吃掉注释之后的声明。
pub fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    while i < text.len() {
        if text[i..].starts_with("/*") {
            match text[i + 2..].find("*/") {
                Some(e) => {
                    i = i + 2 + e + 2;
                    continue;
                }
                None => break,
            }
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// 从 CSS 文本中分离暗色作用域块（`[data-theme="dark"]` / `.dark`），
/// 返回 (其余文本, 暗色块内部文本)。规范常同时定义亮暗两套，不能压平。
pub fn split_dark(text: &str) -> (String, String) {
    let markers = ["[data-theme=\"dark\"]", "[data-theme='dark']", ".dark{"];
    for m in markers {
        if let Some(i) = text.find(m) {
            let after = &text[i..];
            let Some(brace) = after.find('{') else { continue };
            let start = i + brace + 1;
            // 花括号配平
            let bytes = text.as_bytes();
            let mut depth = 1i32;
            let mut j = start;
            while j < bytes.len() {
                match bytes[j] {
                    b'{' => depth += 1,
                    b'}' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
            if depth != 0 {
                continue;
            }
            let dark = text[start..j].to_string();
            let mut base = String::with_capacity(text.len());
            base.push_str(&text[..i]);
            base.push_str(&text[(j + 1).min(text.len())..]);
            return (base, dark);
        }
    }
    (text.to_string(), String::new())
}

/// 抽取 `--name: value;`（容忍换行与注释）。
pub fn parse_css_vars(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let cleaned = strip_comments(text);
    let bytes = cleaned.as_bytes();
    let mut i = 0usize;
    while i + 2 < bytes.len() {
        if bytes[i] == b'-' && bytes[i + 1] == b'-' {
            let start = i + 2;
            let mut j = start;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-' || bytes[j] == b'_') {
                j += 1;
            }
            if j > start && cleaned[j..].starts_with(':') {
                let val_start = j + 1;
                let mut k = val_start;
                while k < bytes.len() && bytes[k] != b';' && bytes[k] != b'\n' && bytes[k] != b'}' {
                    k += 1;
                }
                let name = &cleaned[start..j];
                let value = cleaned[val_start..k].trim();
                if !value.is_empty() {
                    out.insert(format!("--{name}"), value.to_string());
                }
                i = k;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// 抽取 font-family / @font-face 的字体族（按出现顺序去重）。
pub fn parse_fonts(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    fn push(s: String, out: &mut Vec<String>) {
        let s = s.trim().trim_matches('"').trim_matches('\'').trim().to_string();
        if !s.is_empty() && !out.iter().any(|x: &String| x.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    }
    for (idx, _) in text.match_indices("font-family") {
        let rest = &text[idx + "font-family".len()..];
        let Some(colon) = rest.find(':') else { continue };
        let after = &rest[colon + 1..];
        let end = after.find([';', '}', '\n']).unwrap_or(after.len());
        let decl = &after[..end];
        // 取第一个族名（逗号前的第一项）
        let first = decl.split(',').next().unwrap_or(decl);
        if !first.contains('(') {
            push(first.to_string(), &mut out);
        }
    }
    out
}

/// 从 markdown 抽规则：引用块与列表项，过滤短噪音。
pub fn parse_rules(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        let item = t
            .strip_prefix("> ")
            .or_else(|| t.strip_prefix("- "))
            .or_else(|| t.strip_prefix("* "))
            .or_else(|| {
                // 有序列表 "1. "
                let (num, rest) = t.split_once(". ")?;
                if num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty() {
                    Some(rest)
                } else {
                    None
                }
            });
        if let Some(item) = item {
            let s = item.trim().trim_matches('*').trim().to_string();
            if s.chars().count() >= 6 && s.chars().count() <= 200 && !out.contains(&s) {
                out.push(s);
            }
        }
    }
    out
}

/// 从 markdown 抽组件：表格行形如 `| `.btn` | 主要按钮 | … |`。
pub fn parse_archetypes_md(text: &str) -> Vec<Archetype> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if !t.starts_with('|') || t.contains("---") {
            continue;
        }
        let cells: Vec<String> = t
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        if cells.len() < 2 {
            continue;
        }
        let first = cells[0].trim_matches('`').trim();
        // 只收看起来像类名/组件的（.x 或 含点或字母序列）
        if first.starts_with('.') || first.contains('.') || first.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
            let usage = cells[1..].iter().filter(|c| !c.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
            if !first.is_empty() && first.chars().count() < 40 && !usage.is_empty() && !out.iter().any(|a: &Archetype| a.name == first) {
                out.push(Archetype { name: first.to_string(), usage, css: String::new() });
            }
        }
    }
    out
}

/// 抽取 fenced code block 内容（```css / ```html / ``` 起）。
pub fn fenced_blocks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let t = line.trim_start();
        if t.starts_with("```") {
            if inside {
                out.push(cur.join("\n"));
                cur.clear();
                inside = false;
            } else {
                inside = true;
            }
            continue;
        }
        if inside {
            cur.push(line);
        }
    }
    if inside && !cur.is_empty() {
        out.push(cur.join("\n"));
    }
    out
}

/// 解析 design.md / SKILL.md。
pub fn parse_design_md(name: &str, source: &str, text: &str) -> DesignProfile {
    let mut tokens = BTreeMap::new();
    let mut tokens_dark = BTreeMap::new();
    let mut archetypes = Vec::new();
    for block in fenced_blocks(text) {
        let (base_text, dark_text) = split_dark(&block);
        for (k, v) in parse_css_vars(&base_text) {
            tokens.insert(k, v);
        }
        for (k, v) in parse_css_vars(&dark_text) {
            tokens_dark.insert(k, v);
        }
        // 以类选择器开头的片段收作组件
        for line in block.lines() {
            let t = line.trim();
            if t.starts_with('.') && t.contains('{') {
                let cls = t.split('{').next().unwrap_or("").trim().trim_end_matches(',').trim().to_string();
                if cls.chars().count() < 60 && !archetypes.iter().any(|a: &Archetype| a.name == cls) {
                    archetypes.push(Archetype { name: cls, usage: "样式片段".into(), css: t.to_string() });
                }
            }
        }
    }
    let mut arch = parse_archetypes_md(text);
    archetypes.append(&mut arch);
    let mut fonts = parse_fonts(text);
    for f in harvest_font_tokens(&tokens) {
        if !fonts.iter().any(|x| x.eq_ignore_ascii_case(&f)) {
            fonts.push(f);
        }
    }
    DesignProfile {
        name: name.to_string(),
        kind: "design-md".into(),
        tokens,
        tokens_dark,
        fonts,
        rules: parse_rules(text),
        archetypes: cap_archetypes(archetypes),
        source: source.to_string(),
    }
}

/// 解析纯 CSS（tokens.css 等）。
pub fn parse_css(name: &str, source: &str, text: &str) -> DesignProfile {
    let mut archetypes = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('.') && t.contains('{') {
            let cls = t.split('{').next().unwrap_or("").trim().to_string();
            if cls.chars().count() < 60 && !archetypes.iter().any(|a: &Archetype| a.name == cls) {
                archetypes.push(Archetype { name: cls, usage: "样式片段".into(), css: t.to_string() });
            }
        }
    }
    let (base_text, dark_text) = split_dark(text);
    let tokens = parse_css_vars(&base_text);
    let tokens_dark = parse_css_vars(&dark_text);
    let mut fonts = parse_fonts(text);
    for f in harvest_font_tokens(&tokens) {
        if !fonts.iter().any(|x| x.eq_ignore_ascii_case(&f)) {
            fonts.push(f);
        }
    }
    DesignProfile {
        name: name.to_string(),
        kind: "css".into(),
        tokens,
        tokens_dark,
        fonts,
        rules: Vec::new(),
        archetypes: cap_archetypes(archetypes),
        source: source.to_string(),
    }
}

/// 消化一个 HTML 页面的排版：token、字体、容器宽、标题层级、组件类词频。
pub fn digest_html(name: &str, source: &str, html: &str) -> DesignProfile {
    // <style> 内容
    let mut style_text = String::new();
    let mut rest = html;
    while let Some(i) = rest.find("<style") {
        let after = &rest[i..];
        let Some(gt) = after.find('>') else { break };
        let body = &after[gt + 1..];
        let Some(end) = body.find("</style>") else { break };
        style_text.push_str(&body[..end]);
        style_text.push('\n');
        rest = &body[end + 8..];
    }
    let (style_base, style_dark) = split_dark(&style_text);
    let mut tokens = parse_css_vars(&style_base);
    let mut tokens_dark = parse_css_vars(&style_dark);
    let (inline_base, inline_dark) = split_dark(html);
    for (k, v) in parse_css_vars(&inline_base) {
        tokens.entry(k).or_insert(v);
    }
    for (k, v) in parse_css_vars(&inline_dark) {
        tokens_dark.entry(k).or_insert(v);
    }
    let fonts = parse_fonts(&format!("{style_text}\n{html}"));

    // 容器宽度
    let mut widths: Vec<String> = Vec::new();
    for (idx, _) in style_text.match_indices("max-width") {
        let after = &style_text[idx + "max-width".len()..];
        let Some(colon) = after.find(':') else { continue };
        let val = after[colon + 1..].split([';', '}', '\n']).next().unwrap_or("").trim();
        if !val.is_empty() && !val.contains("none") && !widths.contains(&val.to_string()) {
            widths.push(val.to_string());
        }
        if widths.len() >= 6 {
            break;
        }
    }

    // 标题层级
    let mut headings: Vec<(String, usize)> = Vec::new();
    for level in 1..=6u8 {
        let tag = format!("<h{level}");
        let n = html.matches(&tag).count();
        if n > 0 {
            headings.push((format!("h{level}"), n));
        }
    }

    // class 词频（取前 12）
    let mut freq: BTreeMap<String, usize> = BTreeMap::new();
    let mut r = html;
    while let Some(i) = r.find("class=\"") {
        let after = &r[i + 7..];
        let Some(end) = after.find('"') else { break };
        for c in after[..end].split_whitespace() {
            if c.len() >= 2 && !c.contains('{') {
                *freq.entry(c.to_string()).or_insert(0) += 1;
            }
        }
        r = &after[end + 1..];
    }
    let mut top: Vec<(String, usize)> = freq.into_iter().filter(|(_, n)| *n >= 2).collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    top.truncate(12);

    let mut rules = Vec::new();
    if let Some(f) = fonts.first() {
        rules.push(format!("主字体：{f}"));
    }
    if !widths.is_empty() {
        rules.push(format!("容器宽度：{}", widths.join(" / ")));
    }
    if !headings.is_empty() {
        rules.push(format!(
            "标题层级：{}",
            headings.iter().map(|(t, n)| format!("{t}×{n}")).collect::<Vec<_>>().join(" ")
        ));
    }
    if !top.is_empty() {
        rules.push(format!(
            "高频组件类：{}",
            top.iter().take(8).map(|(c, _)| c.clone()).collect::<Vec<_>>().join(", ")
        ));
    }
    let archetypes = cap_archetypes(
        top.iter()
            .map(|(c, n)| Archetype {
                name: format!(".{c}"),
                usage: format!("页面出现 {n} 次"),
                css: String::new(),
            })
            .collect(),
    );

    DesignProfile {
        name: name.to_string(),
        kind: "html-digest".into(),
        tokens,
        tokens_dark,
        fonts,
        rules,
        archetypes,
        source: source.to_string(),
    }
}

/// 从路径导入：文件按扩展名分派；目录递归合并（skill 包）。
pub fn import_path(path: &Path, name: Option<&str>) -> Result<DesignProfile, DesignError> {
    let display = path.to_string_lossy().to_string();
    let fallback_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "design".into());
    let name = name.unwrap_or(&fallback_name).to_string();

    if path.is_dir() {
        let mut md = String::new();
        let mut css = String::new();
        let mut html = String::new();
        collect_dir(path, &mut md, &mut css, &mut html, 0)?;
        let mut profile = if !md.is_empty() {
            parse_design_md(&name, &display, &md)
        } else if !css.is_empty() {
            parse_css(&name, &display, &css)
        } else {
            digest_html(&name, &display, &html)
        };
        // 目录内 CSS 的 token 补进来（含暗色作用域）
        let (css_base, css_dark) = split_dark(&css);
        for (k, v) in parse_css_vars(&css_base) {
            profile.tokens.entry(k).or_insert(v);
        }
        for (k, v) in parse_css_vars(&css_dark) {
            profile.tokens_dark.entry(k).or_insert(v);
        }
        if profile.fonts.is_empty() {
            profile.fonts = parse_fonts(&css);
        }
        profile.kind = "skill-dir".into();
        if profile.is_empty() {
            return Err(DesignError::Empty(display));
        }
        return Ok(profile);
    }

    let text = std::fs::read_to_string(path)?;
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    let head = text.trim_start();
    let looks_markup = matches!(ext.as_str(), "html" | "htm" | "php" | "vue" | "jsx" | "tsx")
        || head.starts_with("<!doctype")
        || head.starts_with("<!DOCTYPE")
        || head.starts_with("<?php")
        || head.starts_with("<html");
    let profile = match ext.as_str() {
        "css" => parse_css(&name, &display, &text),
        _ if looks_markup => digest_html(&name, &display, &text),
        _ => parse_design_md(&name, &display, &text),
    };
    if profile.is_empty() {
        return Err(DesignError::Empty(display));
    }
    Ok(profile)
}

fn collect_dir(dir: &Path, md: &mut String, css: &mut String, html: &mut String, depth: usize) -> Result<(), DesignError> {
    const MAX: usize = 512 * 1024;
    if depth > 4 {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)?.filter_map(|e| e.ok()) {
        let p = entry.path();
        let fname = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if fname.starts_with('.') || fname == "node_modules" || fname == "vendor" {
            continue;
        }
        if p.is_dir() {
            collect_dir(&p, md, css, html, depth + 1)?;
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        if !matches!(ext.as_str(), "md" | "css" | "html" | "htm") {
            continue;
        }
        if let Ok(meta) = p.metadata() {
            if meta.len() as usize > MAX {
                continue;
            }
        }
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        match ext.as_str() {
            "md" => {
                md.push_str(&text);
                md.push('\n');
            }
            "css" => {
                css.push_str(&text);
                css.push('\n');
            }
            _ => {
                html.push_str(&text);
                html.push('\n');
            }
        }
    }
    Ok(())
}

/// 设计规范的 sidecar 存储（可重建）。
pub struct DesignStore {
    dir: PathBuf,
}

impl DesignStore {
    pub fn open(sidecar: &Path) -> Result<Self, DesignError> {
        let dir = sidecar.join("design");
        std::fs::create_dir_all(&dir)?;
        Ok(DesignStore { dir })
    }

    fn slug(name: &str) -> String {
        let s: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
            .collect();
        let s = s.trim_matches('-').to_string();
        if s.is_empty() { "design".into() } else { s }
    }

    pub fn save(&self, p: &DesignProfile) -> Result<(), DesignError> {
        let path = self.dir.join(format!("{}.json", Self::slug(&p.name)));
        std::fs::write(path, serde_json::to_string_pretty(p)?)?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<DesignProfile>, DesignError> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(&self.dir)?.filter_map(|e| e.ok()) {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".json") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                if let Ok(p) = serde_json::from_str::<DesignProfile>(&text) {
                    out.push(p);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get(&self, name: &str) -> Result<DesignProfile, DesignError> {
        let path = self.dir.join(format!("{}.json", Self::slug(name)));
        let text = std::fs::read_to_string(&path).map_err(|_| DesignError::NotFound(name.to_string()))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn set_active(&self, name: &str) -> Result<(), DesignError> {
        self.get(name)?; // 必须存在
        std::fs::write(self.dir.join("active"), Self::slug(name))?;
        Ok(())
    }

    pub fn active(&self) -> Result<Option<DesignProfile>, DesignError> {
        let p = self.dir.join("active");
        if !p.is_file() {
            return Ok(None);
        }
        let slug = std::fs::read_to_string(p)?;
        let slug = slug.trim();
        if slug.is_empty() {
            return Ok(None);
        }
        match self.get(slug) {
            Ok(x) => Ok(Some(x)),
            Err(_) => Ok(None),
        }
    }

    pub fn clear_active(&self) -> Result<(), DesignError> {
        let p = self.dir.join("active");
        if p.is_file() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
}

/// 把规范注入到已渲染的 HTML（作用于 `<head>` 末尾，可覆盖默认样式）。
pub fn apply_to_html(html: &str, profile: &DesignProfile) -> String {
    let css = profile.to_css();
    let inject = format!(
        "<style id=\"design-profile\" data-profile=\"{}\">{}</style>\n",
        profile.name.replace('"', ""),
        css
    );
    match html.find("</head>") {
        Some(i) => {
            let mut out = String::with_capacity(html.len() + inject.len());
            out.push_str(&html[..i]);
            out.push_str(&inject);
            out.push_str(&html[i..]);
            out
        }
        None => format!("{inject}{html}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESIGN_MD: &str = r#"
# OpenFlow 设计系统

> 全 oklch 色彩体系，零 hex；颜色派生用 color-mix。

```css
:root{
  --bg: oklch(96.5% .016 85);
  --accent: oklch(52% .17 258);
  --r-lg: 26px;
  --ease-spring: cubic-bezier(.32,.72,0,1);
}
.btn{display:inline-flex;height:48px;border-radius:12px}
```

- 图标只有一种：24×24 线框 svg，stroke-width 1.8
- 页面私有 CSS 零 hex

| 类 | 用途 |
|---|---|
| `.btn` | 主要按钮 |
| `.card` | 内容卡 |
"#;

    #[test]
    fn parses_design_md() {
        let p = parse_design_md("openflow", "design.md", DESIGN_MD);
        assert_eq!(p.tokens.get("--bg").unwrap(), "oklch(96.5% .016 85)");
        assert_eq!(p.tokens.get("--r-lg").unwrap(), "26px");
        assert!(p.tokens.contains_key("--ease-spring"));
        assert!(p.archetypes.iter().any(|a| a.name == ".btn"));
        assert!(p.archetypes.iter().any(|a| a.name == ".card"));
        assert!(p.rules.iter().any(|r| r.contains("零 hex")));
        assert!(p.rules.iter().any(|r| r.contains("24×24")));
        assert!(!p.is_empty());
    }

    #[test]
    fn css_comments_ignored() {
        let css = "/* --fake: 1; */ :root{--real: 2;}";
        let t = parse_css_vars(css);
        assert!(!t.contains_key("--fake"));
        assert_eq!(t.get("--real").unwrap(), "2");
    }

    #[test]
    fn digests_page_layout() {
        let html = r#"<html><style>:root{--brand:oklch(60% .2 20)} body{font-family:"Space Grotesk",sans-serif;max-width:1120px} .card{padding:1rem}</style>
        <body><h1>A</h1><h2>B</h2><h2>C</h2><div class="card hero"></div><div class="card"></div><div class="card"></div></body></html>"#;
        let p = digest_html("page", "https://x", html);
        assert_eq!(p.kind, "html-digest");
        assert_eq!(p.tokens.get("--brand").unwrap(), "oklch(60% .2 20)");
        assert_eq!(p.fonts[0], "Space Grotesk");
        assert!(p.rules.iter().any(|r| r.contains("容器宽度") && r.contains("1120px")));
        assert!(p.rules.iter().any(|r| r.contains("h2×2")));
        assert!(p.archetypes.iter().any(|a| a.name == ".card"));
    }

    #[test]
    fn store_roundtrip_and_active() {
        let dir = tempfile::tempdir().unwrap();
        let store = DesignStore::open(dir.path()).unwrap();
        let p = parse_design_md("openflow", "design.md", DESIGN_MD);
        store.save(&p).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.active().unwrap().is_none());
        store.set_active("openflow").unwrap();
        assert_eq!(store.active().unwrap().unwrap().name, "openflow");
        assert!(store.get("nope").is_err());
    }

    #[test]
    fn chinese_comment_does_not_eat_following_vars() {
        // 回归：注释含中文时，按字节推进不能多吃声明
        let css = "/* 关键品牌决策：全 oklch，零 hex */\n:root{--bg:oklch(96.5% .016 85);--fg:oklch(22% .02 70);}";
        let t = parse_css_vars(css);
        assert_eq!(t.get("--bg").unwrap(), "oklch(96.5% .016 85)");
        assert_eq!(t.get("--fg").unwrap(), "oklch(22% .02 70)");
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn light_and_dark_scopes_kept_separate() {
        let css = ":root{--bg:oklch(96% .01 85);--fg:oklch(22% .02 70)}\n[data-theme=\"dark\"]{--bg:oklch(19% .014 70);--fg:oklch(93% .008 85)}";
        let p = parse_css("of", "tokens.css", css);
        assert_eq!(p.tokens.get("--bg").unwrap(), "oklch(96% .01 85)", "基础作用域必须是亮色");
        assert_eq!(p.tokens_dark.get("--bg").unwrap(), "oklch(19% .014 70)", "暗色不能被压平");
        let out = p.to_css();
        assert!(out.contains(":root{"));
        assert!(out.contains("[data-theme=\"dark\"]{"));
        assert!(out.contains("oklch(19% .014 70)"));
    }

    #[test]
    fn injects_into_head() {
        let p = parse_design_md("d", "x", DESIGN_MD);
        let html = "<html><head><title>t</title></head><body></body></html>";
        let out = apply_to_html(html, &p);
        assert!(out.contains("id=\"design-profile\""));
        assert!(out.contains("--accent: oklch(52% .17 258)"));
        assert!(out.find("design-profile").unwrap() < out.find("</head>").unwrap());
    }
}

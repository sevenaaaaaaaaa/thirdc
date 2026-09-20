//! kernel-store::refs — 文档内附件引用的抽取。
//!
//! 文档引用形式为 `Assets/<ab>/<cd>/<sha256>[.ext]`（Obsidian 可直接解析）。
//! 抽取 hash 建立 doc↔asset 关系，支撑“反查引用 / 换图床不改文档”。

const DELIMS: &[char] = &[
    ' ', '\t', '\n', ')', '"', '\'', '>', ']', '?', '#', '|', '\r', '(', '<',
];

/// 抽取文本中引用的全部附件 hash（去重、保持出现顺序）。
pub fn find_asset_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    while let Some(idx) = text[cursor..].find("Assets/") {
        let start = cursor + idx;
        let rest = &text[start..];
        let end = rest
            .char_indices()
            .find(|(_, c)| DELIMS.contains(c))
            .map(|(i, _)| i)
            .unwrap_or(rest.len());
        let path = &rest[..end];
        if let Some(hash) = hash_from_path(path) {
            if !out.contains(&hash) {
                out.push(hash);
            }
        }
        cursor = start + end.max(1);
        if cursor >= text.len() {
            break;
        }
    }
    out
}

/// 抽取行内 #标签（跳过标题行）。
pub fn find_tag_refs(md: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in md.lines() {
        if line.trim_start().starts_with('#') { continue; }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let prev_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '#');
            if chars[i] == '#' && prev_ok {
                let mut j = i + 1;
                let mut s = String::new();
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
                    s.push(chars[j]); j += 1;
                }
                let n = s.chars().count();
                if n >= 2 && n <= 24 && !out.contains(&s) { out.push(s); }
                i = j;
            } else { i += 1; }
        }
    }
    out
}

/// 从 `Assets/ab/cd/<name>` 路径提取 64 位 hex hash。
fn hash_from_path(path: &str) -> Option<String> {
    let name = path.rsplit('/').next()?;
    let stem = name.split('.').next()?;
    if stem.len() == 64 && stem.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
        Some(stem.to_string())
    } else {
        None
    }
}

/// 扩展名 → MIME（未知回退 application/octet-stream）。
pub fn mime_for_ext(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "md" => "text/markdown",
        "html" => "text/html",
        "txt" => "text/plain",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(c: char) -> String {
        std::iter::repeat(c).take(64).collect()
    }

    #[test]
    fn extracts_markdown_and_html_refs() {
        let png = h('a');
        let jpg = h('b');
        let text = format!(
            "看图 ![图](Assets/ab/cd/{png}.png) 和 <img src=\"Assets/ef/01/{jpg}.jpg\">\n再看一次 ![图](Assets/ab/cd/{png}.png)"
        );
        let refs = find_asset_refs(&text);
        assert_eq!(refs, vec![png, jpg], "去重并保持顺序");
    }

    #[test]
    fn ignores_non_asset_and_bad_hash() {
        assert!(find_asset_refs("Assets/ab/cd/notahash.png").is_empty());
        assert!(find_asset_refs(&("Other/ab/cd/".to_string() + &h('a'))).is_empty());
    }

    #[test]
    fn mime_mapping() {
        assert_eq!(mime_for_ext("PNG"), "image/png");
        assert_eq!(mime_for_ext("pdf"), "application/pdf");
        assert_eq!(mime_for_ext("xyz"), "application/octet-stream");
    }
}

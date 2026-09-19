import re

# ── 1) server: graph /doc 带 tags ──
p = 'server/src/lib.rs'
s = open(p, encoding='utf-8').read()

anchor = '/// 看板分栏依据：Notes/ 下第一层目录；根目录文档归入「(根)」。'
tags_fn = '''/// 行内 #标签 抽取（跳过标题行；去重保序）。
fn extract_tags(md: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in md.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0usize;
        while i < chars.len() {
            let prev_ok = i == 0 || !(chars[i - 1].is_alphanumeric() || chars[i - 1] == '#');
            if chars[i] == '#' && prev_ok {
                let mut j = i + 1;
                let mut s = String::new();
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
                    s.push(chars[j]);
                    j += 1;
                }
                let n = s.chars().count();
                if n >= 2 && n <= 24 && !out.contains(&s) {
                    out.push(s);
                }
                i = j;
            } else {
                i += 1;
            }
        }
    }
    out
}

'''
assert anchor in s
s = s.replace(anchor, tags_fn + anchor, 1)

old_node = '''        nodes.push(json!({
            "id": format!("doc:{path}"), "kind": kind, "path": path, "title": title,
            "excerpt": excerpt, "mtime": mtime, "collection": collection
        }));'''
new_node = '''        let tags = extract_tags(&md);
        nodes.push(json!({
            "id": format!("doc:{path}"), "kind": kind, "path": path, "title": title,
            "excerpt": excerpt, "mtime": mtime, "collection": collection, "tags": tags
        }));'''
assert old_node in s
s = s.replace(old_node, new_node, 1)

old_doc = '''        Ok(model) => Json(json!({
            "path": path,
            "title": model.title,
            "format": format,
            "source": source,'''
new_doc = '''        let doc_tags = extract_tags(&kernel_core::to_markdown(&model));
        Ok(model) => Json(json!({
            "path": path,
            "title": model.title,
            "format": format,
            "tags": doc_tags,
            "source": source,'''
assert old_doc in s
s = s.replace(old_doc, new_doc, 1)
open(p, 'w', encoding='utf-8').write(s)
print('1) tags wired')

# ── 2) to_html 排版升级 ──
p = 'kernel/crates/kernel-md/src/lib.rs'
s = open(p, encoding='utf-8').read()
start_marker = '    format!(\n        "<!doctype html>\\n<html lang=\\"zh\\"><head><meta charset=\\"utf-8\\">\\n\\'
start_idx = s.index(start_marker)
end_marker = '</body></html>\\n",\n'
end_idx = s.index(end_marker, start_idx) + len(end_marker)
# 再吞掉收尾的 "\n    )\n}"
tail_end = s.index('\n    )\n}', end_idx) + len('\n    )\n}')

new_fmt = '''    format!(
        "<!doctype html>\\n<html lang=\\"zh\\"><head><meta charset=\\"utf-8\\">\\n\\
<meta name=\\"viewport\\" content=\\"width=device-width, initial-scale=1\\">\\n\\
<title>{}</title>\\n\\
<script type=\\"application/ld+json\\">{}</script>\\n\\
<style>\\n\\
:root{{--doc-fg:oklch(24% .02 70);--doc-bg:oklch(98.5% .008 85);--doc-muted:oklch(48% .016 70);--doc-line:oklch(24% .02 70/.14);--doc-accent:oklch(54% .17 258);--doc-code-bg:oklch(96.5% .01 85);--doc-soft:oklch(24% .02 70/.05)}}\\n\\
html{{background:var(--doc-bg)}}\\n\\
body{{max-width:44rem;margin:0 auto;padding:56px 28px 96px;color:var(--doc-fg);font:16.5px/1.95 var(--font-body,system-ui,sans-serif);letter-spacing:.015em;-webkit-font-smoothing:antialiased}}\\n\\
h1{{font-size:2em;line-height:1.35;margin:0 0 14px;padding-bottom:14px;border-bottom:1px solid var(--doc-line)}}\\n\\
h2{{font-size:1.4em;margin:2.2em 0 .7em;position:relative;padding-left:.85em}}\\n\\
h2::before{{content:\\"\\";position:absolute;left:0;top:.3em;bottom:.3em;width:3.5px;border-radius:3px;background:var(--doc-accent)}}\\n\\
h3,h4{{font-size:1.14em;margin:1.7em 0 .55em}}\\n\\
p{{margin:.9em 0}}\\n\\
a{{color:var(--doc-accent);text-decoration:none;border-bottom:1px solid color-mix(in oklab,var(--doc-accent) 38%,transparent);transition:border-color .18s}}\\n\\
a:hover{{border-bottom-color:var(--doc-accent)}}\\n\\
strong{{font-weight:650}}\\n\\
blockquote{{margin:1.3em 0;padding:.7em 1.2em;border-left:3px solid var(--doc-accent);background:var(--doc-soft);border-radius:0 12px 12px 0;color:var(--doc-muted)}}\\n\\
blockquote p{{margin:.35em 0}}\\n\\
code{{font-family:var(--font-mono,ui-monospace,monospace);font-size:.86em;background:var(--doc-code-bg);border:1px solid var(--doc-line);border-radius:7px;padding:.12em .42em}}\\n\\
pre{{background:oklch(23% .014 75);color:oklch(93% .008 85);border-radius:14px;padding:18px 20px;overflow:auto;box-shadow:0 14px 34px -18px oklch(0% 0 0/.5)}}\\n\\
pre code{{background:none;border:none;color:inherit;padding:0;font-size:.88em;line-height:1.75}}\\n\\
table{{width:100%;border-collapse:collapse;margin:1.3em 0;font-size:.93em;border:1px solid var(--doc-line);border-radius:12px;overflow:hidden}}\\n\\
th{{background:var(--doc-soft);text-align:left;font-weight:650}}\\n\\
th,td{{padding:.55em .9em;border-bottom:1px solid var(--doc-line)}}\\n\\
tr:last-child td{{border-bottom:none}}\\n\\
tbody tr:nth-child(even){{background:color-mix(in oklab,var(--doc-soft) 55%,transparent)}}\\n\\
img{{max-width:100%;border-radius:14px;display:block;margin:1.4em auto;box-shadow:0 18px 44px -22px oklch(0% 0 0/.42)}}\\n\\
hr{{border:none;height:1px;background:var(--doc-line);margin:2.4em auto;width:62%}}\\n\\
::selection{{background:color-mix(in oklab,var(--doc-accent) 26%,transparent)}}\\n\\
</style>\\n\\
</head><body><article data-kb-format=\\"ai-html\\" data-block-count=\\"{n}\\">\\n{}\\n</article></body></html>\\n",
        title_esc,
        ld,
        doc.blocks.len(),
    )
}'''
s = s[:start_idx] + new_fmt + s[tail_end:]
open(p, 'w', encoding='utf-8').write(s)
print('2) to_html typography upgraded')

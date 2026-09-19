import re, json, sys

src = open('/Users/seveno/OpenFlow Dev/lib/ThemeSystem.php', encoding='utf-8').read()
# 取 presets() 函数体
m = re.search(r'function presets\(\): array \{(.*)$', src, re.S)
body = m.group(1)
# 手工切片：按 "'name' => [" 顶层分割（花括号配平）
# 先找到 return [ ... ]; 的范围
ret = re.search(r'return \[(.*?)\n        \];', body, re.S)
block = ret.group(1)

presets = {}
# 逐个预设块：'xxx' => [ ... ],  —— 用花括号/方括号配平提取
i = 0
pat = re.compile(r"'([a-z0-9_-]+)'\s*=>\s*\[")
matches = list(pat.finditer(block))
for idx, mm in enumerate(matches):
    name = mm.group(1)
    start = mm.end()
    depth = 1
    j = start
    while j < len(block) and depth > 0:
        if block[j] == '[':
            depth += 1
        elif block[j] == ']':
            depth -= 1
        j += 1
    inner = block[start:j-1]
    if name in presets:
        continue
    def sub_array(key):
        km = re.search(rf"'{key}'\s*=>\s*\[(.*?)\n\s*\]", inner, re.S)
        if not km:
            return {}
        out = {}
        for k, v in re.findall(r"'([a-z0-9-]+)'\s*=>\s*'([^']*)'", km.group(1)):
            out[k] = v
        return out
    nm = re.search(r"'name'\s*=>\s*'([^']*)'", inner)
    desc = re.search(r"'desc'\s*=>\s*'([^']*)'", inner)
    presets[name] = {
        'name': nm.group(1) if nm else name,
        'desc': desc.group(1) if desc else '',
        'light': sub_array('light'),
        'dark': sub_array('dark'),
        'layout': sub_array('layout'),
    }

out = {
    '_source': 'OpenFlow Dev/lib/ThemeSystem.php presets()',
    'presets': presets,
}
open('server/web/presets.json', 'w', encoding='utf-8').write(json.dumps(out, ensure_ascii=False, indent=1))
for k, v in presets.items():
    print(f"  {k:14} {v['name']:10} {v['desc'][:36]:38} light={len(v['light'])} dark={len(v['dark'])} layout={len(v['layout'])}")

import re

p = 'server/web/index.html'
s = open(p, encoding='utf-8').read()

# ═══════════ CSS ═══════════
css_anchor = '#toast{position:fixed;right:16px;bottom:44px;'
css_new = '''/* ── 模式 ── */
#mode-seg{display:flex;background:var(--surface);border:1px solid var(--border);border-radius:13px;padding:3px;gap:2px}
#mode-seg button{height:30px;padding:0 11px;border-radius:10px;border:1px solid transparent;background:transparent;color:var(--muted);font:500 12.5px var(--font-body);cursor:pointer;transition:all .25s var(--ease-spring)}
#mode-seg button:hover{color:var(--fg);background:var(--glass)}
#mode-seg button.on{background:var(--accent-soft);color:var(--accent);border-color:color-mix(in oklab,var(--accent) 40%,transparent)}
body[data-mode="organize"] #dock{display:none}
body[data-mode="learn"] #dock{display:none}
body[data-mode="agent"] #dock .thread{max-height:72vh}
body[data-mode="agent"] #canvas{filter:saturate(.55) brightness(.82)}
body[data-mode="organize"] #canvas{cursor:crosshair}

/* ── 卡片物理：景深 + 悬浮 ── */
#nodes{perspective:1400px}
.node{transform:scale(var(--ds,1)) rotateX(var(--rx,0deg)) rotateY(var(--ry,0deg));transform-style:preserve-3d;
  transition:transform .32s var(--ease-spring),box-shadow .3s,border-color .3s,opacity .3s,filter .3s;will-change:transform,opacity}
.node:hover{transform:scale(calc(var(--ds,1)*1.045)) translateY(-6px) rotateX(var(--rx,0deg)) rotateY(var(--ry,0deg));
  box-shadow:0 30px 70px -26px oklch(0% 0 0/.5),0 0 0 3px var(--accent-soft)}
.node.multi{box-shadow:0 0 0 3px var(--accent),var(--node-shadow)}
#lasso{position:fixed;z-index:55;border:1.5px solid var(--accent);background:var(--accent-soft);display:none;pointer-events:none}
#batchbar{position:fixed;left:50%;bottom:20px;transform:translateX(-50%);z-index:75;display:none;align-items:center;gap:9px;
  padding:9px 14px;border-radius:999px;border:1px solid var(--border);background:var(--surface-strong);backdrop-filter:blur(18px);box-shadow:var(--shadow)}
#batchbar b{font-family:var(--font-mono);font-size:12px;color:var(--accent)}
body[data-mode="organize"] #batchbar.show{display:flex}
/* 时间线/看板关闭景深缩放（保持可读） */
body[data-view="timeline"] .node,body[data-view="kanban"] .node{transform:none}
body[data-view="timeline"] .node:hover,body[data-view="kanban"] .node:hover{transform:translateY(-4px)}

/* ── 侧栏树 ── */
.tree{font-size:13px}
.tree-folder{user-select:none}
.tree-row{display:flex;align-items:center;gap:6px;padding:6px 8px;border-radius:9px;cursor:pointer;color:var(--muted);transition:background .16s,color .16s}
.tree-row:hover{background:var(--glass);color:var(--fg)}
.tree-row.drop-hot{background:var(--accent-soft);color:var(--accent)}
.tree-row .chev{width:12px;height:12px;stroke:currentColor;stroke-width:2;fill:none;transition:transform .25s var(--ease-spring);flex:0 0 auto}
.tree-folder.open>.tree-row .chev{transform:rotate(90deg)}
.tree-row .nm{flex:1;font-weight:600;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.tree-row .ct{font-family:var(--font-mono);font-size:9.5px;color:var(--faint)}
.tree-kids{display:none;padding-left:14px;border-left:1px solid var(--border-soft);margin-left:9px}
.tree-folder.open>.tree-kids{display:block}
.tree-leaf{display:flex;align-items:center;gap:6px;padding:5px 8px;border-radius:9px;cursor:grab;color:var(--fg);transition:background .16s}
.tree-leaf:hover{background:var(--glass)}
.tree-leaf.on{background:var(--surface-strong);box-shadow:inset 2px 0 0 var(--accent)}
.tree-leaf .nm{flex:1;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-weight:500}
.tagchip{display:inline-flex;align-items:center;height:15px;padding:0 6px;border-radius:999px;font:600 9px var(--font-mono);color:var(--on-accent);flex:0 0 auto}
#rail-search{margin:2px 10px 6px;width:calc(100% - 20px);height:30px;padding:0 10px;border-radius:9px;border:1px solid var(--border);background:var(--bg3);color:var(--fg);font:12.5px var(--font-body);outline:none;box-sizing:border-box}
#rail-search:focus{border-color:var(--accent)}
.bg3{background:var(--bg-soft)}

/* ── 展示模式 ── */
#present-ov{position:fixed;inset:0;z-index:200;background:var(--bg);display:none}
#present-ov.open{display:block}
#present-ov iframe{width:100%;height:calc(100% - 46px);border:none;background:var(--bg)}
.present-bar{height:46px;display:flex;align-items:center;gap:12px;padding:0 18px;border-bottom:1px solid var(--border);background:var(--surface)}
.present-bar .t{font-family:var(--font-display);font-weight:700;flex:1;overflow:hidden;white-space:nowrap;text-overflow:ellipsis}

#toast{position:fixed;right:16px;bottom:44px;'''
assert css_anchor in s
s = s.replace(css_anchor, css_new, 1)

# ═══════════ HTML ═══════════
s = s.replace('''  <button id="board-pill" title="切换画布">''', '''  <nav id="mode-seg">
    <button data-mode="studio" class="on">Studio</button>
    <button data-mode="organize">整理</button>
    <button data-mode="learn">学习</button>
    <button data-mode="present">展示</button>
    <button data-mode="rag">RAG</button>
    <button data-mode="online">线上</button>
    <button data-mode="agent">Agent</button>
  </nav>
  <button id="board-pill" title="切换画布">''', 1)

s = s.replace('''  <div class="rail-head"><span class="kicker">Knowledge</span><span class="kicker" id="rail-count" style="color:var(--faint)">0</span></div>''',
'''  <div class="rail-head"><span class="kicker">Knowledge</span><span class="kicker" id="rail-count" style="color:var(--faint)">0</span></div>
  <input id="rail-search" placeholder="过滤文档…">
  <div id="lasso"></div>
  <div id="batchbar">
    <b id="batch-n">0</b>
    <button class="act" id="b-move">移动到…</button>
    <button class="act" id="b-tag">打标签</button>
    <button class="act danger" id="b-del">删除</button>
    <button class="act" id="b-clear">取消</button>
  </div>''', 1)

s = s.replace('</body>', '''<div id="present-ov">
  <div class="present-bar"><span class="t" id="p-title"></span><span class="badge ok" id="p-idx"></span>
  <button class="act" id="p-prev">← 上一篇</button><button class="act" id="p-next">下一篇 →</button>
  <button class="act" id="p-exit">退出 (Esc)</button></div>
  <iframe id="p-frame" sandbox="" title="展示"></iframe>
</div>
</body>''', 1)

open(p, 'w', encoding='utf-8').write(s)
print('markup done')

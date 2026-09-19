p = 'server/web/index.html'
s = open(p, encoding='utf-8').read()

# ═══════════ CSS ═══════════
css_anchor = '#toast{position:fixed;right:16px;bottom:44px;'
css_new = '''/* ── TOC ── */
#toc{position:fixed;left:270px;top:calc(var(--chrome-h) + 72px);bottom:60px;width:180px;z-index:25;
  overflow-y:auto;padding:10px 8px;border-radius:var(--r-md);border:1px solid var(--border);background:var(--surface);
  backdrop-filter:blur(12px);font-size:11.5px;display:none;transition:opacity .3s}
#toc.open{display:block}
#toc h4{margin:0 0 8px;font-family:var(--font-mono);font-size:10px;letter-spacing:.08em;text-transform:uppercase;color:var(--faint)}
.toc-item{padding:3px 8px;border-radius:6px;cursor:pointer;color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis;
  transition:color .15s,background .15s;border-left:2px solid transparent}
.toc-item:hover{color:var(--fg);background:var(--hover)}
.toc-item.active{color:var(--accent);border-left-color:var(--accent);font-weight:600}
.toc-item.lv2{padding-left:16px}.toc-item.lv3{padding-left:24px}.toc-item.lv4{padding-left:32px}
.toc-line{position:fixed;left:268px;top:0;width:2px;height:28px;background:var(--accent);border-radius:2px;opacity:0;
  transition:top .3s var(--ease-spring),opacity .3s;pointer-events:none;z-index:26}
.toc-line.on{opacity:1}

/* ── Memo 模式 ── */
#memo-bar{position:fixed;top:calc(var(--chrome-h) + 12px);left:50%;transform:translateX(-50%);z-index:80;
  width:min(600px,80vw);display:none;align-items:center;gap:8px;padding:8px 14px;
  border-radius:var(--r-lg);border:1px solid var(--border);background:var(--surface-strong);backdrop-filter:blur(20px) saturate(160%);box-shadow:var(--shadow)}
#memo-bar.open{display:flex;animation:nodeIn .35s var(--ease-spring) both}
#memo-bar input{flex:1;border:none;outline:none;background:transparent;color:var(--fg);font:15px var(--font-body)}
#memo-bar input::placeholder{color:var(--faint)}
#memo-bar .send{width:32px;height:32px;border-radius:10px;border:none;background:var(--accent);color:var(--on-accent);display:grid;place-items:center;cursor:pointer}
#memo-bar .send svg{width:15px;height:15px;stroke:currentColor;stroke-width:1.8;fill:none}
#memo-bar .date{font-family:var(--font-mono);font-size:10px;color:var(--faint);white-space:nowrap}

/* ── 发布水滴 ── */
#water-overlay{position:fixed;inset:0;z-index:300;display:none;place-items:center;background:color-mix(in oklab,var(--bg) 70%,transparent);backdrop-filter:blur(10px)}
#water-overlay.open{display:grid}
.drop{width:20px;height:20px;border-radius:50% 50% 50% 0;transform:rotate(-45deg);background:var(--accent);
  animation:drop-fall .7s var(--ease-out) both}
@keyframes drop-fall{from{transform:translateY(-40vh) rotate(-45deg) scale(.5);opacity:1}
  80%{transform:translateY(0) rotate(-45deg) scale(1)}to{transform:translateY(0) rotate(-45deg) scale(1);opacity:1}}
.ripple{position:absolute;width:60px;height:60px;border-radius:50%;border:2px solid var(--accent);opacity:0;
  animation:ripple 1.2s var(--ease-out) .6s both}
@keyframes ripple{0%{transform:scale(0);opacity:.8}100%{transform:scale(6);opacity:0}}
.ripple2{animation-delay:.8s!important}
.envelope{width:280px;padding:24px;border-radius:var(--r-lg);background:var(--surface-strong);border:1px solid var(--border);
  box-shadow:var(--shadow);text-align:center;opacity:0;animation:env-open .5s var(--ease-spring) 1.2s both;cursor:pointer}
@keyframes env-open{from{opacity:0;transform:scale(.85) translateY(20px)}to{opacity:1;transform:none}}
.envelope .seal{width:44px;height:44px;border-radius:50%;background:var(--accent);color:var(--on-accent);display:grid;place-items:center;
  font-size:20px;margin:0 auto 14px;font-weight:800}
.envelope .link{font-family:var(--font-mono);font-size:12px;color:var(--accent);word-break:break-all;padding:8px;background:var(--hover);border-radius:8px;margin:10px 0}
.envelope .hint{font-size:11px;color:var(--faint)}

#toast{position:fixed;right:16px;bottom:44px;'''
assert css_anchor in s
s = s.replace(css_anchor, css_new, 1)

# ═══════════ HTML ═══════════
# TOC + Memo bar + Water overlay
s = s.replace('''<div id="toast"></div>''', '''<div id="toc"><h4>目录</h4><div id="toc-list"></div></div>
<div class="toc-line" id="toc-line"></div>
<div id="memo-bar">
  <span class="date" id="memo-date"></span>
  <input id="memo-input" placeholder="随手记…（Enter 发送）">
  <button class="send" id="memo-send"><svg viewBox="0 0 24 24"><path d="M5 12h13M12 5l7 7-7 7"/></svg></button>
</div>
<div id="water-overlay">
  <div style="text-align:center;position:relative">
    <div class="drop"></div>
    <div class="ripple"></div><div class="ripple ripple2"></div>
    <div class="envelope" id="envelope">
      <div class="seal">3C</div>
      <div style="font-weight:700;font-size:15px;margin-bottom:4px">已发布</div>
      <div class="link" id="env-link"></div>
      <div class="hint">点击复制链接</div>
    </div>
  </div>
</div>
<div id="toast"></div>''', 1)

# ═══════════ JS ═══════════

# TOC：抽屉打开时生成，scroll 联动
old_open_doc = '''    $('preview').srcdoc=d.html;'''
new_open_doc = '''    $('preview').srcdoc=d.html;
    buildToc(d.markdown);'''
assert old_open_doc in s
s = s.replace(old_open_doc, new_open_doc, 1)

old_close = "$('d-close').onclick=()=>$('drawer').classList.remove('open');"
new_close = '''$('d-close').onclick=()=>{$('drawer').classList.remove('open');$('toc').classList.remove('open');$('toc-line').classList.remove('on')};'''
assert old_close in s
s = s.replace(old_close, new_close, 1)

# TOC 生成 + scroll 联动
old_mode_seg = '''/* ───────── 模式框架 ───────── */'''
new_mode_seg = '''/* ───────── TOC ───────── */
function buildToc(md){
  const box=$('toc-list');box.innerHTML='';
  const heads=[];
  for(const line of md.split('\\n')){
    const m=/^(#{2,4})\\s+(.+)/.exec(line);
    if(m)heads.push({level:m[1].length,text:m[2].trim(),idx:heads.length});
  }
  if(!heads.length){$('toc').classList.remove('open');$('toc-line').classList.remove('on');return}
  $('toc').classList.add('open');
  heads.forEach((h,i)=>{
    const el=document.createElement('div');
    el.className=`toc-item lv${h.level}`;el.textContent=h.text;
    el.onclick=()=>{
      const frame=$('preview');
      frame.contentWindow.postMessage({type:'scroll-to-h',index:i},'*');
    };
    box.appendChild(el);
  });
  // 注入 iframe 内的 heading id + scroll 监听
  const frame=$('preview');
  frame.onload=()=>{
    try{
      const doc=frame.contentDocument;
      const hs=doc.querySelectorAll('h2,h3,h4');
      hs.forEach((h,i)=>h.id='toc-h'+i);
      doc.addEventListener('scroll',()=>{
        let active=0;
        hs.forEach((h,i)=>{if(h.getBoundingClientRect().top<80)active=i});
        box.querySelectorAll('.toc-item').forEach((el,i)=>el.classList.toggle('active',i===active));
        const on=box.querySelector('.toc-item.active');
        const line=$('toc-line');
        if(on){const r=on.getBoundingClientRect();line.style.top=r.top+'px';line.classList.add('on')}
        else line.classList.remove('on');
      },{passive:true});
    }catch(e){}
  };
}

/* ───────── Memo 模式 ───────── */
function toggleMemo(){
  const bar=$('memo-bar');
  const opening=!bar.classList.contains('open');
  bar.classList.toggle('open',opening);
  if(opening){
    const now=new Date();
    $('memo-date').textContent=`${now.getFullYear()}-${String(now.getMonth()+1).padStart(2,'0')}-${String(now.getDate()).padStart(2,'0')}`;
    $('memo-input').focus();
  }
}
async function sendMemo(){
  const text=$('memo-input').value.trim();if(!text)return;
  $('memo-input').value='';
  const now=new Date();
  const date=`${now.getFullYear()}-${String(now.getMonth()+1).padStart(2,'0')}-${String(now.getDate()).padStart(2,'0')}`;
  const time=`${String(now.getHours()).padStart(2,'0')}:${String(now.getMinutes()).padStart(2,'0')}`;
  const path=`Notes/Memos/${date}.md`;
  try{
    // 读现有内容（或新建）
    let existing='';
    try{const d=await api('/doc?path='+encodeURIComponent(path));existing=d.markdown||''}catch{}
    const entry=`\\n- ${time} ${text}`;
    if(!existing){existing=`# ${date} Memo\\n${entry}`}
    else{existing+=entry}
    await api('/doc?path='+encodeURIComponent(path),{method:'PUT',body:existing,headers:{'content-type':'text/markdown'}});
    toast('已记录','ok');
    await refreshAll();
  }catch(e){toast('Memo 失败：'+e.message,'err')}
}
$('memo-send').onclick=sendMemo;
$('memo-input').addEventListener('keydown',e=>{if(e.key==='Enter')sendMemo()});

/* ───────── 发布水滴特效 ───────── */
function showWaterEffect(url){
  $('env-link').textContent=url;
  $('water-overlay').classList.add('open');
  setTimeout(()=>$('envelope').onclick=()=>{
    navigator.clipboard?.writeText(url);
    toast('链接已复制','ok');
    setTimeout(()=>$('water-overlay').classList.remove('open'),600);
  },1300);
  setTimeout(()=>$('water-overlay').classList.remove('open'),5000);
}
// 覆盖 doPublish 的 toast，改用水滴特效
const _oldDoPublish=doPublish;
async function doPublish(path){
  try{
    const r=await api('/publish',{method:'POST',json:{path}});
    const url=location.origin+r.url;
    $('water-overlay').classList.remove('open');
    showWaterEffect(url);
    await loadPublishList();await loadPublishTargets();
  }catch(e){toast('发布失败：'+e.message,'err')}
}

/* ───────── 模式框架 ───────── */'''
assert old_mode_seg in s
s = s.replace(old_mode_seg, new_mode_seg, 1)

# Memo 键盘快捷键 (Ctrl+M / Cmd+M)
s = s.replace('''  if(e.key==='m'){sparkle(20);$('organize-btn').click();}''',
'''  if(e.key==='m'&&e.shiftKey){toggleMemo();}
  if(e.key==='m'){sparkle(20);$('organize-btn').click();}''', 1)

# A2UI iframe postMessage 桥里也加 TOC scroll-to 监听
s = s.replace('''frame.srcdoc=d.html;''', '''frame.srcdoc=d.html;
    // TOC scroll-to 注入
    const _old_onload=frame.onload;
    frame.onload=()=>{try{
      const doc=frame.contentDocument;
      doc.addEventListener('message',ev=>{
        if(ev.data&&ev.data.type==='scroll-to-h'){
          const hs=doc.querySelectorAll('h2,h3,h4');const h=hs[ev.data.index];if(h)h.scrollIntoView({behavior:'smooth'});
        }
      });
    }catch(e){}};
    if(_old_onload)frame.onload(_old_onload);''', 1)

open(p, 'w', encoding='utf-8').write(s)
print('TOC + Memo + Water drop done')

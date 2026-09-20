const api = globalThis.browser ?? globalThis.chrome;
const $ = (id) => document.getElementById(id);

function normalizeBase(raw) {
  let base = (raw || "").trim().replace(/\/+$/, "");
  const m = /^(https?:\/\/[^/]+)$/.exec(base);
  if (m && m[1].includes("nownexts.com")) base = m[1] + "/thirdc";
  return base;
}

api.storage.local.get(["baseUrl", "token"]).then((d) => {
  if (d.baseUrl) $("url").value = d.baseUrl;
  if (d.token) $("token").value = d.token;
});

$("save").onclick = async () => {
  const base = normalizeBase($("url").value);
  $("url").value = base;
  await api.storage.local.set({ baseUrl: base, token: $("token").value.trim() });
  $("msg").className = "ok";
  $("msg").textContent = "已保存：" + base;
};

$("test").onclick = async () => {
  const base = normalizeBase($("url").value);
  const token = $("token").value.trim();
  $("msg").className = "";
  $("msg").textContent = "测试中…";
  try {
    const r = await fetch(`${base}/status`, { headers: { authorization: `Bearer ${token}` } });
    if (r.status === 401) throw new Error("Token 无效（用登录密码那个）");
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    const d = await r.json();
    $("msg").className = "ok";
    $("msg").textContent = `✓ 连接成功\n库：${d.vault}\n文档：${d.docs} · 已索引：${d.indexed}`;
  } catch (e) {
    $("msg").className = "err";
    $("msg").textContent = `✗ ${e.message}\n检查地址是否含 /thirdc`;
  }
};

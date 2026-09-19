chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.create({ id: "thirdc-clip", title: "采集到 ThirdC", contexts: ["page", "selection"] });
});
chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId !== "thirdc-clip") return;
  const cfg = await chrome.storage.local.get(["baseUrl", "token"]);
  if (!cfg.baseUrl || !cfg.token) { chrome.action.openPopup(); return; }
  const selection = info.selectionText || "";
  chrome.scripting.executeScript({
    target: { tabId: tab.id },
    func: () => document.documentElement.outerHTML,
  }, async (results) => {
    const html = results?.[0]?.result || "";
    const res = await fetch(`${cfg.baseUrl}/ingest/web`, {
      method: "POST",
      headers: { "content-type": "application/json", "authorization": `Bearer ${cfg.token}` },
      body: JSON.stringify({ url: tab.url, title: tab.title, selection, html: html.slice(0, 500000) }),
    }).then(r => r.json()).catch(e => ({ error: e.message }));
    chrome.notifications?.create({ type: "basic", iconUrl: "icon48.png", title: res.rel ? "已采集" : "采集失败", message: res.rel || res.error || "" });
  });
});

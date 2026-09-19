browser.runtime.onInstalled.addListener(() => {
  browser.contextMenus.create({ id: "thirdc-clip", title: "采集到 ThirdC", contexts: ["page", "selection"] });
});
browser.contextMenus.onClicked.addListener(async (info, tab) => {
  if (info.menuItemId !== "thirdc-clip") return;
  const cfg = await browser.storage.local.get(["baseUrl", "token"]);
  if (!cfg.baseUrl || !cfg.token) {
    browser.runtime.openOptionsPage ? browser.runtime.openOptionsPage() : browser.tabs.create({url: "popup.html"});
    return;
  }
  const selection = info.selectionText || "";
  const results = await browser.scripting.executeScript({
    target: { tabId: tab.id },
    func: () => document.documentElement.outerHTML,
  });
  const html = results?.[0]?.result || "";
  try {
    const res = await fetch(`${cfg.baseUrl}/ingest/web`, {
      method: "POST",
      headers: { "content-type": "application/json", "authorization": `Bearer ${cfg.token}` },
      body: JSON.stringify({ url: tab.url, title: tab.title, selection, html: html.slice(0, 500000) }),
    }).then(r => r.json());
    browser.notifications.create({ type: "basic", iconUrl: "icon48.png", title: res.rel ? "✓ 已采集" : "采集失败", message: res.rel || res.error || "" });
  } catch (e) {
    browser.notifications.create({ type: "basic", iconUrl: "icon48.png", title: "网络错误", message: e.message });
  }
});

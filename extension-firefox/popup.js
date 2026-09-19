browser.storage.local.get(["baseUrl","token"]).then(d=>{
  if(d.baseUrl)document.getElementById("url").value=d.baseUrl;
  if(d.token)document.getElementById("token").value=d.token;
});
document.getElementById("save").onclick=async()=>{
  await browser.storage.local.set({baseUrl:document.getElementById("url").value.trim(),token:document.getElementById("token").value.trim()});
  document.getElementById("save").textContent="已保存 ✓";setTimeout(()=>window.close(),800);
};

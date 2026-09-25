// About window: shows the version and opens the allowlisted web pages.
const invoke = window.__TAURI__?.core.invoke;

document.getElementById("year").textContent = String(new Date().getFullYear());

document.querySelectorAll("[data-link]").forEach((btn) =>
  btn.addEventListener("click", () => invoke?.("open_link", { name: btn.dataset.link })),
);

document.getElementById("licenses").addEventListener("click", () => invoke?.("open_licenses"));

document.addEventListener("contextmenu", (e) => {
  if (!e.target.closest("section p")) e.preventDefault();
});

if (invoke) {
  try {
    const info = await invoke("app_info");
    document.getElementById("version").textContent = `Version ${info.version}`;
  } catch {
    /* version stays as a dash */
  }
}

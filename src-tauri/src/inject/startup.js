(function () {
  const config = window.__WALLPAPER_STARTUP_CONFIG__;
  if (!config || !window.__TAURI__?.core || window.__WALLPAPER_STARTUP__)
    return;
  const url = new URL(window.location.href);
  const official = url.origin === "https://wallpaper.061129.xyz";
  const local =
    url.pathname === "/wallpaper-startup.html" &&
    ((url.protocol === "tauri:" && url.hostname === "localhost") ||
      (url.protocol === "http:" && url.hostname === "tauri.localhost"));
  if (!official && !local) return;
  let active = true;
  let finished = false;
  let completing = false;
  let opening = false;
  let started = false;
  let timer;
  let fallbackTimer;
  let observer;
  const invoke = (event, generation) =>
    window.__TAURI__.core.invoke("wallpaper_startup", { event, generation });
  const session = invoke(local ? "status" : "begin", null);
  session.catch((error) => {
    console.warn("[Pake] Startup session failed:", error);
    slow("暂时无法打开页面，请重试");
  });
  const afterPaint = (callback) =>
    window.requestAnimationFrame(() =>
      window.requestAnimationFrame(() => {
        if (active) callback();
      }),
    );
  const get = (id) => document.getElementById(id);

  function slow(message = "连接用时较长，请检查网络后重试") {
    if (!active || finished) return;
    const root = get("wallpaper-startup");
    if (!root) return;
    root.setAttribute("data-state", "slow");
    get("wallpaper-startup-message").textContent = message;
    get("wallpaper-startup-retry").hidden = false;
  }

  async function open() {
    if (!active || opening) return;
    opening = true;
    try {
      const state = await session;
      if (active) {
        const result = await invoke("open", state.generation);
        state.generation = result.generation;
        if (!result.accepted) slow();
      }
    } catch (error) {
      console.warn("[Pake] Startup connection failed:", error);
      slow("暂时无法连接，请重试");
      opening = false;
    }
  }

  async function retry() {
    if (!active || finished) return;
    get("wallpaper-startup-retry").hidden = true;
    get("wallpaper-startup-message").textContent = "正在重新连接";
    try {
      const state = await session;
      const result = await invoke("retry", state.generation);
      state.generation = result.generation;
      if (!result.accepted) slow();
    } catch (error) {
      console.warn("[Pake] Startup retry failed:", error);
      slow("暂时无法连接，请重试");
    }
  }

  function cleanup() {
    window.clearTimeout(timer);
    window.clearTimeout(fallbackTimer);
    observer?.disconnect();
  }

  function complete() {
    if (!active || finished || completing || local) return;
    completing = true;
    afterPaint(async () => {
      try {
        const state = await session;
        if (!active) return;
        const result = await invoke("ready", state.generation);
        if (!active || !result.accepted) return;
        finished = true;
        cleanup();
        document.documentElement.removeAttribute("data-wallpaper-startup");
        const root = get("wallpaper-startup");
        root?.setAttribute("aria-busy", "false");
        root?.setAttribute("data-state", "leaving");
        if (window.matchMedia("(prefers-reduced-motion: reduce)").matches)
          root?.remove();
        else window.setTimeout(() => root?.remove(), 240);
      } catch (error) {
        console.warn("[Pake] Startup readiness failed:", error);
        completing = false;
        slow("暂时无法打开页面，请重试");
      }
    });
  }

  function visible(element) {
    if (!element || !element.getClientRects().length) return false;
    const style = window.getComputedStyle(element);
    return style.display !== "none" && style.opacity !== "0";
  }

  function headerlessReady() {
    if (!visible(document.querySelector("#app .app.no-header"))) return false;
    const pathname = url.pathname.replace(/\/+$/, "");
    let selectors;
    if (pathname === "/login" || pathname === "/signup") {
      selectors = [
        '#app main .auth-page form input[type="email"]',
        '#app main .auth-page form input[type="password"]',
        '#app main .auth-page form button[type="submit"]',
      ];
    } else if (pathname === "/about") {
      selectors = [
        "#app main .about-page h1",
        "#app main .about-page .hero-card__summary",
        "#app main .about-page .hero-card__actions button",
      ];
    } else return false;
    return selectors.every((selector) =>
      visible(document.querySelector(selector)),
    );
  }

  let fallbackAttempts = 0;
  function checkReady() {
    if (!active || finished) return;
    if (
      document.documentElement.getAttribute("data-wallpaper-app-ready") ===
      "true"
    ) {
      complete();
      return;
    }
    // Older official pages have no explicit latch. Require a rendered shell,
    // actual route content and either the header or complete controls on a
    // known standalone route; a timer alone never means ready.
    if (
      document.readyState !== "loading" &&
      visible(document.querySelector("#app .app")) &&
      visible(document.querySelector("#app main > *")) &&
      (visible(document.querySelector("#app header")) || headerlessReady())
    )
      complete();
    if (++fallbackAttempts < 60)
      fallbackTimer = window.setTimeout(checkReady, 200);
  }

  function mount() {
    if (!active) return;
    if (!get("wallpaper-startup"))
      document.documentElement.insertAdjacentHTML("beforeend", config.surface);
    if (local && config.darkMode)
      document.documentElement.setAttribute("data-theme", "dark");
    if (!local)
      document.documentElement.setAttribute(
        "data-wallpaper-startup",
        "loading",
      );
    get("wallpaper-startup-retry").addEventListener("click", retry);
    timer = window.setTimeout(() => slow(), 12000);
    if (!local) {
      window.addEventListener("wallpaper:app-ready", checkReady);
      observer = new MutationObserver(() => {
        if (
          document.documentElement.getAttribute("data-wallpaper-app-ready") ===
          "true"
        )
          complete();
      });
      observer.observe(document.documentElement, {
        attributes: true,
        attributeFilter: ["data-wallpaper-app-ready"],
      });
      checkReady();
    }
  }

  window.__WALLPAPER_STARTUP__ = {
    async start() {
      if (started || !active || !local) return;
      started = true;
      try {
        const state = await session;
        if (state.timedOut) slow();
        else afterPaint(open);
      } catch (error) {
        console.warn("[Pake] Startup initialization failed:", error);
        slow("暂时无法打开页面，请重试");
      }
    },
  };
  window.addEventListener("pagehide", () => {
    active = false;
    cleanup();
  });
  if (document.documentElement && (!local || document.readyState !== "loading"))
    mount();
  else document.addEventListener("DOMContentLoaded", mount, { once: true });
})();

import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";
import { describe, expect, it } from "vitest";

const scriptPath = path.join(process.cwd(), "src-tauri/src/inject/startup.js");
const source = fs.existsSync(scriptPath)
  ? fs.readFileSync(scriptPath, "utf8")
  : "";
const surface =
  '<section id="wallpaper-startup"><p id="wallpaper-startup-message"></p><button id="wallpaper-startup-retry" hidden></button></section>';

function fixture({
  local = false,
  configured = true,
  ready = false,
  reducedMotion = false,
  timedOut = false,
  loading = false,
  external = false,
  darkMode = false,
  rendered = false,
  pathname = "/desktop",
  selectors = new Set(),
} = {}) {
  const elements = new Map();
  const listeners = new Map();
  const frames = [];
  const timers = [];
  const calls = [];
  const attributes = new Map(
    ready ? [["data-wallpaper-app-ready", "true"]] : [],
  );
  const element = (id) => ({
    id,
    hidden: true,
    textContent: "",
    style: {},
    isConnected: true,
    setAttribute(name, value) {
      this[name] = value;
    },
    addEventListener(name, handler) {
      listeners.set(`${id}:${name}`, handler);
    },
    remove() {
      this.isConnected = false;
      elements.delete(id);
    },
  });
  const root = element("wallpaper-startup");
  const message = element("wallpaper-startup-message");
  const retry = element("wallpaper-startup-retry");
  const mount = () => {
    for (const item of [root, message, retry]) elements.set(item.id, item);
  };
  if (local) mount();
  const document = {
    readyState: loading ? "loading" : "complete",
    visibilityState: "visible",
    documentElement: {
      style: {},
      getAttribute: (key) => attributes.get(key),
      setAttribute: (key, value) => attributes.set(key, value),
      removeAttribute: (key) => attributes.delete(key),
      insertAdjacentHTML: mount,
    },
    body: { insertAdjacentHTML: mount },
    getElementById: (id) => elements.get(id) || null,
    querySelector: (selector) =>
      rendered || selectors.has(selector)
        ? { getClientRects: () => [{}] }
        : null,
    addEventListener: (name, handler) =>
      listeners.set(`document:${name}`, handler),
    removeEventListener: (name) => listeners.delete(`document:${name}`),
  };
  const window = {
    location: new URL(
      external
        ? "https://accounts.google.com/"
        : local
          ? "tauri://localhost/wallpaper-startup.html"
          : `https://wallpaper.061129.xyz${pathname}`,
    ),
    __TAURI__: {
      core: {
        invoke: async (command, args) => {
          calls.push({ command, ...args });
          return { generation: 7, timedOut, accepted: true };
        },
      },
    },
    addEventListener: (name, handler) => listeners.set(name, handler),
    removeEventListener: (name) => listeners.delete(name),
    matchMedia: () => ({ matches: reducedMotion }),
    getComputedStyle: () => ({ display: "block", opacity: "1" }),
    requestAnimationFrame: (callback) => {
      frames.push(callback);
      return frames.length;
    },
    setTimeout: (callback, delay) => {
      timers.push({ callback, delay });
      return timers.length;
    },
    clearTimeout: (id) => {
      if (timers[id - 1]) timers[id - 1].cancelled = true;
    },
  };
  if (configured)
    window.__WALLPAPER_STARTUP_CONFIG__ = {
      target: "https://wallpaper.061129.xyz/",
      surface,
      darkMode,
    };
  const context = {
    window,
    document,
    URL,
    console,
    MutationObserver: class {
      observe() {}
      disconnect() {}
    },
  };
  vm.runInNewContext(source, context);
  const settle = async () => {
    for (let index = 0; index < 8; index++) await Promise.resolve();
  };
  const flushFrames = async () => {
    while (frames.length) {
      frames.shift()();
      await settle();
    }
  };
  return {
    window,
    document,
    root,
    message,
    retry,
    elements,
    attributes,
    listeners,
    calls,
    frames,
    timers,
    settle,
    flushFrames,
  };
}

describe("wallpaper startup lifecycle", () => {
  it("does not alter an ordinary browser page", () => {
    const f = fixture({ configured: false });
    expect(f.elements.size).toBe(0);
    expect(f.attributes.has("data-wallpaper-startup")).toBe(false);
  });

  it("leaves authentication documents alone even in the configured home window", () => {
    const f = fixture({ external: true });
    expect(f.elements.size).toBe(0);
    expect(f.calls).toHaveLength(0);
  });

  it("covers the official remote app until genuine ready, then fades once", async () => {
    const f = fixture();
    await f.settle();
    expect(f.elements.has("wallpaper-startup")).toBe(true);
    expect(f.attributes.get("data-wallpaper-startup")).toBe("loading");
    f.attributes.set("data-wallpaper-app-ready", "true");
    f.listeners.get("wallpaper:app-ready")();
    f.listeners.get("wallpaper:app-ready")();
    await f.flushFrames();
    expect(f.calls.filter((call) => call.event === "ready")).toHaveLength(1);
    expect(f.attributes.has("data-wallpaper-startup")).toBe(false);
    expect(f.root["data-state"]).toBe("leaving");
    f.timers.find((timer) => timer.delay === 240).callback();
    expect(f.elements.has("wallpaper-startup")).toBe(false);
  });

  it("accepts a ready latch written before the injected bridge listener", async () => {
    const f = fixture({ ready: true, reducedMotion: true });
    await f.settle();
    await f.flushFrames();
    expect(f.calls.filter((call) => call.event === "ready")).toHaveLength(1);
    expect(f.elements.has("wallpaper-startup")).toBe(false);
    expect(f.timers.some((timer) => timer.delay === 240)).toBe(false);
  });

  it("keeps a slow or broken app covered and offers an honest retry", async () => {
    const f = fixture();
    await f.settle();
    f.timers.find((timer) => timer.delay === 12000).callback();
    expect(f.message.textContent).toContain("连接用时较长");
    expect(f.retry.hidden).toBe(false);
    expect(f.attributes.get("data-wallpaper-startup")).toBe("loading");
    await f.listeners.get("wallpaper-startup-retry:click")();
    expect(f.calls.at(-1)).toMatchObject({ event: "retry", generation: 7 });
    expect(f.calls.some((call) => call.event === "ready")).toBe(false);
  });

  it("opens the native-configured home only after the local page can paint", async () => {
    const f = fixture({ local: true });
    await f.settle();
    expect(f.calls.some((call) => call.event === "open")).toBe(false);
    f.window.__WALLPAPER_STARTUP__.start();
    await f.settle();
    expect(f.calls.some((call) => call.event === "open")).toBe(false);
    await f.flushFrames();
    expect(f.calls.filter((call) => call.event === "open")).toEqual([
      { command: "wallpaper_startup", event: "open", generation: 7 },
    ]);
  });

  it("lets the local HTML parser build its surface before mounting the bridge and preserves dark override", async () => {
    const f = fixture({ local: true, loading: true, darkMode: true });
    expect(f.listeners.has("wallpaper-startup-retry:click")).toBe(false);
    f.document.readyState = "interactive";
    f.listeners.get("document:DOMContentLoaded")();
    expect(f.listeners.has("wallpaper-startup-retry:click")).toBe(true);
    expect(f.attributes.get("data-theme")).toBe("dark");
  });

  it("finishes an older page only when its real header and route content are rendered", async () => {
    const f = fixture({ rendered: true });
    await f.settle();
    await f.flushFrames();
    expect(f.calls.filter((call) => call.event === "ready")).toHaveLength(1);
  });

  const headerlessShell = ["#app .app", "#app main > *", "#app .app.no-header"];
  const authContent = [
    '#app main .auth-page form input[type="email"]',
    '#app main .auth-page form input[type="password"]',
    '#app main .auth-page form button[type="submit"]',
  ];
  const aboutContent = [
    "#app main .about-page h1",
    "#app main .about-page .hero-card__summary",
    "#app main .about-page .hero-card__actions button",
  ];

  it.each(["/login", "/signup"])(
    "finishes a fully rendered older %s form without a header or ready latch",
    async (pathname) => {
      const f = fixture({
        pathname,
        selectors: new Set([...headerlessShell, ...authContent]),
      });
      await f.settle();
      await f.flushFrames();
      expect(f.calls.filter((call) => call.event === "ready")).toHaveLength(1);
      expect(f.attributes.has("data-wallpaper-startup")).toBe(false);
    },
  );

  it("finishes an older About page only after its heading, summary and primary action render", async () => {
    const f = fixture({
      pathname: "/about",
      selectors: new Set([...headerlessShell, ...aboutContent]),
    });
    await f.settle();
    await f.flushFrames();
    expect(f.calls.filter((call) => call.event === "ready")).toHaveLength(1);
  });

  it("lets a reloaded older headerless route become ready when its form renders later", async () => {
    const selectors = new Set(headerlessShell);
    const f = fixture({ pathname: "/login", loading: true, selectors });
    await f.settle();
    await f.flushFrames();
    expect(f.calls.some((call) => call.event === "ready")).toBe(false);
    f.document.readyState = "complete";
    authContent.forEach((selector) => selectors.add(selector));
    f.timers.find((timer) => timer.delay === 200).callback();
    await f.flushFrames();
    expect(f.calls.filter((call) => call.event === "ready")).toHaveLength(1);
    expect(
      f.calls.some((call) => call.event === "retry" || call.event === "open"),
    ).toBe(false);
  });

  it("keeps an older headerless loading shell covered until its required form controls exist", async () => {
    const f = fixture({
      pathname: "/signup",
      selectors: new Set([...headerlessShell, authContent[0]]),
    });
    await f.settle();
    await f.flushFrames();
    f.timers.find((timer) => timer.delay === 12000).callback();
    expect(f.calls.some((call) => call.event === "ready")).toBe(false);
    expect(f.attributes.get("data-wallpaper-startup")).toBe("loading");
    expect(f.retry.hidden).toBe(false);
  });

  it("does not accept an empty main, a partial About page or an unrecognized headerless route", async () => {
    for (const options of [
      {
        pathname: "/login",
        selectors: new Set([
          "#app .app",
          "#app .app.no-header",
          ...authContent,
        ]),
      },
      {
        pathname: "/about",
        selectors: new Set([...headerlessShell, aboutContent[0]]),
      },
      {
        pathname: "/unknown",
        selectors: new Set([...headerlessShell, ...authContent]),
      },
    ]) {
      const f = fixture(options);
      await f.settle();
      await f.flushFrames();
      expect(f.calls.some((call) => call.event === "ready")).toBe(false);
    }
  });

  it("does not automatically restart a timed-out local page or send a late ready after leaving", async () => {
    const f = fixture({ local: true, timedOut: true });
    f.window.__WALLPAPER_STARTUP__.start();
    await f.settle();
    await f.flushFrames();
    expect(f.calls.some((call) => call.event === "open")).toBe(false);
    expect(f.retry.hidden).toBe(false);
    const remote = fixture();
    await remote.settle();
    remote.listeners.get("pagehide")();
    remote.attributes.set("data-wallpaper-app-ready", "true");
    remote.listeners.get("wallpaper:app-ready")?.();
    await remote.flushFrames();
    expect(remote.calls.some((call) => call.event === "ready")).toBe(false);
  });
});

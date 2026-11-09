import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(
  process.env.PAKE_THEME_SCRIPT ||
    new URL("../../src-tauri/src/inject/theme_refresh.js", import.meta.url),
  "utf8",
);
function setup(preference, color) {
  const attrs = new Map([
    ["data-theme-mode", preference],
    ["data-theme", color],
  ]);
  let domReady, observerCallback, mediaCallback;
  const timers = new Map(),
    calls = [];
  let sequence = 0;
  const element = {
    classList: { contains: () => false },
    getAttribute: (name) => attrs.get(name),
    style: {},
  };
  vm.runInNewContext(source, {
    document: {
      documentElement: element,
      body: { ...element },
      addEventListener: (_name, fn) => {
        domReady = fn;
      },
    },
    window: {
      __TAURI__: {
        core: {
          invoke: (_name, params) => {
            calls.push(params.mode);
            return Promise.resolve();
          },
        },
      },
      matchMedia: () => ({
        addEventListener: (_name, fn) => {
          mediaCallback = fn;
        },
      }),
    },
    MutationObserver: class {
      constructor(fn) {
        observerCallback = fn;
      }
      observe() {}
    },
    setTimeout: (fn) => {
      timers.set(++sequence, fn);
      return sequence;
    },
    clearTimeout: (id) => timers.delete(id),
  });
  function flush() {
    const callbacks = [...timers.values()];
    timers.clear();
    callbacks.forEach((fn) => fn());
  }
  domReady();
  flush();
  return {
    calls,
    change(mode, nextColor) {
      attrs.set("data-theme-mode", mode);
      attrs.set("data-theme", nextColor);
      observerCallback();
      flush();
    },
    systemChange() {
      mediaCallback();
    },
  };
}

test("system keeps the native window following the OS instead of pinning the resolved color", () => {
  const state = setup("system", "light");
  assert.deepEqual(state.calls, ["system"]);
  state.change("system", "dark");
  state.systemChange();
  assert.deepEqual(state.calls, ["system"]);
});
test("manual dark/light can return to system and clear the native override", () => {
  const state = setup("dark", "dark");
  state.change("light", "light");
  state.change("system", "light");
  assert.deepEqual(state.calls, ["dark", "light", "system"]);
});
test("sites without an explicit system preference retain existing DOM theme behavior", () => {
  const state = setup(undefined, "dark");
  state.change(undefined, "light");
  assert.deepEqual(state.calls, ["dark", "light"]);
});

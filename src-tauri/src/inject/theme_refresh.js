document.addEventListener("DOMContentLoaded", () => {
  let lastMode = null;
  const debounce = (func, wait) => {
    let timeout;
    return (...args) => {
      clearTimeout(timeout);
      timeout = setTimeout(() => func(...args), wait);
    };
  };

  const updateTheme = () => {
    const doc = document.documentElement;
    const body = document.body;
    let mode = null;
    // A resolved DOM color is not a manual override when the site follows the OS.
    const preference = doc.getAttribute("data-theme-mode");

    // Check for explicit theme classes or attributes
    const isDark =
      doc.classList.contains("dark") ||
      body.classList.contains("dark") ||
      doc.getAttribute("data-theme") === "dark" ||
      body.getAttribute("data-theme") === "dark" ||
      doc.style.colorScheme === "dark";

    const isLight =
      doc.classList.contains("light") ||
      body.classList.contains("light") ||
      doc.getAttribute("data-theme") === "light" ||
      body.getAttribute("data-theme") === "light" ||
      doc.style.colorScheme === "light";

    if (preference === "system") mode = "system";
    else if (isDark) mode = "dark";
    else if (isLight) mode = "light";

    // Sync explicit modes, including clearing the override for system preference
    if (mode && mode !== lastMode && window.__TAURI__?.core) {
      lastMode = mode;
      window.__TAURI__.core
        .invoke("update_theme_mode", { mode })
        .catch((error) => {
          console.warn("[Pake] Failed to synchronize theme:", error);
          if (lastMode === mode) lastMode = null;
        });
    }
  };

  const debouncedUpdateTheme = debounce(updateTheme, 200);

  // Initial check with delay to allow site to render
  setTimeout(updateTheme, 500);

  // Watch for DOM changes
  const observer = new MutationObserver(debouncedUpdateTheme);
  const config = {
    attributes: true,
    attributeFilter: ["class", "data-theme", "data-theme-mode", "style"],
    subtree: false,
  };

  observer.observe(document.documentElement, config);
  observer.observe(document.body, config);

  // Watch for system theme changes (though window should handle this natively now)
  window
    .matchMedia("(prefers-color-scheme: dark)")
    .addEventListener("change", updateTheme);
});

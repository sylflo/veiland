// strip the top-of-file comment banner from an embedded example config
window.veilandTomlText = (name) => {
  const el = document.getElementById("toml-" + name);
  if (!el) return "";
  const lines = el.textContent.split("\n");
  const i = lines.findIndex((l) => /^(# ---|\[)/.test(l));
  return lines.slice(i < 0 ? 0 : i).join("\n").trim();
};

(() => {
  "use strict";
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const DPR = Math.min(window.devicePixelRatio || 1, 1.5);
  const TAU = Math.PI * 2;
  const R = Math.random;

  /* ---------- clock ---------- */
  const clockEl = document.getElementById("clock");
  const dateEl = document.getElementById("clock-date");
  function tickClock() {
    const d = new Date();
    clockEl.textContent =
      String(d.getHours()).padStart(2, "0") + ":" + String(d.getMinutes()).padStart(2, "0");
    dateEl.textContent = d
      .toLocaleDateString(undefined, { weekday: "long", month: "long", day: "numeric" })
      .toLowerCase();
  }
  tickClock();
  setInterval(tickClock, 5000);

  /* ---------- password pill ---------- */
  const pill = document.getElementById("pill");
  const placeholder = document.getElementById("pill-placeholder");
  let dots = 0, demo = true, demoTimer = null;

  function renderDots() {
    pill.querySelectorAll(".dot").forEach((d) => d.remove());
    placeholder.style.display = dots ? "none" : "";
    for (let i = 0; i < dots; i++) {
      const d = document.createElement("span");
      d.className = "dot";
      pill.appendChild(d);
    }
  }
  function shakePill() {
    pill.classList.remove("shake");
    void pill.offsetWidth;
    pill.classList.add("shake");
    setTimeout(() => { dots = 0; renderDots(); }, 380);
  }
  function demoLoop() {
    if (!demo) return;
    if (dots < 6) { dots++; renderDots(); demoTimer = setTimeout(demoLoop, 260 + R() * 220); }
    else { demoTimer = setTimeout(() => { if (!demo) return; shakePill(); demoTimer = setTimeout(demoLoop, 2200); }, 1100); }
  }
  if (!reduced) demoTimer = setTimeout(demoLoop, 1600);

  const showcase = document.getElementById("showcase");
  window.addEventListener("keydown", (e) => {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const r = showcase.getBoundingClientRect();
    if (r.bottom < 0 || r.top > innerHeight) return;
    if (e.key.length === 1) {
      demo = false; clearTimeout(demoTimer);
      if (dots < 24) { dots++; renderDots(); }
      if (e.key === " ") e.preventDefault();
    } else if (e.key === "Backspace") {
      demo = false; clearTimeout(demoTimer);
      if (dots > 0) { dots--; renderDots(); }
    } else if (e.key === "Enter" && dots > 0) {
      shakePill();
    }
  });
})();

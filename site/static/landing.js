// SPDX-License-Identifier: GPL-3.0-or-later
// Landing-page helper: strip the top-of-file comment banner from an
// embedded example config before showing it in the popup.
window.veilandTomlText = (name) => {
  const el = document.getElementById("toml-" + name);
  if (!el) return "";
  const lines = el.textContent.split("\n");
  const i = lines.findIndex((l) => /^(# ---|\[)/.test(l));
  return lines.slice(i < 0 ? 0 : i).join("\n").trim();
};

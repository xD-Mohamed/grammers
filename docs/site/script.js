"use strict";

const status = document.getElementById("copy-status");
document.querySelectorAll(".copy").forEach((button) => {
  button.addEventListener("click", async () => {
    const code = button.closest(".code").querySelector("pre code").textContent;
    try {
      await navigator.clipboard.writeText(code);
      button.textContent = "Copied";
      status.textContent = "Code copied to clipboard.";
    } catch {
      button.textContent = "Select code";
      status.textContent = "Clipboard unavailable. Select the code to copy it.";
      const selection = window.getSelection();
      const range = document.createRange();
      range.selectNodeContents(button.closest(".code").querySelector("pre code"));
      selection.removeAllRanges();
      selection.addRange(range);
    }
    window.setTimeout(() => { button.textContent = "Copy"; }, 2000);
  });
});

document.addEventListener("keydown", (event) => {
  const editing = event.target instanceof Element && event.target.closest("input, textarea, [contenteditable]");
  if (event.key === "/" && !editing && !event.ctrlKey && !event.metaKey && !event.altKey) {
    event.preventDefault();
    document.getElementById("api-search").focus();
  }
});

const navigation = document.querySelector(".navigation");
const mobile = window.matchMedia("(max-width: 800px)");
const adaptNavigation = () => { navigation.open = !mobile.matches; };
adaptNavigation();
mobile.addEventListener("change", adaptNavigation);

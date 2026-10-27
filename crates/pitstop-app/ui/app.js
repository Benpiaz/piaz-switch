// Piaz Switch's window: draws the app's snapshot and asks the app for changes.
// Everything it shows comes from Rust as names, emails, plans and usage;
// no token ever reaches this page.
"use strict";

const tauri = window.__TAURI__;
const invoke = (command, args) => tauri.core.invoke(command, args);
const listen = (name, handler) => tauri.event.listen(name, (event) => handler(event.payload));

// ---------- state ----------

let snap = null;
let pane = "accounts";
let shownPane = null; // the pane the last render drew, to reset scrolling
let selected = null; // "provider|name" of the selected row
const switching = new Set(); // rows a switch is under way for
let notices = []; // what this window was told: switches, additions
let noticeSerial = 0;
let dialog = null;
let menuEl = null;
let menuReturn = null; // focus key of what opened the menu

// What the window showed last, so a re-render never replays an entrance:
// only new callouts rise in, and a bar moves only when its value changed.
let shownCallouts = new Set();
const shownPercents = new Map();
const shownEmails = new Set(); // rows whose address was clicked into view

const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
const systemDark = window.matchMedia("(prefers-color-scheme: dark)");
const nowSecs = () => Math.floor(Date.now() / 1000);
const rowKey = (provider, name) => `${provider}|${name}`;
const NAME_RULE = /^[A-Za-z0-9_-]{1,64}$/;
const APP_NAME = "Piaz Switch";

// The project's links, opened by name: the app owns the addresses.
const AUTHOR = "Ben Piaz";
const LINKS = [
  ["website", "globe", () => t("linkWebsite")],
  ["repo", "github", () => "GitHub"],
  ["youtube", "youtube", () => "YouTube @benpiaz"],
  ["x", "xlogo", () => "X @benpiazai"],
];

// ---------- DOM ----------

function h(tag, attrs, ...children) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === undefined || v === null || v === false) continue;
    if (k === "class") el.className = v;
    else if (k === "text") el.textContent = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, "");
    else el.setAttribute(k, v);
  }
  for (const child of children.flat(Infinity)) {
    if (child === null || child === undefined || child === false) continue;
    el.append(child instanceof Node ? child : document.createTextNode(String(child)));
  }
  return el;
}

const ICONS = {
  accounts:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><circle cx="9" cy="8" r="3.2"/><path d="M3.5 19c0-3.2 2.5-5.5 5.5-5.5s5.5 2.3 5.5 5.5"/><circle cx="16.6" cy="9" r="2.6"/><path d="M15.6 13.6c.3-.1.7-.1 1-.1 2.7 0 4.4 2 4.4 4.9"/></svg>',
  settings:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/></svg>',
  refresh:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M20 12a8 8 0 1 1-2.34-5.66"/><path d="M20 4v5h-5"/></svg>',
  plus: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>',
  user: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="8.6" r="3.6"/><path d="M5.6 19.4c1.3-3.1 3.6-4.7 6.4-4.7s5.1 1.6 6.4 4.7"/></svg>',
  warning:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round"><path d="M10.3 3.9 2.4 17.6a2 2 0 0 0 1.7 3h15.8a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z"/><path d="M12 9v4.5M12 17h.01"/></svg>',
  error:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round" stroke-linecap="round"><path d="M8 2.5h8L21.5 8v8L16 21.5H8L2.5 16V8z"/><path d="M9 9l6 6M15 9l-6 6"/></svg>',
  check:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>',
  x: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round"><path d="M6 6l12 12M18 6 6 18"/></svg>',
  info: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="12" cy="12" r="9.5"/><path d="M12 11v6M12 7.5h.01"/></svg>',
  swap: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M7 7h12l-3-3M17 17H5l3 3"/></svg>',
  tag: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linejoin="round"><path d="M3 12V4.5A1.5 1.5 0 0 1 4.5 3H12l9 9-9 9z"/><circle cx="7.5" cy="7.5" r="1.3"/></svg>',
  trash:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 7h16M10 11v6M14 11v6M5.5 7l1 12a2 2 0 0 0 2 1.8h7a2 2 0 0 0 2-1.8l1-12M9 7V4.5A1.5 1.5 0 0 1 10.5 3h3A1.5 1.5 0 0 1 15 4.5V7"/></svg>',
  link: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M10 14a4.5 4.5 0 0 0 6.4 0l3-3a4.5 4.5 0 0 0-6.4-6.4l-1 1"/><path d="M14 10a4.5 4.5 0 0 0-6.4 0l-3 3a4.5 4.5 0 0 0 6.4 6.4l1-1"/></svg>',
  external:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M14 4h6v6M20 4l-9 9"/><path d="M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/></svg>',
  flame:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 21c-3.6 0-6.5-2.6-6.5-6.2 0-3.4 2.4-5.4 3.6-8.3.4 1.9 1.4 3 2.6 3.6C11.5 7 12.6 4.6 14.6 3c.3 3.2 3.9 5.6 3.9 11.2 0 3.9-2.9 6.8-6.5 6.8z"/><path d="M12 21c-1.6 0-2.8-1.2-2.8-2.8 0-1.8 1.5-2.6 2.2-4.2.9 1 3.4 2.2 3.4 4.4 0 1.5-1.2 2.6-2.8 2.6z"/></svg>',
  layers:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3.5 3.5 8 12 12.5 20.5 8z"/><path d="m3.5 12 8.5 4.5 8.5-4.5"/><path d="m3.5 16 8.5 4.5 8.5-4.5"/></svg>',
  clock:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 7.5V12l3 2"/></svg>',
  globe:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a14 14 0 0 1 0 18M12 3a14 14 0 0 0 0 18"/></svg>',
  github:
    '<svg viewBox="0 0 24 24"><path fill="currentColor" d="M12 .5C5.73.5.5 5.73.5 12a11.5 11.5 0 0 0 7.86 10.92c.58.1.79-.25.79-.56v-2c-3.2.7-3.88-1.37-3.88-1.37-.53-1.33-1.28-1.69-1.28-1.69-1.05-.72.08-.7.08-.7 1.16.08 1.77 1.19 1.77 1.19 1.03 1.77 2.7 1.26 3.36.96.1-.75.4-1.26.73-1.55-2.55-.29-5.24-1.28-5.24-5.68 0-1.26.45-2.28 1.19-3.09-.12-.29-.52-1.46.11-3.04 0 0 .97-.31 3.17 1.18a11 11 0 0 1 5.77 0c2.2-1.49 3.17-1.18 3.17-1.18.63 1.58.23 2.75.11 3.04.74.81 1.19 1.83 1.19 3.09 0 4.41-2.69 5.39-5.25 5.67.41.36.78 1.06.78 2.14v3.17c0 .31.21.67.8.56A11.5 11.5 0 0 0 23.5 12C23.5 5.73 18.27.5 12 .5Z"/></svg>',
  youtube:
    '<svg viewBox="0 0 24 24"><path fill="currentColor" d="M23.5 6.2a3 3 0 0 0-2.1-2.1C19.5 3.6 12 3.6 12 3.6s-7.5 0-9.4.5A3 3 0 0 0 .5 6.2 31 31 0 0 0 0 12a31 31 0 0 0 .5 5.8 3 3 0 0 0 2.1 2.1c1.9.5 9.4.5 9.4.5s7.5 0 9.4-.5a3 3 0 0 0 2.1-2.1A31 31 0 0 0 24 12a31 31 0 0 0-.5-5.8ZM9.6 15.6V8.4l6.2 3.6-6.2 3.6Z"/></svg>',
  xlogo:
    '<svg viewBox="0 0 24 24"><path fill="currentColor" d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z"/></svg>',
  mark: '<svg viewBox="0 0 32 32"><rect width="32" height="32" rx="9" fill="currentColor"/><g fill="none" stroke="#fff" stroke-width="2.3" stroke-linecap="round"><circle cx="16" cy="17.6" r="7.4"/><path d="M16 17.6v-4.4M16 17.6l3.1 2.2M13.4 6.6h5.2M16 6.6v3.4"/></g></svg>',
};

function icon(name) {
  const holder = document.createElement("span");
  holder.innerHTML = ICONS[name] || ICONS.info;
  return holder.firstChild;
}

/** A button; `kind` is any of primary, danger, ghost, compact. */
function button(label, run, kind) {
  return h("button", { class: "button" + (kind ? ` ${kind}` : ""), onclick: run }, label);
}

/** Mark an element so focus can find its way back to it after a re-render. */
function keyed(el, key) {
  el.dataset.focus = key;
  return el;
}

function focusKeyOf(el) {
  if (!el || el === document.body || !el.dataset) return null;
  if (el.dataset.key) return `key:${el.dataset.key}`;
  if (el.dataset.focus) return `focus:${el.dataset.focus}`;
  return null;
}

function findByFocusKey(root, focusKey) {
  if (!focusKey) return null;
  const at = focusKey.indexOf(":");
  const kind = focusKey.slice(0, at);
  const value = CSS.escape(focusKey.slice(at + 1));
  return root.querySelector(kind === "key" ? `[data-key="${value}"]` : `[data-focus="${value}"]`);
}

/** Put focus back on the element that stands where the focused one stood. */
function refocus(root, focusKey) {
  const active = document.activeElement;
  if (!focusKey || (active && active !== document.body && active.isConnected)) return;
  const el = findByFocusKey(root, focusKey);
  if (el && !el.disabled) el.focus({ preventScroll: true });
}

function announce(text) {
  const live = document.getElementById("announcer");
  live.textContent = "";
  requestAnimationFrame(() => {
    live.textContent = text;
  });
}

/** Remove `el` once its exit animation ends (or straight away without one). */
function leave(el, ms) {
  if (reducedMotion.matches) ms = Math.min(ms, 100);
  el.classList.add("leave");
  const done = () => el.remove();
  el.addEventListener("animationend", (event) => event.target === el && done());
  setTimeout(done, ms + 120);
}

let toastEl = null;

/** A brief note at the bottom; `iconName` says what kind (a check by default). */
function toast(text, iconName) {
  if (toastEl) toastEl.remove();
  const el = h("div", { class: "toast", "aria-hidden": "true" }, icon(iconName || "check"), text);
  toastEl = el;
  document.body.append(el);
  announce(text);
  setTimeout(() => {
    if (toastEl === el) toastEl = null;
    leave(el, 160);
  }, 1600);
}

// The copy button that just copied shows a check for a moment instead of a
// toast; `copiedFresh` lets the check draw itself only on its first render.
let copiedKey = null;
let copiedFresh = false;
let copiedTimer = 0;

/** Copy `text`. With `key` (a copyButton's), that button confirms it; without, a toast does. */
async function copy(text, key) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const area = h("textarea");
    area.value = text;
    document.body.append(area);
    area.select();
    document.execCommand("copy");
    area.remove();
  }
  if (!key) return toast(t("copied"));
  announce(t("copied"));
  copiedKey = key;
  copiedFresh = true;
  clearTimeout(copiedTimer);
  copiedTimer = setTimeout(() => {
    copiedKey = null;
    if (dialog) renderDialog();
  }, 1800);
  if (dialog) renderDialog();
}

/** A compact button that copies `text`, then reads "Copied" with a check until it settles back. */
function copyButton(text, label, key) {
  const done = copiedKey === key;
  const mark = icon(done ? "check" : "link");
  if (done && copiedFresh) {
    mark.querySelectorAll("path").forEach((p) => p.setAttribute("pathLength", "1"));
    copiedFresh = false;
  }
  return keyed(
    h(
      "button",
      { class: "button compact" + (done ? " copied" : ""), disabled: !text, onclick: () => copy(text, key) },
      mark,
      done ? t("copied") : label,
    ),
    key,
  );
}

// ---------- words ----------

/** `5h` as a row label; with `long`, as a sentence says it (`5-hour`). */
function windowName(label, long) {
  const [head, ...rest] = label.split(" ");
  const names = { "5h": "window5h", week: "windowWeek", month: "windowMonth" };
  const name = names[head] ? t(names[head] + (long ? "Long" : "")) : head;
  return [name, ...rest].join(" ");
}

function duration(secs) {
  const d = Math.floor(secs / 86400);
  const hours = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d) return `${d}d ${hours}h`;
  if (hours) return `${hours}h ${String(m).padStart(2, "0")}m`;
  return `${Math.max(m, 1)}m`;
}

function ago(at) {
  const secs = nowSecs() - at;
  if (secs < 60) return t("agoNow");
  if (secs < 3600) return t("agoMinutes", { n: Math.floor(secs / 60) });
  if (secs < 86400) return t("agoHours", { n: Math.floor(secs / 3600) });
  const days = Math.floor(secs / 86400);
  return days === 1 ? t("agoDay") : t("agoDays", { n: days });
}

function clockTime(at) {
  return new Date(at * 1000).toLocaleTimeString(LANG === "vi" ? "vi-VN" : "en-GB", {
    hour: "2-digit",
    minute: "2-digit",
  });
}

function updatedText() {
  if (!snap) return "";
  if (snap.reading) return t("reading");
  if (snap.updatedAt) return t("updated", { time: clockTime(snap.updatedAt) });
  return t("notReadYet");
}

/** Where an even use of a limit would be by now, and which side of it the
 * limit is on. None where the window's length or reset is unknown. */
function paceOf(w, now) {
  if (!w.lengthSecs || !w.resetsAt || w.resetsAt <= now) return null;
  const left = w.resetsAt - now;
  if (left > w.lengthSecs) return null;
  const expected = 100 * (1 - left / w.lengthSecs);
  const diff = w.percent - expected;
  const standing = Math.abs(diff) <= 3 ? "even" : diff > 0 ? "over" : "under";
  return { expected, diff, standing };
}

function toolOf(provider) {
  return snap && snap.tools.find((tool) => tool.id === provider);
}

function suggestedName(email, tool) {
  const local = (email || "").split("@")[0].replace(/[^A-Za-z0-9_-]+/g, "-").replace(/^-+|-+$/g, "");
  const base = (local || "account").slice(0, 60);
  const taken = new Set((tool ? tool.accounts : []).map((a) => a.name.toLowerCase()));
  let name = base;
  for (let n = 2; taken.has(name.toLowerCase()); n++) name = `${base}-${n}`;
  return name;
}

// ---------- theme ----------

// "system" resolves here into a concrete data-theme, and keeps following the
// system while chosen. The last choice is kept locally too, so the first frame
// paints in it before the app's snapshot arrives.
let themeChoice = localStorage.getItem("theme") || "system";

function applyTheme(choice) {
  themeChoice = choice;
  localStorage.setItem("theme", choice);
  const dark = choice === "dark" || (choice === "system" && systemDark.matches);
  document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
}

applyTheme(themeChoice);
systemDark.addEventListener("change", () => applyTheme(themeChoice));

// ---------- the window ----------

// A render replaces the buttons: one landing between the press and the
// release of a click would swallow it, so it waits for the release.
let pressing = false;
let renderWaiting = false;
let pressTimer = 0;
const released = () => {
  pressing = false;
  clearTimeout(pressTimer);
  if (renderWaiting) {
    renderWaiting = false;
    setTimeout(render);
  }
};
window.addEventListener("pointerdown", () => {
  pressing = true;
  // A release outside the window may never arrive: never hold renders long.
  clearTimeout(pressTimer);
  pressTimer = setTimeout(released, 2000);
}, true);
window.addEventListener("pointerup", released, true);
window.addEventListener("pointercancel", released, true);
window.addEventListener("blur", released);

function render() {
  if (pressing) {
    renderWaiting = true;
    return;
  }
  if (snap) {
    setLanguage(snap.language);
    applyTheme(snap.themeChoice || "system");
  }
  const focusKey = focusKeyOf(document.activeElement);
  renderNav();
  renderHead();
  renderPane();
  refocus(document.querySelector(".app"), focusKey);
}

function renderNav() {
  const item = (id, iconName, label) =>
    keyed(
      h(
        "button",
        {
          class: "nav-item",
          "aria-current": pane === id ? "page" : null,
          onclick: () => {
            pane = id;
            render();
          },
        },
        icon(iconName),
        h("span", { text: label }),
      ),
      `nav-${id}`,
    );
  document.getElementById("nav").replaceChildren(item("accounts", "accounts", t("accounts")), item("usage", "usage", t("usage")), item("settings", "settings", t("settings")));
  document
    .getElementById("brand")
    .replaceChildren(
      h("span", { class: "brand-mark", "aria-hidden": "true" }, icon("mark")),
      h(
        "div",
        null,
        h("div", { class: "brand-name", text: APP_NAME }),
        h("div", { class: "brand-version", text: snap ? `${snap.version} · ${t("byAuthor", { author: AUTHOR })}` : t("byAuthor", { author: AUTHOR }) }),
      ),
    );
}

function renderHead() {
  if (pane === "usage") {
    renderUsageHead();
    return;
  }
  const accounts = pane === "accounts";
  document.getElementById("title").textContent = accounts ? t("accounts") : t("settings");
  const status = document.getElementById("status");
  if (accounts && snap) {
    status.replaceChildren(...[snap.reading ? h("span", { class: "status-dot", "aria-hidden": "true" }) : null, h("span", { text: updatedText() })].filter(Boolean));
  } else {
    status.replaceChildren();
  }
  const toolbar = document.getElementById("toolbar");
  if (!accounts) {
    toolbar.replaceChildren();
    return;
  }
  const refresh = h(
    "button",
    {
      class: "icon-button" + (snap && snap.reading ? " spinning" : ""),
      title: t("refreshHelp"),
      "aria-label": t("refresh"),
      disabled: !snap || snap.reading,
      onclick: () => invoke("refresh", { usage: true }),
    },
    icon("refresh"),
  );
  const add = h("button", { class: "button", title: t("addAccountHelp"), onclick: () => openAdd() }, icon("plus"), t("addAccount"));
  toolbar.replaceChildren(keyed(refresh, "refresh"), keyed(add, "add"));
}

function renderPane() {
  const root = document.getElementById("pane");
  const focusKey = focusKeyOf(document.activeElement);
  const scrollTop = shownPane === pane ? root.scrollTop : 0;
  const moves = []; // bar movements to start once the rows are in the page
  root.replaceChildren(pane === "settings" ? renderSettings() : pane === "usage" ? renderUsage(moves) : renderAccounts(moves));
  shownPane = pane;
  root.scrollTop = scrollTop;
  refocus(root, focusKey);
  for (const move of moves) move();
  updateFades();
}

/** Fade the pane's edges only where there is more to scroll. */
function updateFades() {
  const root = document.getElementById("pane");
  const more = root.scrollHeight - root.clientHeight;
  root.style.setProperty("--fade-top", root.scrollTop > 2 ? "22px" : "0px");
  root.style.setProperty("--fade-bottom", root.scrollTop < more - 2 ? "28px" : "0px");
}

/** Placeholder rows shaped like the account list, breathing while the first read runs. */
function skeleton() {
  // The CSP allows no style attributes, so the stagger and widths go through CSSOM.
  const styled = (el, props) => {
    for (const [k, v] of Object.entries(props)) el.style.setProperty(k, v);
    return el;
  };
  const row = (i, wide) =>
    styled(
      h(
        "div",
        { class: "row skeleton-row" },
        h("div", { class: "avatar sk" }),
        h("div", { class: "identity" }, styled(h("div", { class: "sk sk-line" }), { width: `${wide}%` }), h("div", { class: "sk sk-line thin" })),
        h("div", { class: "meters sk-meters" }, h("div", { class: "sk sk-bar" }), h("div", { class: "sk sk-bar short" })),
      ),
      { "--i": String(i) },
    );
  return h(
    "div",
    { class: "accounts", "aria-busy": "true" },
    h("p", { class: "sr-only", text: t("readingAccounts") }),
    h(
      "div",
      { class: "group", "aria-hidden": "true" },
      h("div", { class: "group-head" }, h("div", { class: "sk sk-line head" })),
      h("div", { class: "panel" }, row(1, 34), row(2, 26), row(3, 40)),
    ),
  );
}

function empty({ iconName, title, detail, action, tone }) {
  return h(
    "div",
    { class: "empty" + (tone ? ` ${tone}` : "") },
    h("div", { class: "empty-badge", "aria-hidden": "true" }, icon(iconName)),
    h("h2", { text: title }),
    detail ? h("p", { text: detail }) : null,
    action ? keyed(action, "empty-action") : null,
  );
}

function renderAccounts(moves) {
  if (!snap) return skeleton();
  if (snap.error) {
    return empty({
      iconName: "error",
      tone: "error",
      title: t("readFailed"),
      detail: errorText(snap.error),
      action: button(t("tryAgain"), () => invoke("refresh", { usage: true })),
    });
  }

  const callouts = [];
  for (const suggestion of snap.suggestions) callouts.push(suggestionNotice(suggestion));
  for (const notice of notices) callouts.push(noticeRow(notice));
  for (const tool of snap.tools) {
    if (tool.live.state === "unmanaged") callouts.push(nameTip(tool));
  }
  const keys = new Set(callouts.map((el) => el.dataset.callout));
  for (const el of callouts) {
    if (!shownCallouts.has(el.dataset.callout)) el.classList.add("enter");
  }
  shownCallouts = keys;

  const shown = snap.tools.filter((tool) => tool.accounts.length);
  if (!shown.length && !callouts.length) {
    return empty({
      iconName: "accounts",
      title: t("noAccounts"),
      detail: t("noAccountsDetail"),
      action: button([icon("plus"), t("addAccount")], () => openAdd(), "primary"),
    });
  }
  const list = h("div", { class: "accounts" });
  if (callouts.length) list.append(h("div", { class: "callouts" }, callouts));
  for (const tool of shown) {
    list.append(
      h(
        "section",
        { class: "group", "aria-label": tool.name },
        h(
          "div",
          { class: "group-head" },
          h(
            "h2",
            { class: "group-title" },
            toolLogo(tool.id),
            h("span", { text: tool.name }),
            h("span", { class: "count", "aria-hidden": "true", text: String(tool.accounts.length) }),
          ),
          groupAdd(tool),
        ),
        h(
          "div",
          { class: "panel" },
          poolRow(tool, moves),
          tool.accounts.map((account) => accountRow(tool, account, moves)),
        ),
      ),
    );
  }
  return list;
}

/** Adding an account where it will show, with its tool already chosen. */
function groupAdd(tool) {
  if (tool.installed === false) return null;
  const label = t("addAccountTo", { tool: tool.name });
  const add = h("button", { class: "button compact ghost group-add", title: label, "aria-label": label, onclick: () => openAdd(tool.id) }, icon("plus"), t("add"));
  return keyed(add, `add:${tool.id}`);
}

function accountRow(tool, account, moves) {
  const key = rowKey(tool.id, account.name);
  const usage = account.usage;
  const needsSignIn = usage && usage.status === "unauthorized";
  return h(
    "div",
    {
      class: "row" + (account.inUse ? " in-use" : "") + (needsSignIn ? " attention" : "") + (selected === key ? " selected" : ""),
      tabindex: "0",
      "data-key": key,
      onclick: () => select(key),
      ondblclick: (event) => {
        if (!event.target.closest("button")) primaryAction(tool, account);
      },
      oncontextmenu: (event) => {
        event.preventDefault();
        select(key);
        openRowMenu(event.clientX, event.clientY, tool, account);
      },
      onkeydown: (event) => onRowKey(event, tool, account),
    },
    h("div", { class: "avatar", "aria-hidden": "true" }, icon(needsSignIn ? "warning" : "user")),
    h(
      "div",
      { class: "identity" },
      h("div", { class: "row-title" }, h("span", { class: "row-name", text: account.name }), account.plan ? h("span", { class: "tag", text: account.plan }) : null),
      account.label && account.label !== account.name ? h("div", { class: "row-email" }, emailMask(key, account.label)) : null,
    ),
    h("div", { class: "row-side" }, sideOf(tool, account)),
    metersOf(tool, account, moves),
    notesOf(tool, account),
  );
}

/** An address kept blurred until clicked, so a shared or recorded screen
 * doesn't show it; clicking again blurs it, and so does leaving the window. */
function emailMask(key, email) {
  const shown = shownEmails.has(key);
  const mask = h(
    "button",
    {
      class: "email-mask" + (shown ? " shown" : ""),
      "aria-pressed": shown ? "true" : "false",
      title: t(shown ? "hideEmail" : "showEmail"),
      onclick: () => {
        const now = !shownEmails.has(key);
        if (now) shownEmails.add(key);
        else shownEmails.delete(key);
        mask.classList.toggle("shown", now);
        mask.setAttribute("aria-pressed", now ? "true" : "false");
        mask.title = t(now ? "hideEmail" : "showEmail");
      },
    },
    h("span", { class: "email-text", text: email }),
  );
  return keyed(mask, `email:${key}`);
}

function hideEmails() {
  shownEmails.clear();
  for (const mask of document.querySelectorAll(".email-mask.shown")) {
    mask.classList.remove("shown");
    mask.setAttribute("aria-pressed", "false");
    mask.title = t("showEmail");
  }
}

function sideOf(tool, account) {
  if (switching.has(rowKey(tool.id, account.name))) return h("div", { class: "spinner", role: "status", "aria-label": t("switching") });
  if (account.inUse) return h("span", { class: "in-use-pill" }, icon("check"), t("inUse"));
  if (account.usage && account.usage.status === "unauthorized") {
    return button(t("signInAgain"), () => openSignInAgain(tool, account), "compact");
  }
  const use = button(t("use"), () => useAccount(tool, account), "compact");
  // Its login is lent to a warm-up that may renew it: a switch waits for it.
  const warm = warmOf(tool, account);
  if (warm && warm.standing && warm.standing.state === "warming") {
    use.disabled = true;
    use.title = t("useAfterWarm");
  }
  return use;
}

/** One line per limit: name, bar with the even-pace tick, figure, timing. */
function metersOf(tool, account, moves) {
  const windows = shownWindows(account.usage);
  if (!windows.length) return null;
  const now = nowSecs();
  const grid = h("div", { class: "meters" });
  for (const w of windows) {
    // Past its reset a limit has started afresh, before the next read says so.
    const fresh = Boolean(w.resetsAt && w.resetsAt <= now);
    const resets = fresh
      ? t("justReset")
      : w.resetsAt && w.resetsAt > now
        ? t("resetsIn", { d: duration(w.resetsAt - now) })
        : w.idle
          ? t("notStarted")
          : "";
    meterLine(grid, moves, {
      id: `${rowKey(tool.id, account.name)}|${w.label}`,
      label: windowName(w.label),
      used: fresh ? 0 : w.percent,
      pace: fresh ? null : paceOf(w, now),
      resets,
      resetsHelp: w.idle ? t("notStartedHelp") : undefined,
    });
  }
  return grid;
}

/** The limits an account lists as its own: those read, less the
 * per-model ones nobody has used yet (`quiet`), which only totals count. */
function shownWindows(usage) {
  return usage && usage.status === "ok" ? usage.windows.filter((w) => !w.quiet) : [];
}

/** How much of a limit a plan holds next to a plain one: the `5` of
 * `Max 5x` or `Team 5x`; any plan without such a multiple counts as 1. */
function planWeight(plan) {
  const m = /(\d+)x$/i.exec((plan || "").trim());
  return m ? Number(m[1]) : 1;
}

/** The accounts of a tool whose limits can be read, when there are enough
 * of them to pool. */
function pooledOf(tool) {
  const read = tool.accounts.filter((a) => shownWindows(a.usage).length);
  return read.length >= 2 ? read : [];
}

/** One row ahead of a tool's accounts: each limit over all of them at
 * once, every account weighed by its plan's size, so the pace reads for
 * the pool rather than one account at a time. A limit's pace is only
 * shown when every account's is known. */
function poolRow(tool, moves) {
  const accounts = pooledOf(tool);
  if (!accounts.length) return null;
  const now = nowSecs();
  const count = accounts.length;
  const grid = h("div", { class: "meters" });
  const labels = [];
  for (const a of accounts) for (const w of a.usage.windows) if (!labels.includes(w.label)) labels.push(w.label);
  for (const label of labels) {
    const found = accounts.map((a) => [a, a.usage.windows.find((x) => x.label === label)]).filter(([, w]) => w);
    // A per-model limit nobody has touched on any account says nothing.
    if (found.every(([, w]) => w.quiet)) continue;
    let used = 0;
    let expected = 0;
    let weight = 0;
    let paced = 0;
    let reset = false;
    let nextReset = null;
    for (const [a, w] of found) {
      const share = planWeight(a.plan);
      weight += share;
      const fresh = Boolean(w.resetsAt && w.resetsAt <= now);
      reset ||= fresh;
      // Past 100% an account holds no more than all of its own limit.
      used += share * (fresh ? 0 : Math.min(100, w.percent));
      // A limit that just reset, or has not started, is at the start of an even pace.
      const pace = fresh || w.idle ? { expected: 0 } : paceOf(w, now);
      if (pace) {
        paced++;
        expected += share * pace.expected;
      }
      if (w.resetsAt && w.resetsAt > now && (nextReset === null || w.resetsAt < nextReset)) nextReset = w.resetsAt;
    }
    used /= weight;
    let pace = null;
    if (paced === found.length) {
      expected /= weight;
      const diff = used - expected;
      pace = { expected, diff, standing: Math.abs(diff) <= 3 ? "even" : diff > 0 ? "over" : "under" };
    }
    const n = found.length;
    const idle = !nextReset && !reset && found.every(([, w]) => w.idle);
    meterLine(grid, moves, {
      // `*` can't be in an account's name, so this never meets a row's id.
      id: `${tool.id}|*pool|${label}`,
      label: windowName(label),
      used,
      pace,
      resets: nextReset ? t("poolResets", { d: duration(nextReset - now) }) : reset ? t("justReset") : idle ? t("notStarted") : "",
      resetsHelp: idle ? t("notStartedHelp") : undefined,
      help: n < count ? t("poolHelpSome", { n, total: count }) : t("poolHelp", { n }),
    });
  }
  // Which accounts are in, which are left out, and what each plan weighs,
  // so the figure can be checked against the rows below.
  const plans = [...new Set(accounts.map((a) => a.plan || ""))];
  const weights = plans.map((plan) => `${plan || t("poolPlanUnknown")} ×${planWeight(plan)}`).join(" · ");
  const names = accounts.map((a) => a.name).join(" · ");
  const left = tool.accounts.filter((a) => !accounts.includes(a)).map((a) => a.name);
  const total = tool.accounts.length;
  const countText = left.length ? t("poolCountOf", { n: count, total }) : t("poolCount", { n: count });
  return h(
    "div",
    { class: "row pool", role: "group", "aria-label": `${t("poolTitle")} · ${countText}` },
    h("div", { class: "avatar", "aria-hidden": "true" }, icon("layers")),
    h(
      "div",
      { class: "identity" },
      h("div", { class: "row-title" }, h("span", { class: "row-name", text: t("poolTitle") }), h("span", { class: "tag", text: countText })),
      h(
        "div",
        { class: "pool-notes" },
        h("div", { class: "pool-note", text: t("poolWho", { names }) }),
        left.length ? h("div", { class: "pool-note left-out", text: t("poolLeftOut", { names: left.join(" · ") }) }) : null,
        h("div", { class: "pool-note", text: t("poolNote", { plans: weights }), title: t("poolNoteHelp") }),
      ),
    ),
    grid,
  );
}

/** One line of a meters grid: `used` as a bar with the even-pace tick,
 * the figure, then when it resets and how far off pace it is. */
function meterLine(grid, moves, { id, label, used, pace, resets, resetsHelp, help }) {
  const percent = Math.max(0, Math.min(100, used));
  const level = used >= 100 ? " out" : used >= 75 ? " low" : "";
  const fill = h("div", { class: "bar-fill" + level + (percent > 0 ? "" : " none") });
  fill.style.width = `${percent}%`;
  const bar = h("div", { class: "bar" }, fill);
  const off = pace && pace.standing !== "even";
  if (off && pace.standing === "over" && percent < 100) {
    // The part used ahead of pace, as a share of the fill so it scales with it.
    const ahead = h("div", { class: "pace-over" });
    ahead.style.width = `${(100 * (percent - pace.expected)) / percent}%`;
    fill.append(ahead);
  }
  if (off) {
    const mark = h("div", { class: "pace-mark" + (pace.standing === "over" ? " over" : "") });
    mark.style.left = `${pace.expected}%`;
    bar.append(mark);
    bar.title = t("paceHelp", { n: Math.round(pace.expected) });
  }
  if (help) bar.title = bar.title ? `${help} · ${bar.title}` : help;
  const paceWords = off ? t(pace.standing === "over" ? "overPace" : "underPace", { n: Math.round(Math.abs(pace.diff)) }) : "";
  grid.append(
    h("span", { class: "meter-label", text: label }),
    bar,
    h("span", { class: "meter-figure" }, String(Math.round(used)), h("span", { class: "unit", text: "%" })),
    h(
      "span",
      { class: "meter-meta" },
      resets ? h("span", { class: "meter-resets", text: resets, title: resetsHelp }) : null,
      paceWords ? h("span", { class: "meter-pace" + (pace.standing === "over" ? " over" : ""), text: paceWords }) : null,
    ),
  );

  const before = shownPercents.get(id);
  shownPercents.set(id, percent);
  if (before !== percent && !reducedMotion.matches) {
    moves.push(() => {
      // A limit that just reset drains to empty before it hides.
      const draining = percent <= 0 && before > 0;
      if (draining) fill.classList.remove("none");
      const move = fill.animate([{ width: `${before === undefined ? 0 : before}%` }, { width: `${percent}%` }], {
        duration: before === undefined ? 700 : 450,
        easing: "cubic-bezier(0.165, 0.84, 0.44, 1)",
      });
      if (draining) move.finished.then(() => fill.classList.add("none"), () => {});
    });
  }
}

/** The warm-up row Settings shows for a Claude Code account, when it is on. */
function warmOf(tool, account) {
  const a = snap && snap.automation;
  if (tool.id !== "claude-code" || !a) return null;
  const row = a.warmUp.find((r) => r.name === account.name);
  return row && row.on ? row : null;
}

const WARM_TONES = {
  running: "ok",
  waiting: "warn",
  gaveUp: "warn",
  loginExpired: "warn",
  loginRefused: "warn",
  limitUsedUp: "warn",
};

/** One line of chips under a row: what stops its usage being read, its
 * warm-up, and how long it has been parked. */
function notesOf(tool, account) {
  const chips = [];
  const usage = account.usage;
  const warm = warmOf(tool, account);
  if (usage && usage.status === "expired" && !account.inUse) {
    // Only a warm-up that is still going to run renews the token.
    const stuck = warm && warm.standing && ["loginExpired", "loginRefused", "gaveUp"].includes(warm.standing.state);
    const text = warm && !stuck ? t("tokenExpiredWarm") : t("tokenExpired", { tool: tool.name });
    chips.push(h("span", { class: "chip" }, icon("clock"), h("span", { text })));
  }
  if (usage && usage.status === "unauthorized") chips.push(h("span", { class: "chip warn" }, icon("warning"), h("span", { text: t("unauthorized") })));
  if (usage && usage.status === "failed") {
    // The usage service turning reads away for a while is not a fault: say so plainly.
    const text = usage.rateLimited
      ? t("usageRateLimited")
      : usage.offline
        ? t("usageOffline")
        : t("usageFailed", { message: usage.message });
    chips.push(h("span", { class: "chip warn", title: usage.message }, icon("warning"), h("span", { text })));
  }
  if (usage && usage.status === "ok" && !shownWindows(usage).length) chips.push(h("span", { class: "chip" }, h("span", { text: t("noLimits") })));
  if (warm) {
    const tone = (warm.standing && WARM_TONES[warm.standing.state]) || "";
    chips.push(
      h(
        "span",
        { class: "chip" + (tone ? ` ${tone}` : ""), title: `${t("warmUp")}: ${warmText(warm)}` },
        icon("flame"),
        h("span", { class: "chip-name", text: t("warmUp") }),
        h("span", { text: warmText(warm, true) }),
      ),
    );
  }
  const parked = !account.inUse && account.parkedAt ? h("span", { class: "parked-at", text: t("parked", { ago: ago(account.parkedAt) }) }) : null;
  if (!chips.length && !parked) return null;
  return h("div", { class: "notes" }, chips, parked);
}

/** A callout: tinted badge, title, lines, actions. `key` names it across
 * renders so only a new one animates in. */
function noticeRow(notice) {
  const key = notice.key || `notice:${notice.serial}`;
  return h(
    "div",
    { class: `panel callout ${notice.kind || "info"}`, "data-callout": key },
    h("div", { class: "callout-badge", "aria-hidden": "true" }, icon(notice.icon || "info")),
    h(
      "div",
      { class: "callout-body" },
      h("div", { class: "callout-title", text: notice.title }),
      (notice.lines || []).map((line) => h("p", { class: "callout-line", text: line })),
      notice.actions && notice.actions.length
        ? h(
            "div",
            { class: "callout-actions" },
            notice.actions.map((action, at) => keyed(button(action.title, action.run, `${action.kind || ""} compact`.trim()), `${key}:${at}`)),
          )
        : null,
    ),
    notice.dismissible
      ? keyed(
          h(
            "button",
            {
              class: "dismiss",
              title: t("dismiss"),
              "aria-label": t("dismiss"),
              onclick: () => {
                notices = notices.filter((n) => n !== notice);
                renderPane();
              },
            },
            icon("x"),
          ),
          `${key}:dismiss`,
        )
      : h("span"),
  );
}

function pushNotice(notice) {
  notice.dismissible = true;
  notice.serial = ++noticeSerial;
  notices = [notice, ...notices.filter((n) => n.id !== notice.id)].slice(0, 3);
  announce(notice.title);
  renderPane();
}

function suggestionNotice(suggestion) {
  const tool = toolOf(suggestion.provider);
  const target = tool && tool.accounts.find((a) => a.name === suggestion.to);
  return noticeRow({
    key: `suggest:${suggestion.provider}:${suggestion.from}:${suggestion.to}:${suggestion.window}`,
    kind: "warning",
    icon: "swap",
    title: t("ranOutTitle", { name: suggestion.from, window: windowName(suggestion.window, true) }),
    lines: [t("ranOutDetail", { other: suggestion.to })],
    actions: [{ title: t("switchTo", { name: suggestion.to }), kind: "primary", run: () => useAccount(tool, target) }],
  });
}

function nameTip(tool) {
  const email = tool.live.label;
  return noticeRow({
    key: `tip-name:${tool.id}:${email || ""}`,
    kind: "tip",
    icon: "tag",
    title: t("tipNameTitle"),
    lines: [email ? t("tipNameDetail", { tool: tool.name, email }) : t("tipNameUnknown", { tool: tool.name })],
    actions: [{ title: t("nameButton"), run: () => openName(tool.id) }],
  });
}

// ---------- rows: selection, keys, menu ----------

function select(key) {
  selected = key;
  for (const row of document.querySelectorAll(".row[data-key]")) row.classList.toggle("selected", row.dataset.key === key);
}

function primaryAction(tool, account) {
  if (account.inUse) return;
  if (account.usage && account.usage.status === "unauthorized") openSignInAgain(tool, account);
  else useAccount(tool, account);
}

function onRowKey(event, tool, account) {
  if (event.target !== event.currentTarget) return;
  const rows = () => [...document.querySelectorAll(".row[data-key]")];
  const go = (row) => {
    if (!row) return;
    select(row.dataset.key);
    row.focus();
  };
  if (event.key === "Enter") {
    event.preventDefault();
    primaryAction(tool, account);
  } else if (event.key === "Delete" || event.key === "Backspace") {
    event.preventDefault();
    openForget(tool, account);
  } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    const all = rows();
    go(all[all.indexOf(event.currentTarget) + (event.key === "ArrowDown" ? 1 : -1)]);
  } else if (event.key === "Home" || event.key === "End") {
    event.preventDefault();
    const all = rows();
    go(event.key === "Home" ? all[0] : all[all.length - 1]);
  } else if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
    event.preventDefault();
    const rect = event.currentTarget.getBoundingClientRect();
    openRowMenu(rect.left + 56, rect.top + 30, tool, account);
  }
}

function openRowMenu(x, y, tool, account) {
  const items = [];
  if (!account.inUse) items.push({ title: t("useName", { name: account.name }), run: () => useAccount(tool, account) });
  items.push({ title: t("signInAgain"), run: () => openSignInAgain(tool, account), disabled: tool.installed === false });
  items.push({ title: t("rename"), run: () => openRename(tool, account) });
  if (account.label) items.push({ separator: true }, { title: t("copyEmail"), run: () => copy(account.label) });
  items.push({ separator: true }, { title: t("forget"), danger: true, run: () => openForget(tool, account) });
  showMenu(x, y, items, `key:${rowKey(tool.id, account.name)}`);
}

function showMenu(x, y, items, returnKey) {
  closeMenu();
  menuEl = h(
    "div",
    { class: "menu", role: "menu", onkeydown: onMenuKey },
    items.map((item) =>
      item.separator
        ? h("div", { class: "menu-sep", role: "separator" })
        : h(
            "button",
            {
              class: "menu-item" + (item.danger ? " danger" : ""),
              role: "menuitem",
              tabindex: "-1",
              disabled: item.disabled,
              onclick: () => {
                closeMenu();
                item.run();
              },
            },
            item.title,
          ),
    ),
  );
  document.body.append(menuEl);
  const rect = menuEl.getBoundingClientRect();
  const left = Math.max(8, Math.min(x, innerWidth - rect.width - 8));
  const top = Math.max(8, Math.min(y, innerHeight - rect.height - 8));
  menuEl.style.left = `${left}px`;
  menuEl.style.top = `${top}px`;
  // Grow out of the point that opened it, even when the menu had to flip.
  menuEl.style.setProperty("--origin-x", `${x - left}px`);
  menuEl.style.setProperty("--origin-y", `${y - top}px`);
  menuReturn = returnKey || null;
  const first = menuEl.querySelector(".menu-item:not(:disabled)");
  if (first) first.focus({ preventScroll: true });
}

function onMenuKey(event) {
  const items = [...menuEl.querySelectorAll(".menu-item:not(:disabled)")];
  const at = items.indexOf(document.activeElement);
  let next = null;
  if (event.key === "ArrowDown") next = items[(at + 1) % items.length];
  else if (event.key === "ArrowUp") next = items[(at - 1 + items.length) % items.length];
  else if (event.key === "Home") next = items[0];
  else if (event.key === "End") next = items[items.length - 1];
  else if (event.key === "Tab") {
    event.preventDefault();
    closeMenu();
    return;
  }
  if (next) {
    event.preventDefault();
    next.focus();
  }
}

function closeMenu() {
  if (!menuEl) return;
  const hadFocus = menuEl.contains(document.activeElement);
  menuEl.remove();
  menuEl = null;
  if (hadFocus) refocus(document.querySelector(".app"), menuReturn);
  menuReturn = null;
}

// ---------- changes ----------

/** Editors (Cursor, Antigravity) read a login only when they start. */
const EDITORS = ["cursor", "antigravity"];
const isEditor = (id) => EDITORS.includes(id);

async function useAccount(tool, account, force, restart) {
  if (!tool || !account) return;
  const key = rowKey(tool.id, account.name);
  if (switching.has(key)) return;
  // Every way to switch (button, Enter, menu, suggestion) waits out a warm-up.
  const warm = warmOf(tool, account);
  if (warm && warm.standing && warm.standing.state === "warming") {
    toast(t("useAfterWarm"), "clock");
    return;
  }
  switching.add(key);
  renderPane();
  try {
    // The app asks first when the tool is signed in to a login nobody owns.
    // And before closing an editor that is open, to switch it.
    const result = await invoke("use_account", { provider: tool.id, name: account.name, replaceUnmanaged: Boolean(force), restartEditor: Boolean(restart) });
    if (result.needsConfirmation) openConfirmSwitch(tool, account, result.replacedLabel);
    else if (result.needsRestart) openConfirmRestart(tool, account, Boolean(force));
    else if (result.switched) noticeSwitched(result);
  } catch (error) {
    openAlert(t("couldntSwitch", { name: account.name }), errorText(error));
  } finally {
    switching.delete(key);
    renderPane();
  }
}

function noticeSwitched(result) {
  const tool = toolOf(result.provider);
  const toolName = tool ? tool.name : result.provider;
  const lines = [
    isEditor(result.provider)
      ? t(result.restarted ? "editorReopened" : "editorNextOpen", { tool: toolName })
      : result.provider === "codex"
        ? t("codexRestart")
        : t("claudeFollows"),
  ];
  if (result.replaced) lines.push(t("replacedNote", { tool: toolName, email: result.replacedLabel || "?" }));
  pushNotice({ id: "switched", kind: "success", icon: "check", title: t("switchedTitle", { tool: toolName, name: result.name }), lines });
}

// ---------- dialogs ----------

let overlayEl = null; // the backdrop, kept while a dialog is up
let sheetEl = null; // the dialog itself, kept while its model is the same
let sheetModel = null;
let dialogReturn = null; // focus key of what had focus before the dialog

// What asks for a dialog while a sign-in runs waits for it to end (an alert)
// or is dropped (the window already shows the sign-in): replacing it would
// leave the sign-in running out of sight.
let deferredAlert = null;

function openDialog(model) {
  if (dialog && dialog !== model && dialog.type === "signin" && dialog.phase === "running") {
    if (model.type === "alert") deferredAlert = model;
    return;
  }
  closeMenu();
  if (!dialog) dialogReturn = focusKeyOf(document.activeElement);
  dialog = model;
  renderDialog();
}

function closeDialog() {
  dialog = null;
  renderDialog();
  if (deferredAlert) {
    const alert = deferredAlert;
    deferredAlert = null;
    setTimeout(() => openDialog(alert), 200);
  }
}

/** Draw the dialog. The backdrop and sheet stay in the page while the same
 * dialog is open, so typing re-renders its contents without replaying the
 * entrance, and focus and caret stay where they were. */
function renderDialog() {
  const app = document.querySelector(".app");
  if (!dialog) {
    if (overlayEl) {
      const el = overlayEl;
      overlayEl = sheetEl = sheetModel = null;
      app.inert = false;
      // Focus leaves the fading sheet now, or it would drop to the page when
      // the sheet is removed, and the way back below would be skipped.
      if (el.contains(document.activeElement)) document.activeElement.blur();
      el.inert = true;
      leave(el, 160);
      const back = dialogReturn;
      dialogReturn = null;
      refocus(app, back);
    }
    return;
  }

  const active = document.activeElement;
  const inside = sheetEl && sheetEl.contains(active);
  const focusedField = inside && active.dataset ? active.dataset.field : null;
  const focusKey = inside ? focusKeyOf(active) : null;
  const caret = focusedField && typeof active.selectionStart === "number" ? [active.selectionStart, active.selectionEnd] : null;
  const before = sheetModel === dialog ? segmentedIndex(sheetEl) : null;
  const fresh = sheetModel !== dialog;

  // A new sheet animates in by itself (CSS animations start when an element
  // enters the page), whether the backdrop is new or already up.
  if (!overlayEl) {
    // Clicking the backdrop does nothing: a stray click must not cancel a
    // sign-in or drop what was typed.
    overlayEl = h("div", { class: "overlay enter" });
    document.getElementById("overlay-root").append(overlayEl);
    app.inert = true;
  }
  if (sheetModel !== dialog) {
    sheetEl = h("div", { class: "sheet", role: "dialog", "aria-modal": "true", "aria-labelledby": "dialog-title", tabindex: "-1" });
    overlayEl.replaceChildren(sheetEl);
    sheetModel = dialog;
  }
  sheetEl.replaceChildren(...[DIALOGS[dialog.type]()].flat(Infinity).filter(Boolean));

  const again = (focusedField && sheetEl.querySelector(`[data-field="${focusedField}"]`)) || findByFocusKey(sheetEl, focusKey);
  const target =
    again && !again.disabled
      ? again
      : sheetEl.querySelector("[data-autofocus]:not(:disabled)") ||
        sheetEl.querySelector("input:not(:disabled)") ||
        sheetEl.querySelector(".button.primary:not(:disabled), .button.danger:not(:disabled)") ||
        sheetEl;
  target.focus({ preventScroll: true });
  if (again && caret && again.dataset.field === focusedField) again.setSelectionRange(caret[0], caret[1]);
  // A suggested name is taken with Return or replaced by typing.
  else if (fresh && target.dataset && target.dataset.field === "name") target.select();
  slideSegmented(sheetEl, before);
}

function escapeDialog() {
  if (!dialog) return;
  if (dialog.type === "signin" && dialog.phase === "running") {
    cancelSignIn();
    return;
  }
  if (dialog.busy) return;
  closeDialog();
}

/** Keep Tab inside the dialog. */
function trapFocus(event) {
  if (!sheetEl) return;
  const items = [...sheetEl.querySelectorAll("button:not(:disabled), input:not(:disabled), select:not(:disabled)")];
  if (!items.length) {
    event.preventDefault();
    return;
  }
  const first = items[0];
  const last = items[items.length - 1];
  const active = document.activeElement;
  if (!sheetEl.contains(active) || active === sheetEl) {
    event.preventDefault();
    (event.shiftKey ? last : first).focus();
  } else if (event.shiftKey && active === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && active === last) {
    event.preventDefault();
    first.focus();
  }
}

function failureBox(title, error) {
  if (!error) return null;
  return h("div", { class: "failure", role: "alert" }, icon("error"), h("div", null, h("strong", { text: title }), errorText(error)));
}

let fieldSerial = 0;

/** A label over its control. */
function field(label, control) {
  if (control.matches("input, select")) {
    if (!control.id) control.id = `field-${control.dataset.field || ++fieldSerial}`;
    return h("div", { class: "field" }, h("label", { for: control.id, text: label }), control);
  }
  return h("div", { class: "field" }, h("div", { class: "field-label", id: control.dataset.labelId || null, text: label }), control);
}

function nameInput(model, placeholder) {
  return h("input", {
    type: "text",
    "data-field": "name",
    value: model.name,
    placeholder,
    spellcheck: "false",
    autocomplete: "off",
    maxlength: "64",
    disabled: model.busy,
    oninput: (event) => {
      model.name = event.target.value;
      model.error = null;
      renderDialog();
    },
    onkeydown: (event) => {
      if (event.key === "Enter" && !event.isComposing) {
        event.preventDefault();
        if (model.submit && NAME_RULE.test(model.name.trim())) model.submit();
      }
    },
  });
}

/** The naming rule under a name field, in red once what is typed breaks it. */
function nameHint(model) {
  const typed = model.name.trim();
  const broken = typed !== "" && !NAME_RULE.test(typed);
  return h("p", { class: "hint" + (broken ? " invalid" : ""), text: broken ? t("nameInvalid") : t("nameRule") });
}

function buttons(...children) {
  return h("div", { class: "sheet-buttons" }, children);
}

/** Title and message, with a tinted badge for dialogs that warn. */
function sheetHead(title, message, badge) {
  const words = [h("h2", { id: "dialog-title", text: title }), message ? h("p", { class: "message", text: message }) : null];
  if (!badge) return words;
  return h(
    "div",
    { class: "sheet-head" },
    h("div", { class: `sheet-badge ${badge.tone}`, "aria-hidden": "true" }, icon(badge.icon)),
    h("div", { class: "sheet-words" }, words),
  );
}

/** A pill track with a thumb that slides to the chosen option. `choose`
 * gets the option's id and whether the keyboard picked it.
 * `variant` "tiles" stacks each choice's mark over its name, for choices
 * too many to fit side by side as words. */
function segmented(options, chosen, labelId, choose, variant) {
  const index = Math.max(0, options.findIndex((o) => o.id === chosen));
  const group = h(
    "div",
    {
      class: "segmented" + (variant ? ` ${variant}` : ""),
      role: "radiogroup",
      "aria-labelledby": labelId,
      "data-index": String(index),
      "data-label-id": labelId,
      onkeydown: (event) => {
        if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) return;
        event.preventDefault();
        const step = event.key === "ArrowLeft" || event.key === "ArrowUp" ? -1 : 1;
        for (let at = index + step; at >= 0 && at < options.length; at += step) {
          if (!options[at].disabled) {
            choose(options[at].id, true);
            break;
          }
        }
      },
    },
    h("span", { class: "seg-thumb", "aria-hidden": "true" }),
    options.map((o) =>
      keyed(
        h(
          "button",
          {
            role: "radio",
            "aria-checked": o.id === chosen ? "true" : "false",
            tabindex: o.id === chosen ? "0" : "-1",
            disabled: o.disabled,
            onclick: () => choose(o.id, false),
          },
          o.label,
        ),
        `seg-${o.id}`,
      ),
    ),
  );
  group.style.setProperty("--count", String(Math.max(1, options.length)));
  group.style.setProperty("--index", String(index));
  return group;
}

function segmentedIndex(root) {
  const group = root && root.querySelector(".segmented");
  return group ? Number(group.dataset.index) : null;
}

/** After a re-render, glide the thumb from where it was. */
function slideSegmented(root, before) {
  const group = root.querySelector(".segmented");
  if (!group || before === null || reducedMotion.matches) return;
  const now = Number(group.dataset.index);
  if (now === before) return;
  group
    .querySelector(".seg-thumb")
    .animate([{ transform: `translateX(${(before - now) * 100}%)` }, { transform: "translateX(0)" }], {
      duration: 120,
      easing: "cubic-bezier(0.32, 0.72, 0, 1)",
    });
}

const DIALOGS = {
  signin() {
    const d = dialog;
    const tool = toolOf(d.provider);
    const toolName = tool ? tool.name : d.provider;
    if (d.phase === "running") {
      const code = d.wantsCode
        ? [
            field(
              t("code"),
              h("input", {
                type: "text",
                "data-field": "code",
                value: d.code || "",
                placeholder: t("codePrompt"),
                spellcheck: "false",
                autocomplete: "off",
                oninput: (event) => {
                  d.code = event.target.value;
                  renderDialog();
                },
                onkeydown: (event) => {
                  if (event.key === "Enter" && !event.isComposing) {
                    event.preventDefault();
                    sendCode();
                  }
                },
              }),
            ),
            d.refused ? h("p", { class: "hint invalid", text: t("codeRefused", { tool: toolName }) }) : null,
          ]
        : null;
      const how = { codex: "signingInCodex", cursor: "signingInCursor", antigravity: "signingInAntigravity" }[d.provider] || "signingInClaude";
      const message = d.again && !isEditor(d.provider) ? t("signingInAgain", { email: d.email || d.name }) : t(how, { email: d.email || d.name });
      return [
        sheetHead(t("signingInTitle", { tool: toolName }), message),
        h(
          "div",
          { class: "form" },
          // Antigravity signs in in its own window: there is no page to open.
          d.provider === "antigravity"
            ? null
            : field(
            t("signInPage"),
            h(
              "div",
              { class: "link-row" },
              copyButton(d.url, t("copyLink"), "copy-link"),
              keyed(
                h("button", { class: "button compact", disabled: !d.url, onclick: () => invoke("open_sign_in_page").catch(() => {}) }, icon("external"), t("open")),
                "open-page",
              ),
            ),
          ),
          code,
        ),
        buttons(
          h("div", { class: "waiting", role: "status" }, h("div", { class: "spinner" }), t("waiting")),
          h("span", { class: "spacer" }),
          keyed(button(t("cancel"), cancelSignIn), "cancel"),
          d.wantsCode ? keyed(h("button", { class: "button primary", disabled: !(d.code || "").trim(), onclick: sendCode }, t("submitCode")), "primary") : null,
        ),
      ];
    }

    const offered = snap ? snap.tools : [];
    const missing = offered.filter((x) => x.installed === false && !x.accounts.length);
    const chosenMissing = missing.some((x) => x.id === d.provider);
    const nameOk = d.again || NAME_RULE.test(d.name.trim());
    const nameBroken = d.name.trim() !== "" && !nameOk;
    // With one tool to add to there is nothing to choose: the title names it.
    const choice = offered.filter((x) => !missing.includes(x)).length > 1;
    d.submit = () => !chosenMissing && startSignIn();
    // A click moves on to the name, as typing it is what comes next; arrow
    // keys stay on the choice.
    const choose = (id, byKey) => {
      if (d.provider !== id) {
        d.provider = id;
        d.error = null;
        // A name still as suggested follows the tool; a typed one stays.
        if (d.name === d.suggested) d.name = d.suggested = nextName(toolOf(id));
        renderDialog();
      }
      const next = byKey ? sheetEl.querySelector(`[data-focus="seg-${CSS.escape(id)}"]`) : sheetEl.querySelector('[data-field="name"]');
      if (next) next.focus({ preventScroll: true });
      if (next && !byKey && d.name === d.suggested) next.select();
    };
    return [
      sheetHead(
        d.again ? t("signInAgainTitle", { name: d.name }) : choice ? t("addTitle") : t("addTitleFor", { tool: toolName }),
        d.again
          ? t(d.provider === "antigravity" ? "signInAgainAntigravity" : "signInAgainMessage", { email: d.email || d.name })
          : t({ cursor: "addMessageCursor", antigravity: "addMessageAntigravity" }[d.provider] || "addMessage"),
      ),
      h(
        "div",
        { class: "form" },
        d.again
          ? null
          : [
              choice
                ? field(
                    t("tool"),
                    segmented(
                      offered.map((x) => ({ id: x.id, label: [toolLogo(x.id), h("span", { class: "seg-text", text: x.name })], disabled: missing.includes(x) })),
                      d.provider,
                      "tool-label",
                      choose,
                      "tiles",
                    ),
                  )
                : null,
              field(t("name"), nameInput(d, t("namePrompt"))),
              nameBroken ? nameHint(d) : null,
            ],
        missing.map((x) =>
          h("p", { class: "footnote", text: isEditor(x.id) ? t("appMissing", { tool: x.name }) : t("toolMissing", { tool: x.name, program: x.id === "codex" ? "codex" : "claude" }) }),
        ),
        d.again ? null : h("p", { class: "footnote", text: isEditor(d.provider) ? t("addWarningEditor", { tool: toolName }) : t("addWarning") }),
      ),
      failureBox(d.again ? t("couldntSignIn", { name: d.name }) : t("couldntAdd", { name: d.name.trim() }), d.error),
      buttons(
        h("span", { class: "spacer" }),
        keyed(button(t("cancel"), closeDialog), "cancel"),
        keyed(h("button", { class: "button primary", disabled: !nameOk || chosenMissing, onclick: startSignIn }, t("signIn")), "primary"),
      ),
    ];
  },

  name() {
    const d = dialog;
    const tool = toolOf(d.provider);
    d.submit = async () => {
      d.busy = true;
      renderDialog();
      try {
        await invoke("name_account", { provider: d.provider, name: d.name.trim() });
        closeDialog();
      } catch (error) {
        d.busy = false;
        d.error = error;
        renderDialog();
      }
    };
    return [
      sheetHead(t("nameTitle"), t("nameMessage", { tool: tool ? tool.name : d.provider, email: d.email || "?" })),
      h("div", { class: "form" }, field(t("name"), nameInput(d, t("namePrompt"))), nameHint(d)),
      failureBox(t("couldntName"), d.error),
      buttons(
        h("span", { class: "spacer" }),
        keyed(h("button", { class: "button", disabled: d.busy, onclick: closeDialog }, t("cancel")), "cancel"),
        keyed(h("button", { class: "button primary", disabled: d.busy || !NAME_RULE.test(d.name.trim()), onclick: d.submit }, t("save")), "primary"),
      ),
    ];
  },

  rename() {
    const d = dialog;
    d.submit = async () => {
      d.busy = true;
      renderDialog();
      try {
        await invoke("rename_account", { provider: d.provider, name: d.oldName, newName: d.name.trim() });
        if (selected === rowKey(d.provider, d.oldName)) selected = rowKey(d.provider, d.name.trim());
        if (dialogReturn === `key:${rowKey(d.provider, d.oldName)}`) dialogReturn = `key:${rowKey(d.provider, d.name.trim())}`;
        closeDialog();
      } catch (error) {
        d.busy = false;
        d.error = error;
        renderDialog();
      }
    };
    return [
      sheetHead(t("renameTitle", { name: d.oldName }), t("renameMessage")),
      h("div", { class: "form" }, field(t("name"), nameInput(d, d.oldName)), nameHint(d)),
      failureBox(t("couldntRename", { name: d.oldName }), d.error),
      buttons(
        h("span", { class: "spacer" }),
        keyed(h("button", { class: "button", disabled: d.busy, onclick: closeDialog }, t("cancel")), "cancel"),
        keyed(
          h(
            "button",
            { class: "button primary", disabled: d.busy || !NAME_RULE.test(d.name.trim()) || d.name.trim() === d.oldName, onclick: d.submit },
            t("renameButton"),
          ),
          "primary",
        ),
      ),
    ];
  },

  forget() {
    const d = dialog;
    const tool = toolOf(d.provider);
    const run = async () => {
      d.busy = true;
      renderDialog();
      try {
        await invoke("forget_account", { provider: d.provider, name: d.name });
        closeDialog();
      } catch (error) {
        d.busy = false;
        d.error = error;
        renderDialog();
      }
    };
    return [
      sheetHead(
        t("forgetTitle", { name: d.name }),
        d.inUse ? t("forgetActiveMessage", { tool: tool ? tool.name : d.provider }) : t("forgetMessage"),
        { tone: "danger", icon: "trash" },
      ),
      failureBox(t("couldntForget", { name: d.name }), d.error),
      buttons(
        h("span", { class: "spacer" }),
        keyed(h("button", { class: "button", disabled: d.busy, onclick: closeDialog, "data-autofocus": true }, t("cancel")), "cancel"),
        keyed(h("button", { class: "button danger", disabled: d.busy, onclick: run }, t("forgetButton")), "primary"),
      ),
    ];
  },

  confirm() {
    const d = dialog;
    const tool = toolOf(d.provider);
    const account = tool && tool.accounts.find((a) => a.name === d.name);
    return [
      sheetHead(
        t("confirmSwitchTitle", { email: d.email || "?" }),
        t("confirmSwitchMessage", { tool: tool ? tool.name : d.provider, email: d.email || "?", name: d.name }),
        { tone: "warning", icon: "swap" },
      ),
      buttons(
        h("span", { class: "spacer" }),
        keyed(button(t("cancel"), closeDialog), "cancel"),
        keyed(
          button(t("switchAnyway"), () => {
            closeDialog();
            useAccount(tool, account, true);
          }),
          "switch-anyway",
        ),
        keyed(button(t("nameItFirst"), () => openName(d.provider), "primary"), "primary"),
      ),
    ];
  },

  restart() {
    const d = dialog;
    const tool = toolOf(d.provider);
    const account = tool && tool.accounts.find((a) => a.name === d.name);
    const toolName = tool ? tool.name : d.provider;
    return [
      sheetHead(t("confirmRestartTitle", { tool: toolName }), t("confirmRestartMessage", { tool: toolName, name: d.name }), { tone: "warning", icon: "swap" }),
      buttons(
        h("span", { class: "spacer" }),
        keyed(button(t("cancel"), closeDialog), "cancel"),
        keyed(
          button(
            t("closeAndSwitch", { tool: toolName }),
            () => {
              closeDialog();
              useAccount(tool, account, d.force, true);
            },
            "primary",
          ),
          "primary",
        ),
      ),
    ];
  },

  alert() {
    const d = dialog;
    return [sheetHead(d.title, d.message, { tone: "error", icon: "error" }), buttons(h("span", { class: "spacer" }), keyed(button(t("ok"), closeDialog, "primary"), "primary"))];
  },
};

function openAdd(provider) {
  const tools = snap ? snap.tools : [];
  const usable = tools.filter((x) => x.installed !== false || x.accounts.length);
  const chosen = provider || (usable[0] && usable[0].id) || "claude-code";
  const name = nextName(toolOf(chosen));
  openDialog({ type: "signin", again: false, provider: chosen, name, suggested: name, phase: "form", error: null });
}

/** The name an added account most likely takes: the next of a numbered
 * series (ben1, ben2 → ben3), else none. */
function nextName(tool) {
  const names = tool ? tool.accounts.map((a) => a.name) : [];
  const taken = new Set(names.map((n) => n.toLowerCase()));
  // The series of the last account in the list, numbered on from its highest.
  const last = [...names].reverse().map((name) => /^(.*?)(\d+)$/.exec(name)).find(Boolean);
  if (!last) return "";
  const prefix = last[1];
  let n = Math.max(...names.map((name) => /^(.*?)(\d+)$/.exec(name)).filter((m) => m && m[1] === prefix).map((m) => Number(m[2]))) + 1;
  while (taken.has(`${prefix}${n}`.toLowerCase())) n++;
  const name = `${prefix}${n}`;
  return NAME_RULE.test(name) ? name : "";
}

function openSignInAgain(tool, account) {
  openDialog({ type: "signin", again: true, provider: tool.id, name: account.name, email: account.label, phase: "form", error: null });
}

function openName(provider) {
  const tool = toolOf(provider);
  const email = tool && tool.live.label;
  openDialog({ type: "name", provider, email, name: suggestedName(email, tool), error: null, busy: false });
}

function openRename(tool, account) {
  openDialog({ type: "rename", provider: tool.id, oldName: account.name, name: account.name, error: null, busy: false });
}

function openForget(tool, account) {
  openDialog({ type: "forget", provider: tool.id, name: account.name, inUse: account.inUse, error: null, busy: false });
}

function openConfirmRestart(tool, account, force) {
  openDialog({ type: "restart", provider: tool.id, name: account.name, force });
}

function openConfirmSwitch(tool, account, label) {
  openDialog({ type: "confirm", provider: tool.id, name: account.name, email: label || tool.live.label });
}

function openAlert(title, message) {
  openDialog({ type: "alert", title, message });
}

// ---------- signing in ----------

async function startSignIn() {
  const d = dialog;
  if (!d || d.type !== "signin") return;
  d.name = d.name.trim();
  Object.assign(d, { phase: "running", error: null, url: null, wantsCode: false, refused: false, code: "" });
  renderDialog();
  try {
    await invoke("start_sign_in", { provider: d.provider, name: d.name, again: d.again });
  } catch (error) {
    if (dialog === d) {
      d.phase = "form";
      d.error = error;
      renderDialog();
    }
  }
}

async function sendCode() {
  const d = dialog;
  if (!d || !(d.code || "").trim()) return;
  try {
    await invoke("send_sign_in_code", { code: d.code.trim() });
    d.code = "";
    d.refused = false;
    renderDialog();
  } catch (error) {
    // Back to the form first, so the "cancelled" the cancel sends doesn't
    // close the dialog; and no sign-in is left running behind the form.
    d.phase = "form";
    d.error = error;
    renderDialog();
    await invoke("cancel_sign_in").catch(() => {});
  }
}

async function cancelSignIn() {
  await invoke("cancel_sign_in").catch(() => {});
  closeDialog();
}

function onSignIn(event) {
  const d = dialog;
  const ours = d && d.type === "signin" && d.phase === "running" && d.provider === event.provider && d.name === event.name;
  if (event.phase === "done") {
    if (ours) closeDialog();
    pushNotice({
      id: `added-${event.provider}-${event.name}`,
      kind: "success",
      icon: "check",
      title: event.again ? t("signedInAgainTitle", { name: event.name }) : t("addedTitle", { name: event.name }),
      lines: event.again ? [] : [t("addedDetail")],
    });
    return;
  }
  if (!ours) return;
  if (event.phase === "running") {
    Object.assign(d, { url: event.url, wantsCode: event.wantsCode, refused: event.refusedCode });
    renderDialog();
  } else if (event.phase === "failed") {
    Object.assign(d, { phase: "form", error: event.error });
    renderDialog();
  } else if (event.phase === "cancelled") {
    closeDialog();
  }
}

/** The tray menu asked for one of the window's dialogs. */
function onOpenDialog(request) {
  if (request.kind === "add") openAdd(request.provider || undefined);
  else if (request.kind === "name" && request.provider) openName(request.provider);
  else if (request.kind === "confirm-switch") {
    const tool = toolOf(request.provider);
    const account = tool && tool.accounts.find((a) => a.name === request.name);
    if (tool && account) openConfirmSwitch(tool, account, request.label);
  } else if (request.kind === "confirm-restart") {
    const tool = toolOf(request.provider);
    const account = tool && tool.accounts.find((a) => a.name === request.name);
    if (tool && account) openConfirmRestart(tool, account, false);
  }
}

// ---------- settings ----------

function renderSettings() {
  const choice = snap ? snap.languageChoice || "system" : "system";
  const language = h(
    "select",
    {
      "aria-label": t("language"),
      onchange: (event) => {
        const value = event.target.value;
        invoke("set_language", { language: value === "system" ? null : value }).catch((error) => openAlert(t("couldntChange"), errorText(error)));
      },
    },
    [
      ["system", t("languageSystem")],
      ["en", "English"],
      ["vi", "Tiếng Việt"],
    ].map(([value, label]) => h("option", { value, selected: value === choice }, label)),
  );
  const appearance = h(
    "select",
    {
      "aria-label": t("theme"),
      onchange: (event) => {
        const value = event.target.value;
        applyTheme(value);
        invoke("set_theme", { theme: value === "system" ? null : value }).catch((error) => openAlert(t("couldntChange"), errorText(error)));
      },
    },
    [
      ["system", t("themeSystem")],
      ["light", t("themeLight")],
      ["dark", t("themeDark")],
    ].map(([value, label]) => h("option", { value, selected: value === themeChoice }, label)),
  );
  const on = Boolean(snap && snap.autostart);
  const autostart = h("button", {
    class: "switch" + (on ? " on" : ""),
    role: "switch",
    "aria-checked": on ? "true" : "false",
    "aria-label": t("openAtLogin"),
    onclick: () => invoke("set_autostart", { on: !on }).catch((error) => openAlert(t("couldntChange"), errorText(error))),
  });
  return h(
    "div",
    { class: "settings" },
    h(
      "section",
      { class: "group" },
      h("h2", { class: "group-head", text: t("general") }),
      h(
        "div",
        { class: "panel" },
        h("div", { class: "setting-row" }, h("div", { class: "setting-name", text: t("language") }), h("span", { class: "select" }, keyed(language, "language"))),
        h("div", { class: "setting-row" }, h("div", { class: "setting-name", text: t("theme") }), h("span", { class: "select" }, keyed(appearance, "theme"))),
        h(
          "div",
          { class: "setting-row" },
          h("div", null, h("div", { class: "setting-name", text: t("openAtLogin") }), h("p", { class: "help", text: t("openAtLoginHelp") })),
          keyed(autostart, "autostart"),
        ),
      ),
    ),
    automationGroups(),
    updateGroup(),
    h(
      "section",
      { class: "group" },
      h("h2", { class: "group-head", text: t("about") }),
      h(
        "div",
        { class: "panel about" },
        h(
          "div",
          { class: "about-head" },
          h("span", { class: "about-mark", "aria-hidden": "true" }, icon("mark")),
          h("div", null, h("div", { class: "about-name", text: APP_NAME }), h("div", { class: "about-sub", text: t("version", { version: snap ? snap.version : "" }) })),
        ),
        h("p", { text: t("aboutPrivacy") }),
        h("p", { text: t("aboutAdding") }),
        h(
          "div",
          { class: "about-author" },
          h("p", { class: "about-made", text: t("madeBy", { author: AUTHOR }) }),
          h(
            "div",
            { class: "about-links" },
            LINKS.map(([name, glyph, label]) =>
              h("button", { class: "button compact", onclick: () => invoke("open_link", { name }).catch((error) => openAlert(t("couldntOpen"), errorText(error))) }, icon(glyph), label()),
            ),
          ),
        ),
      ),
    ),
  );
}

// ---------- settings: automation ----------

function changeSetting(args) {
  invoke("set_automation", args).catch((error) => openAlert(t("couldntChange"), errorText(error)));
}

/** A pill toggle, as the other settings use. */
function toggle(on, label, run, key) {
  return keyed(
    h("button", {
      class: "switch" + (on ? " on" : ""),
      role: "switch",
      "aria-checked": on ? "true" : "false",
      "aria-label": label,
      onclick: run,
    }),
    key,
  );
}

/** A time of day, `HH:MM`, saved when it changes. */
function clockInput(value, label, key, save) {
  return keyed(
    h("input", {
      type: "time",
      class: "clock",
      value,
      "aria-label": label,
      onchange: (event) => {
        if (/^\d\d:\d\d$/.test(event.target.value)) save(event.target.value);
      },
    }),
    key,
  );
}

/** Where an account's warm-up stands, in a few words. */
/** Where a warm-up stands; `brief` leaves out when it last worked. */
function warmText(row, brief) {
  if (!row.on) return t("warmOff");
  const s = row.standing;
  let text = "";
  if (!s) text = t("warmJudging");
  else if (s.state === "due") text = t("warmDue");
  else if (s.state === "warming") text = t("warmWarming");
  else if (s.state === "running") text = t("warmRunning", { time: clockTime(s.until) });
  else if (s.state === "outsideSpan") text = t("warmOutside");
  else if (s.state === "noReading") text = t("warmNoReading");
  else if (s.state === "noFiveHour") text = t("warmNoFiveHour");
  else if (s.state === "loginExpired") text = t("warmLoginExpired");
  else if (s.state === "renewing") text = t("warmRenewing");
  else if (s.state === "loginRefused") text = t("warmLoginRefused");
  else if (s.state === "limitUsedUp") text = t("warmLimitUsedUp", { time: clockTime(s.until) });
  else if (s.state === "gaveUp") text = t("warmGaveUp");
  else if (s.state === "waiting") {
    text = t("warmWaiting", { time: clockTime(s.until) });
    const why = s.reason ? t("warmWhy_" + s.reason) : "";
    // A reason this window has no words for is left out, not shown as a code.
    if (why && why !== "warmWhy_" + s.reason) text += ` (${why})`;
  }
  if (!brief && row.warmedAt && nowSecs() - row.warmedAt < 86400) text += " · " + t("warmLast", { time: clockTime(row.warmedAt) });
  return text;
}

function automationGroups() {
  const a = snap && snap.automation;
  if (!a) return [];
  const threshold = h(
    "select",
    {
      "aria-label": t("autoSwitchAt"),
      disabled: !a.autoSwitch,
      onchange: (event) => changeSetting({ autoSwitchAt: Number(event.target.value) }),
    },
    Array.from({ length: 11 }, (_, i) => 50 + i * 5).map((n) => h("option", { value: String(n), selected: n === a.autoSwitchAt }, `${n}%`)),
  );
  const automatic = h(
    "section",
    { class: "group" },
    h("h2", { class: "group-head", text: t("automation") }),
    h(
      "div",
      { class: "panel" },
      h(
        "div",
        { class: "setting-row" },
        h("div", null, h("div", { class: "setting-name", text: t("autoSwitch") }), h("p", { class: "help", text: t("autoSwitchHelp") })),
        toggle(a.autoSwitch, t("autoSwitch"), () => changeSetting({ autoSwitch: !a.autoSwitch }), "auto-switch"),
      ),
      h(
        "div",
        { class: "setting-row" + (a.autoSwitch ? "" : " muted") },
        h("div", { class: "setting-name", text: t("autoSwitchAt") }),
        h("span", { class: "select" }, keyed(threshold, "auto-switch-at")),
      ),
      h(
        "div",
        { class: "setting-row" },
        h("div", null, h("div", { class: "setting-name", text: t("notifyRunOut") }), h("p", { class: "help", text: t("notifyRunOutHelp") })),
        toggle(a.notifyRunOut, t("notifyRunOut"), () => changeSetting({ notifyRunOut: !a.notifyRunOut }), "notify-run-out"),
      ),
    ),
  );
  const rows = a.warmUp.length
    ? a.warmUp.map((row) =>
        h(
          "div",
          { class: "setting-row warm-row" },
          h(
            "div",
            { class: "warm-account" },
            h("div", { class: "row-title" }, h("span", { class: "setting-name", text: row.name }), row.plan ? h("span", { class: "tag", text: row.plan }) : null),
            h("p", { class: "help", text: warmText(row) }),
          ),
          toggle(row.on, t("warmFor", { name: row.name }), () => invoke("set_warm_up", { name: row.name, on: !row.on }).catch((error) => openAlert(t("couldntChange"), errorText(error))), `warm:${row.name}`),
        ),
      )
    : [h("div", { class: "setting-row" }, h("p", { class: "help", text: t("warmNoAccounts") }))];
  const warmUp = h(
    "section",
    { class: "group" },
    h("h2", { class: "group-head", text: t("warmUp") }),
    h(
      "div",
      { class: "panel" },
      h(
        "div",
        { class: "setting-row" },
        h("div", null, h("div", { class: "setting-name", text: t("warmSpan") }), h("p", { class: "help", text: t("warmUpHelp") })),
        h(
          "div",
          { class: "span-pick" },
          clockInput(a.warmFrom, t("warmFrom"), "warm-from", (v) => changeSetting({ warmFrom: v })),
          h("span", { class: "span-dash", text: "–" }),
          clockInput(a.warmTo, t("warmTo"), "warm-to", (v) => changeSetting({ warmTo: v })),
        ),
      ),
      rows,
    ),
  );
  return [automatic, warmUp];
}

function noticeAutoSwitched(event) {
  pushNotice({ id: "switched", kind: "success", icon: "swap", title: event.title, lines: [event.why] });
}

// ---------- start ----------

document.addEventListener("keydown", (event) => {
  const mod = event.ctrlKey || event.metaKey;
  const key = event.key.toLowerCase();
  if (event.key === "Escape") {
    if (menuEl) closeMenu();
    else if (dialog) escapeDialog();
    return;
  }
  if (event.key === "Tab" && dialog) {
    trapFocus(event);
    return;
  }
  if (event.key === "F5" || (mod && key === "r")) {
    event.preventDefault();
    if (dialog) return;
    if (pane === "usage") refreshUsage();
    else invoke("refresh", { usage: true });
  } else if (mod && key === "n") {
    event.preventDefault();
    if (!dialog) openAdd();
  } else if (mod && event.key === ",") {
    event.preventDefault();
    pane = "settings";
    render();
  }
});

document.addEventListener("mousedown", (event) => {
  if (menuEl && !menuEl.contains(event.target)) closeMenu();
});

document.addEventListener("contextmenu", (event) => {
  if (!event.target.closest("input, textarea")) event.preventDefault();
});

// Coming back to the window rereads usage when the last reading is over a
// minute old, and only the accounts otherwise. At most once a minute: while
// reads are turned away the reading shown stays old, and asking on every
// focus would only get more of them turned away.
let focusReadAt = 0;
window.addEventListener("focus", () => {
  const stale = !snap || !snap.updatedAt || nowSecs() - snap.updatedAt > 60;
  const usage = stale && nowSecs() - focusReadAt > 60;
  if (usage) focusReadAt = nowSecs();
  invoke("refresh", { usage });
});
window.addEventListener("blur", () => {
  closeMenu();
  hideEmails();
});
window.addEventListener("resize", updateFades);

async function start() {
  if (navigator.userAgent.includes("Mac")) document.body.classList.add("mac");
  document.getElementById("pane").addEventListener("scroll", updateFades, { passive: true });
  await listen("snapshot", (next) => {
    // Snapshots can arrive out of order; an older one never replaces a newer.
    if (snap && next.revision < snap.revision) return;
    snap = next;
    render();
    if (dialog && dialog.type === "signin" && dialog.phase === "form") renderDialog();
  });
  await listen("sign-in", onSignIn);
  await listen("open-dialog", onOpenDialog);
  await listen("switched", noticeSwitched);
  await listen("auto-switched", noticeAutoSwitched);
  await listen("switch-failed", (failure) => openAlert(t("couldntSwitch", { name: failure.name }), errorText(failure.error)));
  // A quick first read draws straight away; a slow one shows the skeleton
  // meanwhile instead of an empty window.
  const first = invoke("get_snapshot");
  const early = setTimeout(render, 150);
  const fetched = await first;
  if (!snap || fetched.revision >= snap.revision) snap = fetched;
  clearTimeout(early);
  render();
  // "resets in" and pace move with the clock.
  setInterval(() => {
    if (pressing) return;
    if (pane === "accounts" || (pane === "usage" && usage.tab === "limits")) renderPane();
  }, 30000);
}

start();

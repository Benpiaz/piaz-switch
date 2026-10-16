// Updates: Settings' Updates group and the dialog that installs a new
// version. The app looks for one, checks its signature and installs it; this
// page only shows where that stands and asks for it.
"use strict";

const update = {
  view: null, // what the app last said: current, available, installing…
  checking: false,
  error: null, // why the last Check Now failed
  progress: null, // { done, total } while downloading
};

ICONS.download =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M12 4v11M7 10.5l5 5 5-5"/><path d="M5 19.5h14"/></svg>';

/** Draw again whatever shows the update: Settings and the update dialog. */
function renderUpdate() {
  if (pane === "settings") renderPane();
  if (dialog && dialog.type === "update") renderDialog();
}

async function checkForUpdate() {
  if (update.checking) return;
  update.checking = true;
  update.error = null;
  renderUpdate();
  try {
    const view = await invoke("check_for_update");
    if (!update.view || view.serial > update.view.serial) update.view = view;
  } catch (error) {
    update.error = error;
  } finally {
    update.checking = false;
    renderUpdate();
  }
}

/** CHANGELOG.md's section as plain text: headings, bullets and emphasis
 * without their Markdown marks. */
function plainNotes(notes) {
  return notes
    .replace(/^#+\s*/gm, "")
    .replace(/^(\s*)[-*] /gm, "$1• ")
    .replace(/\*\*(.+?)\*\*/g, "$1")
    .replace(/`([^`]+)`/g, "$1");
}

function openUpdate() {
  if (!update.view || !update.view.available) return;
  openDialog({ type: "update", error: null });
}

/** The Updates group in Settings. */
function updateGroup() {
  const v = update.view;
  if (!v) return null;
  const found = v.available;
  let status;
  if (update.checking) status = t("updateChecking");
  else if (found) status = t("updateFound", { version: found.version });
  else if (v.checkedAt) status = t("updateNone");
  else status = t("updateNotChecked");
  const help = update.error
    ? t("updateCheckFailed")
    : v.checkedAt && !update.checking
      ? t("updateCheckedAt", { time: clockTime(v.checkedAt) })
      : null;
  const action = found
    ? keyed(button(v.canInstall ? t("updateButton") : t("updateDownload"), openUpdate, "primary compact"), "update")
    : keyed(h("button", { class: "button compact", disabled: update.checking, onclick: checkForUpdate }, t("updateCheckNow")), "update-check");
  return h(
    "section",
    { class: "group" },
    h("h2", { class: "group-head", text: t("updates") }),
    h(
      "div",
      { class: "panel" },
      h(
        "div",
        { class: "setting-row" },
        h("div", null, h("div", { class: "setting-name", text: t("updateAuto") }), h("p", { class: "help", text: t("updateAutoHelp") })),
        toggle(v.autoCheck, t("updateAuto"), () => invoke("set_check_updates", { on: !v.autoCheck }).catch((error) => openAlert(t("couldntChange"), errorText(error))), "update-auto"),
      ),
      h(
        "div",
        { class: "setting-row" },
        h(
          "div",
          { role: "status" },
          h("div", { class: "setting-name", text: status }),
          help ? h("p", { class: "help" + (update.error ? " invalid" : ""), text: help, title: update.error ? errorText(update.error) : null }) : null,
        ),
        action,
      ),
    ),
  );
}

DIALOGS.update = function () {
  const d = dialog;
  const v = update.view;
  const found = v && v.available;
  if (!found) {
    // Installed elsewhere, or withdrawn while the dialog was up.
    return [sheetHead(t("updateNone"), null), buttons(h("span", { class: "spacer" }), keyed(button(t("ok"), closeDialog, "primary"), "primary"))];
  }
  // Busy from the click on, before the app says it is installing.
  const busy = v.installing || d.starting;
  d.busy = busy; // Escape leaves it alone while installing
  const run = async () => {
    if (!v.canInstall) {
      invoke("open_releases").catch(() => {});
      closeDialog();
      return;
    }
    d.error = null;
    d.starting = true;
    update.progress = null;
    renderDialog();
    try {
      // Returns only on failure; on success the app restarts.
      await invoke("install_update");
    } catch (error) {
      d.error = error;
    }
    d.starting = false;
    renderDialog();
  };
  let progress = null;
  if (busy) {
    const p = update.progress;
    const percent = p && p.total ? Math.min(100, Math.floor((p.done / p.total) * 100)) : null;
    const text = p && percent !== null && percent >= 100 ? t("updateInstalling") : percent !== null ? t("updateDownloading", { percent }) : t("updateStarting");
    // Set through the style object: the page's CSP refuses style attributes.
    const fill = h("div", { class: "bar-fill" });
    fill.style.width = `${percent === null ? 4 : percent}%`;
    progress = h(
      "div",
      { class: "update-progress" },
      h("div", { class: "bar", role: "progressbar", "aria-label": text, "aria-valuemin": "0", "aria-valuemax": "100", "aria-valuenow": percent === null ? null : String(percent) }, fill),
      h("p", { class: "hint", text }),
    );
  }
  return [
    sheetHead(t("updateTitle", { version: found.version }), t(v.canInstall ? "updateMessage" : "updatePortable", { current: v.current }), { tone: "accent", icon: "download" }),
    found.notes ? h("div", { class: "update-notes", tabindex: "0", "aria-label": t("updateNotes") }, plainNotes(found.notes)) : null,
    progress,
    failureBox(t("updateFailed"), d.error),
    buttons(
      h("span", { class: "spacer" }),
      keyed(h("button", { class: "button", disabled: busy, onclick: closeDialog }, t("later")), "cancel"),
      keyed(h("button", { class: "button primary", disabled: busy, onclick: run, "data-autofocus": true }, v.canInstall ? t("updateInstall") : t("updateDownload")), "primary"),
    ),
  ];
};

async function startUpdates() {
  await listen("update", (view) => {
    // Views can arrive out of order; an older one never replaces a newer.
    if (update.view && view.serial < update.view.serial) return;
    // A look that worked since clears the last failed one.
    if (update.error && view.checkedAt && (!update.view || view.checkedAt !== update.view.checkedAt)) update.error = null;
    update.view = view;
    if (!view.installing) update.progress = null;
    renderUpdate();
  });
  await listen("update-progress", (progress) => {
    update.progress = progress;
    if (dialog && dialog.type === "update") renderDialog();
  });
  await listen("open-dialog", (request) => {
    if (request.kind === "update") openUpdate();
  });
  const first = await invoke("update_status");
  if (!update.view || first.serial > update.view.serial) update.view = first;
  renderUpdate();
}

startUpdates();

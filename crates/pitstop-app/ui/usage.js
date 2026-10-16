// The Usage pane: what Claude Code and Codex used and would have cost at
// API prices, read by the app from the tools' own session logs, and the
// limits every account has left. The app sends numbers, model names and
// tool names only; nothing said in a session reaches this page.
"use strict";

const usage = {
  tab: "cost", // cost | tokens | limits
  range: "30d", // 24h | 7d | 30d | 90d
  tool: "all", // all | claude-code | codex
  breakdown: "model", // model | day
  answer: null,
  loading: false,
  error: null,
  hover: null, // column index under the pointer
};

const USAGE_TOOLS = ["claude-code", "codex"];
const USAGE_RANGES = ["24h", "7d", "30d", "90d"];

const TOOL_ICONS = {
  // A plain eight-spoke spark and a plain hexagon: marks for the two
  // tools, not their makers' logos.
  "claude-code":
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round"><path d="M12 3.5v17M3.5 12h17M6 6l12 12M18 6 6 18"/></svg>',
  codex:
    '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linejoin="round"><path d="M12 3 19.8 7.5v9L12 21l-7.8-4.5v-9z"/><path d="M12 8.2 15.3 10.1v3.8L12 15.8l-3.3-1.9v-3.8z"/></svg>',
};

ICONS.usage =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 19.5h16"/><path d="M6.5 16v-4M11 16V7.5M15.5 16v-6M20 16V5"/></svg>';
ICONS.chevron = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m7 10 5 5 5-5"/></svg>';

function toolIcon(id) {
  const holder = document.createElement("span");
  holder.innerHTML = TOOL_ICONS[id] || TOOL_ICONS.codex;
  return holder.firstChild;
}

// ---------- reading ----------

/** Ask the app to read what the logs gained and report the chosen range.
 * The pane keeps showing the last report, dimmed, until the new one lands. */
async function loadUsage() {
  if (usage.loading) return;
  usage.loading = true;
  if (pane === "usage") {
    renderHead();
    document.getElementById("pane").classList.add("refetching");
  }
  const range = usage.range;
  try {
    const answer = await invoke("usage_report", { range });
    if (range === usage.range) {
      usage.answer = answer;
      usage.error = null;
    }
  } catch (error) {
    usage.error = error;
  } finally {
    usage.loading = false;
    document.getElementById("pane").classList.remove("refetching");
    if (pane === "usage") {
      renderHead();
      renderPane();
    }
    // The range changed while this one was being read.
    if (usage.answer && usage.answer.range !== usage.range) loadUsage();
  }
}

/** Refresh, as the toolbar button and Ctrl/⌘R do: the logs and the limits. */
function refreshUsage() {
  loadUsage();
  invoke("refresh", { usage: true });
}

function usageView() {
  const answer = usage.answer;
  return answer && answer.views.find((v) => v.id === usage.tool);
}

// ---------- words and numbers ----------

/** `$6,795.04` */
function money(dollars) {
  return "$" + dollars.toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

/** Three significant digits: `5.47B`, `508M`, `42.5K`, `950`. */
function compact(n) {
  for (const [unit, suffix] of [
    [1e9, "B"],
    [1e6, "M"],
    [1e3, "K"],
  ]) {
    if (n >= unit * 0.9995) {
      const v = n / unit;
      if (suffix !== "B" && v >= 999.5) continue;
      return (v >= 99.95 ? v.toFixed(0) : v >= 9.995 ? v.toFixed(1) : v.toFixed(2)) + suffix;
    }
  }
  return String(Math.round(n));
}

function percent(share) {
  if (share === null || share === undefined) return "—";
  if (share <= 0) return "0%";
  if (share < 0.001) return "<0.1%";
  return `${(share * 100).toFixed(1)}%`;
}

function sessionsText(n) {
  return n === 1 ? t("oneSession") : t("sessions", { n: n.toLocaleString("en-US") });
}

function dateLocale() {
  return LANG === "vi" ? "vi-VN" : "en-US";
}

function shortDate(at) {
  return new Date(at * 1000).toLocaleDateString(dateLocale(), { month: "short", day: "numeric" });
}

function hourLabel(at) {
  return new Date(at * 1000).toLocaleTimeString(dateLocale() === "vi-VN" ? "vi-VN" : "en-GB", { hour: "2-digit", minute: "2-digit" });
}

/** What a column is called in the table and the tooltip. */
function columnLabel(at, hourly) {
  if (hourly) return `${shortDate(at)}, ${hourLabel(at)}`;
  return new Date(at * 1000).toLocaleDateString(dateLocale(), { weekday: "short", month: "short", day: "numeric" });
}

function spanText(answer) {
  if (answer.bucket === "hour") return t("rangeSpan", { from: columnLabel(answer.starts[0], true), to: columnLabel(answer.end - 1, true) });
  return t("rangeSpan", { from: shortDate(answer.starts[0]), to: shortDate(answer.end - 1) });
}

const metric = () => (usage.tab === "tokens" ? "tokens" : "cost");

// ---------- the header ----------

function renderUsageHead() {
  const title = document.getElementById("title");
  const toolName = usage.tool === "all" ? t("allTools") : (toolOf(usage.tool) || { name: usage.tool }).name;
  const picker = keyed(
    h(
      "button",
      {
        class: "crumb",
        "aria-haspopup": "menu",
        title: t("filterTools"),
        onclick: (event) => {
          const rect = event.currentTarget.getBoundingClientRect();
          openToolMenu(rect.left, rect.bottom + 4);
        },
      },
      h("span", { text: toolName }),
      icon("chevron"),
    ),
    "usage-tools",
  );
  title.replaceChildren(h("span", { text: t("usage") }), h("span", { class: "crumb-sep", "aria-hidden": "true", text: "/" }), picker);

  const status = document.getElementById("status");
  const answer = usage.answer;
  const parts = [];
  if (usage.loading) parts.push(h("span", { class: "status-dot", "aria-hidden": "true" }));
  if (usage.tab === "limits") parts.push(h("span", { text: updatedText() }));
  else if (answer) parts.push(h("span", { text: `${spanText(answer)} · ${t("updated", { time: clockTime(answer.scannedAt) })}` }));
  else if (usage.loading) parts.push(h("span", { text: t("reading") }));
  status.replaceChildren(...parts);

  const tabs = segmented(
    [
      { id: "cost", label: t("tabCost") },
      { id: "tokens", label: t("tabTokens") },
      { id: "limits", label: t("tabLimits") },
    ],
    usage.tab,
    "usage-tab-label",
    (id) => {
      usage.tab = id;
      render();
    },
  );
  const ranges = segmented(
    USAGE_RANGES.map((id) => ({ id, label: t(`range${id}`) })),
    usage.range,
    "usage-range-label",
    (id) => {
      if (id === usage.range) return;
      usage.range = id;
      usage.hover = null;
      render();
      loadUsage();
    },
  );
  const refresh = h(
    "button",
    {
      class: "icon-button" + (usage.loading ? " spinning" : ""),
      title: t("refreshUsageHelp"),
      "aria-label": t("refresh"),
      disabled: usage.loading,
      onclick: refreshUsage,
    },
    icon("refresh"),
  );
  document
    .getElementById("toolbar")
    .replaceChildren(
      ...[
        h("span", { class: "sr-only", id: "usage-tab-label", text: t("usageShows") }),
        h("span", { class: "sr-only", id: "usage-range-label", text: t("usagePeriod") }),
        tabs,
        usage.tab === "limits" ? null : ranges,
        keyed(refresh, "usage-refresh"),
      ].filter(Boolean),
    );
}

function openToolMenu(x, y) {
  const choices = [["all", t("allTools")], ...USAGE_TOOLS.map((id) => [id, (toolOf(id) || { name: id }).name])];
  showMenu(
    x,
    y,
    choices.map(([id, label]) => ({
      title: (id === usage.tool ? "✓ " : "") + label,
      run: () => {
        usage.tool = id;
        usage.hover = null;
        render();
      },
    })),
    "focus:usage-tools",
  );
}

// ---------- the pane ----------

function renderUsage(moves) {
  if (usage.tab === "limits") return renderLimits(moves);
  const answer = usage.answer;
  if (!answer) {
    if (usage.error) {
      return empty({
        iconName: "error",
        tone: "error",
        title: t("readLogsFailed"),
        detail: errorText(usage.error),
        action: button(t("tryAgain"), loadUsage),
      });
    }
    if (!usage.loading) loadUsage();
    return empty({ iconName: "usage", title: t("readingLogs"), tone: "quiet" });
  }
  if (!answer.files) {
    return empty({ iconName: "usage", title: t("noLogs"), detail: t("noLogsDetail", { dirs: answer.searched.join(", ") }) });
  }
  const view = usageView();
  return h(
    "div",
    { class: "usage" },
    answer.errors.length
      ? h(
          "div",
          { class: "callout warning" },
          h("div", { class: "callout-badge", "aria-hidden": "true" }, icon("warning")),
          h("div", { class: "callout-body" }, h("div", { class: "callout-title", text: answer.errors.length === 1 ? t("logFailed") : t("logsFailed", { n: answer.errors.length }) }), h("p", { class: "callout-line", text: answer.errors[0] })),
        )
      : null,
    h("section", { class: "panel usage-top" }, summaryOf(view), chartOf(answer, view)),
    totalsOf(view),
    breakdownOf(answer, view),
  );
}

/** The headline figure, the sessions, and a line per tool (the legend). */
function summaryOf(view) {
  const cost = metric() === "cost";
  const sublines = h("div", { class: "hero-sub" }, h("span", { text: sessionsText(view.sessions) }));
  if (cost) {
    sublines.append(
      h("span", { class: "dot-sep", "aria-hidden": "true", text: "·" }),
      h("span", { class: "estimate", tabindex: "0", title: t("apiEstimateHelp"), "aria-label": `${t("apiEstimate")}. ${t("apiEstimateHelp")}` }, t("apiEstimate"), icon("info")),
    );
  }
  return h(
    "div",
    { class: "usage-summary" },
    h("div", { class: "hero" }, cost ? money(view.cost) : compact(view.totals.processed)),
    sublines,
    cost && view.unpricedTokens ? h("div", { class: "hero-sub", text: t("unpricedNote", { n: compact(view.unpricedTokens) }) }) : null,
    h(
      "div",
      { class: "tool-lines" },
      view.tools.map((tool) =>
        h(
          "div",
          { class: "tool-line" },
          h("span", { class: `series-dot ${tool.id}`, "aria-hidden": "true" }),
          h("span", { class: `tool-mark ${tool.id}`, "aria-hidden": "true" }, toolIcon(tool.id)),
          h("span", { class: "tool-name" }, h("span", { text: tool.name }), h("span", { class: "tool-sessions", text: sessionsText(tool.sessions) })),
          h("span", { class: "tool-value", text: cost ? money(tool.cost) : compact(tool.tokens) }),
          h(
            "span",
            { class: "tool-detail" },
            cost
              ? t("ofCostTokens", { p: percent(tool.costShare), n: compact(tool.tokens) })
              : t("ofTokensCost", { p: percent(tool.tokenShare), cost: money(tool.cost) }),
          ),
        ),
      ),
    ),
  );
}

function totalsOf(view) {
  const tiles = [
    [t("processed"), compact(view.totals.processed)],
    [t("cachedInput"), compact(view.totals.cacheRead)],
    [t("uncachedInput"), compact(view.totals.input)],
    [t("cacheWrites"), compact(view.totals.cacheWrite)],
    [t("output"), compact(view.totals.output)],
    [t("cacheSavings"), money(view.cacheSavings)],
  ];
  return h(
    "section",
    { class: "group" },
    h("h2", { class: "group-head", text: t("totals") }),
    h(
      "div",
      { class: "panel totals" },
      tiles.map(([label, value]) => h("div", { class: "tile" }, h("div", { class: "tile-label", text: label }), h("div", { class: "tile-value", text: value }))),
    ),
  );
}

function breakdownOf(answer, view) {
  const hourly = answer.bucket === "hour";
  const by = metric();
  const switcher = segmented(
    [
      { id: "model", label: t("byModel") },
      { id: "day", label: hourly ? t("byHour") : t("byDay") },
    ],
    usage.breakdown,
    "usage-breakdown-label",
    (id) => {
      usage.breakdown = id;
      renderPane();
    },
  );
  const shareOf = (row) => (by === "cost" ? row.costShare : row.tokenShare);
  let rows;
  if (usage.breakdown === "model") {
    const models = [...view.models];
    if (by === "tokens") models.sort((a, b) => b.tokens - a.tokens);
    rows = models.map((m) =>
      tableRow(
        h("span", { class: "cell-name" }, h("span", { class: `tool-mark ${m.tool}`, title: (toolOf(m.tool) || { name: m.tool }).name }, toolIcon(m.tool)), h("span", { class: "model-name", text: m.model })),
        m.cost === null ? h("span", { class: "unpriced", text: t("unpriced") }) : money(m.cost),
        by === "cost" && m.cost === null ? "—" : percent(shareOf(m)),
        compact(m.tokens),
      ),
    );
  } else {
    rows = view.days.map((d) => tableRow(columnLabel(d.start, hourly), money(d.cost), percent(shareOf(d)), compact(d.tokens)));
  }
  const firstColumn = usage.breakdown === "model" ? t("colModel") : hourly ? t("colHour") : t("colDay");
  return h(
    "section",
    { class: "group" },
    h(
      "div",
      { class: "group-head breakdown-head" },
      h("h2", { id: "usage-breakdown-label", text: t("breakdown") }),
      switcher,
    ),
    h(
      "div",
      { class: "panel breakdown", role: "table", "aria-labelledby": "usage-breakdown-label" },
      h(
        "div",
        { class: "table-row table-head", role: "row" },
        [firstColumn, t("colCost"), t("colShare"), t("colTokens")].map((label) => h("span", { role: "columnheader", text: label })),
      ),
      rows.length ? rows : h("p", { class: "table-empty", text: t("noUsage") }),
    ),
    h("p", { class: "prices-note", text: answer.pricesAt ? t("pricesFrom", { date: shortDate(answer.pricesAt) }) : t("pricesShipped") }),
  );
}

function tableRow(...cells) {
  return h(
    "div",
    { class: "table-row", role: "row" },
    cells.map((cell) => h("span", { role: "cell" }, cell)),
  );
}

// ---------- the chart ----------

/** "Nice" steps for an axis: 1, 2, 2.5 or 5 times a power of ten. */
function niceStep(raw) {
  if (!(raw > 0)) return 1;
  const power = 10 ** Math.floor(Math.log10(raw));
  for (const m of [1, 2, 2.5, 5, 10]) if (raw <= m * power) return m * power;
  return 10 * power;
}

/** A smooth line through the points that never overshoots them
 * (monotone cubic, Fritsch–Carlson), as an SVG path. */
function smoothPath(points) {
  const n = points.length;
  if (n < 2) return n ? `M${points[0][0]},${points[0][1]}` : "";
  const dx = [];
  const slope = [];
  for (let i = 0; i < n - 1; i++) {
    dx.push(points[i + 1][0] - points[i][0]);
    slope.push((points[i + 1][1] - points[i][1]) / dx[i]);
  }
  const tangent = [slope[0]];
  for (let i = 1; i < n - 1; i++) tangent.push(slope[i - 1] * slope[i] <= 0 ? 0 : (slope[i - 1] + slope[i]) / 2);
  tangent.push(slope[n - 2]);
  for (let i = 0; i < n - 1; i++) {
    if (slope[i] === 0) {
      tangent[i] = 0;
      tangent[i + 1] = 0;
      continue;
    }
    const a = tangent[i] / slope[i];
    const b = tangent[i + 1] / slope[i];
    const s = a * a + b * b;
    if (s > 9) {
      const k = 3 / Math.sqrt(s);
      tangent[i] = k * a * slope[i];
      tangent[i + 1] = k * b * slope[i];
    }
  }
  let d = `M${points[0][0]},${points[0][1]}`;
  for (let i = 0; i < n - 1; i++) {
    const third = dx[i] / 3;
    d += ` C${points[i][0] + third},${points[i][1] + tangent[i] * third} ${points[i + 1][0] - third},${points[i + 1][1] - tangent[i + 1] * third} ${points[i + 1][0]},${points[i + 1][1]}`;
  }
  return d;
}

function svg(tag, attrs, ...children) {
  const el = document.createElementNS("http://www.w3.org/2000/svg", tag);
  for (const [k, v] of Object.entries(attrs || {})) if (v !== null && v !== undefined) el.setAttribute(k, v);
  for (const child of children.flat()) if (child) el.append(child);
  return el;
}

function chartOf(answer, view) {
  const by = metric();
  const hourly = answer.bucket === "hour";
  const title = by === "cost" ? (hourly ? t("hourlyCost") : t("dailyCost")) : hourly ? t("hourlyTokens") : t("dailyTokens");
  const host = h("div", { class: "chart-plot" });
  const tip = h("div", { class: "chart-tip", role: "status" });
  const box = h("div", { class: "usage-chart" }, h("h3", { class: "chart-title", text: title }), host, tip);
  // Drawn once the plot has a width, and again whenever it changes.
  let drawnWidth = 0;
  const observer = new ResizeObserver(() => {
    if (!host.isConnected) {
      observer.disconnect();
      return;
    }
    const width = Math.round(host.clientWidth);
    if (width && width !== drawnWidth) {
      drawnWidth = width;
      drawChart(host, tip, answer, view, width);
    }
  });
  observer.observe(host);
  return box;
}

function drawChart(host, tip, answer, view, width) {
  const by = metric();
  const hourly = answer.bucket === "hour";
  const height = 196;
  const pad = { top: 8, right: 8, bottom: 24, left: by === "cost" ? 58 : 46 };
  const plotW = width - pad.left - pad.right;
  const plotH = height - pad.top - pad.bottom;
  const n = answer.starts.length;
  // Bigger totals are drawn first, so the smaller line stays on top.
  const series = [...view.series].sort((a, b) => sum(b[by]) - sum(a[by]));
  const max = Math.max(0, ...series.flatMap((s) => s[by]));
  const step = niceStep(max / 4);
  const top = max > 0 ? Math.ceil(max / step) * step : step;
  const x = (i) => pad.left + (n > 1 ? (i * plotW) / (n - 1) : plotW / 2);
  const y = (v) => pad.top + plotH - (v / top) * plotH;
  // As many decimals as the step needs: a 2.5 step shows 7.5, not 8.
  let decimals = 0;
  while (decimals < 4 && Math.abs(step * 10 ** decimals - Math.round(step * 10 ** decimals)) > 1e-9) decimals++;
  if (step < 1) decimals = Math.max(decimals, 2);
  const tickText = (v) => (by === "cost" ? "$" + v.toLocaleString("en-US", { maximumFractionDigits: decimals, minimumFractionDigits: decimals }) : compact(v));

  const grid = [];
  for (let v = 0; v <= top + step / 2; v += step) {
    grid.push(
      svg("line", { class: "grid", x1: pad.left, x2: width - pad.right, y1: y(v), y2: y(v) }),
      svg("text", { class: "tick", x: pad.left - 8, y: y(v) + 4, "text-anchor": "end" }, document.createTextNode(tickText(v))),
    );
  }

  // Labels on the first and last columns and on month starts (by day) or
  // every six hours, dropped where they would crowd a neighbour.
  const labels = [];
  const wanted = [0];
  for (let i = 1; i < n - 1; i++) {
    const at = new Date(answer.starts[i] * 1000);
    if (hourly ? at.getHours() % 6 === 0 : at.getDate() === 1) wanted.push(i);
  }
  wanted.push(n - 1);
  const placed = [];
  for (const i of wanted) {
    const last = placed[placed.length - 1];
    if (last === undefined || x(i) - x(last) >= 64) placed.push(i);
    else if (i === n - 1 && last !== 0) placed[placed.length - 1] = i;
  }
  for (const i of placed) {
    const at = answer.starts[i];
    const text = hourly ? hourLabel(at) : shortDate(at).toUpperCase();
    const anchor = i === 0 ? "start" : i === n - 1 ? "end" : "middle";
    labels.push(svg("text", { class: "tick", x: x(i), y: height - 6, "text-anchor": anchor }, document.createTextNode(text)));
  }

  const marks = series.map((s) => {
    const points = s[by].map((v, i) => [x(i), y(v)]);
    const line = smoothPath(points);
    const area = `${line} L${x(n - 1)},${y(0)} L${x(0)},${y(0)} Z`;
    return svg("g", { class: `series ${s.tool}` }, svg("path", { class: "area", d: area }), svg("path", { class: "line", d: line }));
  });

  const guide = svg("line", { class: "guide", y1: pad.top, y2: pad.top + plotH, visibility: "hidden" });
  const dots = svg("g", { class: "dots" });
  const hit = svg("rect", { class: "hit", x: pad.left, y: pad.top, width: Math.max(0, plotW), height: plotH });
  const chart = svg(
    "svg",
    {
      class: "chart",
      width,
      height,
      viewBox: `0 0 ${width} ${height}`,
      role: "img",
      tabindex: "0",
      "aria-label": t("chartHelp"),
    },
    grid,
    labels,
    marks,
    guide,
    dots,
    hit,
  );

  const show = (i) => {
    if (i === null || i < 0 || i >= n) {
      usage.hover = null;
      guide.setAttribute("visibility", "hidden");
      dots.replaceChildren();
      tip.classList.remove("shown");
      return;
    }
    usage.hover = i;
    guide.setAttribute("x1", x(i));
    guide.setAttribute("x2", x(i));
    guide.setAttribute("visibility", "visible");
    dots.replaceChildren(...series.map((s) => svg("circle", { class: `dot ${s.tool}`, cx: x(i), cy: y(s[by][i]), r: 4 })));
    const rows = [...series]
      .sort((a, b) => b[by][i] - a[by][i])
      .map((s) =>
        h(
          "div",
          { class: "tip-row" },
          h("span", { class: `tip-key ${s.tool}`, "aria-hidden": "true" }),
          h("strong", { text: by === "cost" ? money(s.cost[i]) : compact(s.tokens[i]) }),
          h("span", { text: (toolOf(s.tool) || { name: s.tool }).name }),
        ),
      );
    tip.replaceChildren(h("div", { class: "tip-when", text: columnLabel(answer.starts[i], hourly) }), ...rows);
    tip.classList.add("shown");
    // Beside the guide, on whichever side has room.
    const wide = tip.offsetWidth;
    tip.style.left = `${x(i) + 12 + wide <= width ? x(i) + 12 : Math.max(0, x(i) - 12 - wide)}px`;
  };
  const nearest = (event) => {
    const rect = chart.getBoundingClientRect();
    const px = event.clientX - rect.left;
    return n > 1 ? Math.round(((px - pad.left) / plotW) * (n - 1)) : 0;
  };
  hit.addEventListener("pointermove", (event) => show(Math.max(0, Math.min(n - 1, nearest(event)))));
  hit.addEventListener("pointerleave", () => show(null));
  chart.addEventListener("keydown", (event) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const from = usage.hover === null ? n - 1 : usage.hover + (event.key === "ArrowLeft" ? -1 : 1);
    show(Math.max(0, Math.min(n - 1, from)));
  });
  chart.addEventListener("blur", () => show(null));
  host.replaceChildren(chart);
  if (usage.hover !== null) show(Math.min(usage.hover, n - 1));
}

function sum(values) {
  return values.reduce((a, b) => a + b, 0);
}

// ---------- limits ----------

function renderLimits(moves) {
  if (!snap) return empty({ iconName: "usage", title: t("readingAccounts"), tone: "quiet" });
  const tools = snap.tools.filter((tool) => tool.accounts.length && (usage.tool === "all" || tool.id === usage.tool));
  if (!tools.length) return empty({ iconName: "accounts", title: t("noAccounts"), detail: t("noAccountsDetail") });
  return h(
    "div",
    { class: "accounts" },
    tools.map((tool) =>
      h(
        "section",
        { class: "group", "aria-label": tool.name },
        h("h2", { class: "group-head" }, h("span", { class: `tool-mark ${tool.id}`, "aria-hidden": "true" }, toolIcon(tool.id)), h("span", { text: tool.name })),
        h(
          "div",
          { class: "panel" },
          tool.accounts.map((account) => {
            const needsSignIn = account.usage && account.usage.status === "unauthorized";
            return h(
              "div",
              { class: "row" + (account.inUse ? " in-use" : "") + (needsSignIn ? " attention" : "") },
              h("div", { class: "avatar", "aria-hidden": "true" }, icon(needsSignIn ? "warning" : "user")),
              h(
                "div",
                { class: "identity" },
                h("div", { class: "row-title" }, h("span", { class: "row-name", text: account.name }), account.plan ? h("span", { class: "tag", text: account.plan }) : null),
                account.label && account.label !== account.name ? h("div", { class: "row-email" }, emailMask(`limits|${rowKey(tool.id, account.name)}`, account.label)) : null,
              ),
              h("div", { class: "row-side" }, account.inUse ? h("span", { class: "in-use-pill" }, icon("check"), t("inUse")) : null),
              metersOf(tool, account, moves),
              notesOf(tool, account),
            );
          }),
        ),
      ),
    ),
  );
}

// ---------- start ----------

listen("usage-changed", () => {
  if (pane === "usage") loadUsage();
});

// New lines land in the logs as the tools work.
setInterval(() => {
  if (pane === "usage" && usage.tab !== "limits" && !document.hidden) loadUsage();
}, 60000);

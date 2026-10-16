(function () {
  "use strict";

  var REPO = "Benpiaz/piaz-switch";
  var LATEST = "https://github.com/" + REPO + "/releases/latest/download/";
  var root = document.documentElement;

  function lang() {
    return root.getAttribute("data-lang") === "en" ? "en" : "vi";
  }

  // ---------- language ----------
  var listeners = [];
  function setLang(next) {
    root.setAttribute("data-lang", next);
    root.lang = next;
    try {
      localStorage.setItem("lang", next);
    } catch (e) {}
    listeners.forEach(function (fn) {
      fn(next);
    });
  }
  document.querySelectorAll("[data-lang-toggle]").forEach(function (button) {
    button.addEventListener("click", function () {
      setLang(lang() === "vi" ? "en" : "vi");
    });
  });

  // ---------- mobile menu ----------
  var toggle = document.querySelector(".menu-toggle");
  var menu = document.getElementById("mobile-menu");
  if (toggle && menu) {
    toggle.addEventListener("click", function () {
      var open = menu.hidden;
      menu.hidden = !open;
      toggle.setAttribute("aria-expanded", String(open));
    });
    menu.addEventListener("click", function (event) {
      if (event.target.closest("a")) {
        menu.hidden = true;
        toggle.setAttribute("aria-expanded", "false");
      }
    });
  }

  // ---------- which computer is this ----------
  function detectOs() {
    var ua = navigator.userAgent || "";
    var platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "";
    if (/android|iphone|ipad|ipod/i.test(ua) || (/Mac/.test(platform) && navigator.maxTouchPoints > 1)) return "mobile";
    if (/win/i.test(platform) || /Windows/.test(ua)) return "windows";
    if (/mac/i.test(platform) || /Mac OS X/.test(ua)) return "mac";
    return "other";
  }

  // The Mac's chip from the GPU WebGL reports: "Apple M1…" on Apple Silicon,
  // Intel/AMD on Intel Macs. Safari says "Apple GPU" on every Mac, so that
  // (and anything else) is unknown: no card is marked and the hint under the
  // cards tells how to check.
  function macChipFromGpu() {
    try {
      var gl = document.createElement("canvas").getContext("webgl");
      var info = gl && gl.getExtension("WEBGL_debug_renderer_info");
      var renderer = info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : "";
      if (/Apple M\d/i.test(renderer)) return "arm";
      if (/Intel|AMD|Radeon|NVIDIA/i.test(renderer)) return "intel";
    } catch (e) {}
    return null;
  }

  // Chrome and Edge tell the CPU architecture outright; ask them first.
  function macChip(done) {
    var uaData = navigator.userAgentData;
    if (!uaData || !uaData.getHighEntropyValues) return done(macChipFromGpu());
    uaData.getHighEntropyValues(["architecture"]).then(
      function (values) {
        if (values.architecture === "arm") return done("arm");
        if (values.architecture === "x86") return done("intel");
        done(macChipFromGpu());
      },
      function () {
        done(macChipFromGpu());
      }
    );
  }

  var os = detectOs();
  var suggested = null;
  if (os === "windows") suggested = document.querySelector('[data-os="windows"]');

  var labels = {
    windows: { vi: "Tải cho Windows", en: "Download for Windows" },
    mac: { vi: "Tải cho macOS", en: "Download for macOS" },
  };
  var suggestText = { vi: "Hợp với máy bạn", en: "For your computer" };

  function paintSuggestion() {
    if (suggested) suggested.setAttribute("data-suggest", suggestText[lang()]);
    if (!labels[os]) return;
    document.querySelectorAll("[data-primary-label]").forEach(function (el) {
      el.textContent = labels[os][lang()];
    });
  }

  function suggest(card) {
    suggested = card;
    if (!suggested) return;
    suggested.classList.add("suggested");
    paintSuggestion();
  }

  suggest(suggested);
  if (os === "mac") {
    macChip(function (chip) {
      if (chip) suggest(document.querySelector(chip === "intel" ? '[data-os="mac-intel"]' : '[data-os="mac-arm"]'));
    });
  }
  if (labels[os]) {
    document.querySelectorAll("[data-os-icon] use").forEach(function (use) {
      use.setAttribute("href", os === "windows" ? "#i-windows" : "#i-apple");
    });
  }
  // On Windows the hero button downloads the installer at once; elsewhere it
  // goes to the cards, where the right one is marked when the chip is known.
  if (os === "windows") {
    document.querySelectorAll(".hero [data-primary-download]").forEach(function (a) {
      a.href = LATEST + "PiazSwitch-windows-x64-setup.exe";
    });
  }
  paintSuggestion();
  listeners.push(paintSuggestion);

  // ---------- the app on the sky ----------
  var shots = {
    accounts: {
      src: "assets/accounts-light.webp",
      alt: { vi: "Piaz Switch: mỗi tài khoản với hạn mức 5 giờ, tuần và theo model", en: "Piaz Switch: every account with its 5-hour, weekly and per-model limits" },
      caption: { vi: "Mọi tài khoản, hạn mức 5 giờ / tuần / theo model và nhịp dùng của bạn.", en: "Every account with its 5-hour, weekly and per-model limits and your pace." },
    },
    usage: {
      src: "assets/usage-light.webp",
      alt: { vi: "Pane Mức dùng: chi phí theo giá API và biểu đồ theo ngày", en: "Usage pane: cost at API prices and a daily chart per tool" },
      caption: { vi: "Token và chi phí theo giá API, theo tool và theo ngày.", en: "Tokens and cost at API prices, per tool and per day." },
    },
    settings: {
      src: "assets/settings-light.webp",
      alt: { vi: "Cài đặt: tự đổi tài khoản và warm-up", en: "Settings: automatic switching and warm-up" },
      caption: { vi: "Tự đổi trước khi hết hạn mức, warm-up cửa sổ 5 giờ kế tiếp.", en: "Switch before you run out, warm up the next 5-hour window." },
    },
  };
  // Load the other screenshots once the page has settled.
  window.addEventListener("load", function () {
    Object.keys(shots).forEach(function (k) {
      new Image().src = shots[k].src;
    });
  });

  var stage = document.querySelector(".demo-stage");
  var tabs = Array.prototype.slice.call(document.querySelectorAll('.demo-tabs [role="tab"]'));
  var panel = document.getElementById("shot");
  var img = document.querySelector("[data-shot-img]");
  var caption = document.querySelector("[data-shot-caption]");
  var TAB_MS = 7000;
  var current = 0;
  var timer = null;
  var userPicked = false;
  var visible = false;
  if (stage) stage.style.setProperty("--tab-ms", TAB_MS + "ms");

  function paintShot() {
    var shot = shots[tabs[current].getAttribute("data-shot")];
    img.alt = shot.alt[lang()];
    caption.textContent = shot.caption[lang()];
  }

  function show(index, focus) {
    current = (index + tabs.length) % tabs.length;
    tabs.forEach(function (tab, n) {
      tab.setAttribute("aria-selected", String(n === current));
      tab.tabIndex = n === current ? 0 : -1;
    });
    var tab = tabs[current];
    var shot = shots[tab.getAttribute("data-shot")];
    panel.setAttribute("aria-labelledby", tab.id);
    if (focus) tab.focus();
    img.classList.add("fading");
    setTimeout(function () {
      img.src = shot.src;
      paintShot();
      img.classList.remove("fading");
    }, 180);
    restartProgress();
  }

  function restartProgress() {
    // Re-trigger the bar's animation on the selected tab.
    var bar = tabs[current].querySelector(".tab-progress > span");
    if (bar) {
      bar.style.animation = "none";
      void bar.offsetWidth;
      bar.style.animation = "";
    }
  }

  function schedule() {
    clearInterval(timer);
    timer = null;
    var playing = visible && !userPicked && !document.hidden && !matchMedia("(prefers-reduced-motion: reduce)").matches;
    stage.classList.toggle("playing", playing);
    if (playing) {
      restartProgress();
      timer = setInterval(function () {
        show(current + 1);
      }, TAB_MS);
    }
  }

  if (stage && tabs.length && img) {
    tabs.forEach(function (tab, n) {
      tab.addEventListener("click", function () {
        userPicked = true;
        show(n);
        schedule();
      });
      tab.addEventListener("keydown", function (event) {
        var step = { ArrowRight: 1, ArrowLeft: -1 }[event.key];
        if (!step) return;
        event.preventDefault();
        userPicked = true;
        show(current + step, true);
        schedule();
      });
    });
    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        visible = entries[0].isIntersecting;
        schedule();
      }, { threshold: 0.35 }).observe(stage);
    }
    document.addEventListener("visibilitychange", schedule);
    listeners.push(paintShot);
  }

  // ---------- numbers from GitHub ----------
  function compact(n) {
    if (n >= 1000) return (n / 1000).toFixed(n >= 10000 ? 0 : 1).replace(/\.0$/, "") + "k";
    return String(n);
  }
  function getJson(url) {
    var key = "gh:" + url;
    try {
      var cached = JSON.parse(sessionStorage.getItem(key) || "null");
      if (cached && Date.now() - cached.at < 10 * 60 * 1000) return Promise.resolve(cached.data);
    } catch (e) {}
    return fetch(url, { headers: { Accept: "application/vnd.github+json" } })
      .then(function (r) {
        if (!r.ok) throw new Error(r.status);
        return r.json();
      })
      .then(function (data) {
        try {
          sessionStorage.setItem(key, JSON.stringify({ at: Date.now(), data: data }));
        } catch (e) {}
        return data;
      });
  }

  function fill(selector, text) {
    document.querySelectorAll(selector).forEach(function (el) {
      el.textContent = text;
      el.hidden = false;
    });
  }

  if (window.fetch) {
    getJson("https://api.github.com/repos/" + REPO)
      .then(function (repo) {
        fill("[data-stars]", compact(repo.stargazers_count || 0));
      })
      .catch(function () {});
    getJson("https://api.github.com/repos/" + REPO + "/releases?per_page=100")
      .then(function (releases) {
        var total = 0;
        releases.forEach(function (release) {
          (release.assets || []).forEach(function (asset) {
            // Installers and archives: what people download (Windows self-updates fetch the .exe too).
            if (/\.(exe|zip|dmg)$/.test(asset.name)) total += asset.download_count || 0;
          });
        });
        fill("[data-downloads]", compact(total));
        var latest = releases.filter(function (r) {
          return !r.draft && !r.prerelease;
        })[0];
        if (latest) {
          fill("[data-version]", latest.tag_name);
          fill("[data-version-text]", latest.tag_name);
        }
      })
      .catch(function () {});
  }
})();

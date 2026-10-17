// Limitray site: a faithful HTML recreation of the tray icon (src/icon.rs) and popup (src/popup.rs).
(() => {
  "use strict";

  const WARN = [245, 165, 36];
  const CRIT = [239, 68, 68];
  const BAD = "#ef5350", WARN_C = "#f5a524", GOOD = "#4cc38a", MUTED = "var(--pp-muted)";
  const THRESHOLDS = [80, 95]; // Config::default().notify_thresholds

  const rgb = (c, a = 1) => `rgba(${c[0]},${c[1]},${c[2]},${a})`;
  const $ = (s, el = document) => el.querySelector(s);

  // Deterministic pseudo-random so the mock looks the same on every load.
  function rng(seed) {
    return () => ((seed = (seed * 16807) % 2147483647) - 1) / 2147483646;
  }
  function days(seed, base, spread, zeros) {
    const r = rng(seed);
    return Array.from({ length: 30 }, (_, i) => {
      if (zeros.includes(i)) return { cost: 0, tokens: 0 };
      const cost = base * (0.35 + r() * spread) * (i > 22 ? 1.25 : 1);
      return { cost, tokens: Math.round(cost * (190000 + r() * 60000)) };
    });
  }

  const providers = {
    claude: {
      name: "Claude", plan: "Max", brand: [217, 119, 87],
      status: "All Systems Operational",
      windows: [
        { label: "Session", minutes: 300, used: 42, expected: 55, resetsMin: 134 },
        { label: "Weekly", minutes: 10080, used: 71, expected: 64, resetsMin: 2 * 1440 + 5 * 60 + 20 },
      ],
      spend: { days: days(7, 14, 1.6, [5, 12, 13]), models: [["Opus 5.5", 0.71], ["Sonnet 5.5", 0.24], ["Haiku 5.5", 0.05]] },
    },
    codex: {
      name: "Codex", plan: "Plus", brand: [16, 163, 127],
      status: "All Systems Operational",
      windows: [
        { label: "Session", minutes: 300, used: 88, expected: 60, resetsMin: 96, runsOut: 72 },
        { label: "Weekly", minutes: 10080, used: 34, expected: 40, resetsMin: 4 * 1440 + 9 * 60 },
      ],
      spend: { days: days(29, 6, 1.4, [0, 1, 6, 7, 14, 20, 21]), models: [] },
    },
    gemini: {
      name: "Gemini", plan: null, brand: [66, 133, 244],
      status: "",
      windows: [
        { label: "Daily", minutes: 1440, used: 12, resetsMin: 9 * 60 + 41 },
      ],
      spend: null, // local spend is read from ~/.claude and ~/.codex only
    },
  };
  const order = ["claude", "codex", "gemini"];

  // ───── formatting (mirrors src/format.rs) ─────
  function duration(mins) {
    const m = Math.max(0, Math.round(mins));
    if (m < 60) return `${m}m`;
    if (m < 1440) return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, "0")}m`;
    return `${Math.floor(m / 1440)}d ${Math.floor((m % 1440) / 60)}h`;
  }
  function clock(mins) {
    const at = new Date(Date.now() + mins * 60000);
    const now = new Date();
    const hm = at.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
    const dayDiff = Math.round((new Date(at.toDateString()) - new Date(now.toDateString())) / 86400000);
    if (dayDiff === 0) return hm;
    if (dayDiff < 7) return `${at.toLocaleDateString("en-US", { weekday: "short" })} ${hm}`;
    return `${at.toLocaleDateString("en-US", { month: "short", day: "numeric" })} ${hm}`;
  }
  function money(v) {
    if (v >= 1000) return `$${v.toFixed(0)}`;
    if (v >= 100) return `$${v.toFixed(1)}`;
    return `$${v.toFixed(2)}`;
  }
  function tokens(v) {
    if (v >= 1e9) return `${(v / 1e9).toFixed(2)}B`;
    if (v >= 1e6) return `${(v / 1e6).toFixed(1)}M`;
    if (v >= 1e3) return `${(v / 1e3).toFixed(1)}K`;
    return String(v);
  }
  function cadence(min) {
    if (min >= 1440) return `${Math.round(min / 1440)}d`;
    if (min >= 60 && min % 60 === 0) return `${min / 60}h`;
    return min > 0 ? `${min}m` : "";
  }

  // ───── colours (mirrors level_color / pace_color) ─────
  function levelColor(used, brand) {
    if (used >= THRESHOLDS[1]) return BAD;
    if (used >= THRESHOLDS[0]) return WARN_C;
    return rgb(brand);
  }
  function paceColor(w) {
    const d = w.used - w.expected;
    if (w.runsOut && d > 0) return WARN_C;
    if (d <= -5) return GOOD;
    if (d >= 15) return BAD;
    if (d >= 5) return WARN_C;
    return MUTED;
  }

  // ───── tray icon (mirrors src/icon.rs, drawn on a 100×100 viewBox) ─────
  const S = 100;
  function level(v) {
    const used = v * 100;
    if (used >= THRESHOLDS[1]) return "crit";
    if (used >= THRESHOLDS[0]) return "warn";
    return "normal";
  }
  function trayColor(lvl, brand) {
    return lvl === "crit" ? rgb(CRIT) : lvl === "warn" ? rgb(WARN) : rgb(brand);
  }
  function capsule(x, y, w, h, fill) {
    if (w <= 0.2 || h <= 0.2) return "";
    const r = Math.min(h / 2, w / 2);
    return `<rect x="${x}" y="${y}" width="${w}" height="${h}" rx="${r}" fill="${fill}"/>`;
  }
  function bar(x, y, w, h, v, track, color) {
    let out = capsule(x, y, w, h, track);
    if (v > 0) out += capsule(x, y, Math.min(Math.max(w * v, h), w), h, color);
    return out;
  }
  function arc(r, v, width, color) {
    if (v <= 0.001) return "";
    const c = S / 2;
    if (v >= 0.999) return `<circle cx="${c}" cy="${c}" r="${r}" fill="none" stroke="${color}" stroke-width="${width}"/>`;
    const a0 = -Math.PI / 2, a1 = a0 + 2 * Math.PI * v;
    const [x0, y0, x1, y1] = [c + r * Math.cos(a0), c + r * Math.sin(a0), c + r * Math.cos(a1), c + r * Math.sin(a1)];
    return `<path d="M${x0} ${y0} A${r} ${r} 0 ${v > 0.5 ? 1 : 0} 1 ${x1} ${y1}" fill="none" stroke="${color}" stroke-width="${width}" stroke-linecap="round"/>`;
  }
  function trayIcon(style, { primary, secondary, brand, provider, dark = true }) {
    const fg = dark ? [252, 252, 252] : [35, 38, 41];
    const track = dark ? "rgba(255,255,255,0.26)" : "rgba(0,0,0,0.20)";
    const p = primary / 100, s = secondary == null ? 0 : secondary / 100;
    const pc = trayColor(level(p), brand), sc = trayColor(level(s), brand);
    if (style === "bars") {
      const pad = S * 0.06, h = S * 0.3, gap = S * 0.14, top = (S - 2 * h - gap) / 2;
      return bar(pad, top, S - 2 * pad, h, p, track, pc) + bar(pad, top + h + gap, S - 2 * pad, h, s, track, sc);
    }
    if (style === "rings") {
      const w = S * 0.15, r1 = S / 2 - w / 2 - S * 0.02, r2 = S / 2 - w * 1.5 - S * 0.08;
      return `<circle cx="50" cy="50" r="${r1}" fill="none" stroke="${track}" stroke-width="${w}"/>` + arc(r1, p, w, pc) +
        `<circle cx="50" cy="50" r="${r2}" fill="none" stroke="${track}" stroke-width="${w * 0.85}"/>` + arc(r2, s, w * 0.85, sc);
    }
    const barH = S * 0.16;
    const bottom = bar(S * 0.04, S - barH - S * 0.02, S * 0.92, barH, p, track, pc);
    if (style === "number") {
      const lvl = level(p);
      const color = lvl === "normal" ? rgb(fg) : pc;
      const textH = S - barH - S * 0.14;
      const label = String(Math.round(primary));
      const fit = label.length > 2 ? ` textLength="96" lengthAdjust="spacingAndGlyphs"` : "";
      return `<text x="50" y="${textH * 0.5}" dominant-baseline="central" text-anchor="middle" font-family="Inter, sans-serif" font-weight="700"
        font-size="${label.length > 2 ? 52 : 74}" fill="${color}"${fit}>${label}</text>` + bottom;
    }
    // logo: the provider SVGs are white template icons, tinted through a shared mask (see #icon-masks)
    const logo = S - barH - S * 0.1;
    const tint = level(p) === "normal" ? rgb(brand) : pc;
    return `<rect x="${(S - logo) / 2}" y="0" width="${logo}" height="${logo}" fill="${tint}" mask="url(#m-${provider})"/>` + bottom;
  }

  function installMasks() {
    const logo = S - S * 0.16 - S * 0.1;
    const defs = order.map((id) =>
      `<mask id="m-${id}" maskUnits="userSpaceOnUse" x="0" y="0" width="100" height="100"><image href="/icons/${id}.svg" x="${(S - logo) / 2}" y="0" width="${logo}" height="${logo}"/></mask>`
    ).join("");
    document.body.insertAdjacentHTML("afterbegin",
      `<svg id="icon-masks" width="0" height="0" style="position:absolute" aria-hidden="true"><defs>${defs}</defs></svg>`);
  }

  function svgInto(el, inner) {
    el.setAttribute("viewBox", "0 0 100 100");
    el.innerHTML = inner;
  }

  // ───── popup ─────
  let selected = "claude";
  let trayWindow = 0; // which window the single-value styles track; scroll flips it

  function logoEl(id, cls, color) {
    const s = document.createElement("span");
    s.className = `logo ${cls || ""}`;
    s.style.setProperty("--src", `url(/icons/${id}.svg)`);
    s.style.color = color;
    return s;
  }

  function renderTabs() {
    const tabs = $("#pp-tabs");
    tabs.innerHTML = "";
    for (const id of order) {
      const p = providers[id];
      const b = document.createElement("button");
      b.className = "tab" + (id === selected ? " on" : "");
      b.setAttribute("role", "tab");
      b.setAttribute("aria-selected", id === selected);
      b.appendChild(logoEl(id, "", id === selected ? rgb(p.brand) : "var(--pp-muted)"));
      const t = document.createElement("span");
      t.innerHTML = `${p.name}&nbsp; <span class="pct">${p.windows[0].used}%</span>`;
      b.appendChild(t);
      b.addEventListener("click", () => { selected = id; renderPopup(); renderTray(); });
      tabs.appendChild(b);
    }
  }

  function windowCard(p, w) {
    const accent = levelColor(w.used, p.brand);
    const cad = cadence(w.minutes);
    const showCad = cad && !w.label.toLowerCase().includes(cad);
    let pill = "";
    if (w.expected != null) {
      const d = w.used - w.expected;
      const [txt, col] = Math.abs(d) < 1 ? ["on pace", MUTED]
        : d < 0 ? [`${Math.round(-d)}% in reserve`, paceColor(w)]
        : [`${Math.round(d)}% ahead of pace`, paceColor(w)];
      pill = `<span class="pill" style="--c:${col}">${txt}</span>`;
    }
    const mark = w.expected != null
      ? `<span class="mark" style="left:${w.expected}%" title="Pace marker: ${w.expected}% used expected by now"></span>` : "";
    const detail = w.runsOut ? `<div class="detail" style="color:${paceColor(w)}">Runs out in ${duration(w.runsOut)}</div>` : "";
    return `<div class="card">
      <div class="card-row"><span class="card-title">${w.label}</span>${showCad ? `<span class="cad">${cad}</span>` : ""}
        <span class="resets">resets in ${duration(w.resetsMin)} · ${clock(w.resetsMin)}</span></div>
      <div class="card-row mid"><span class="big">${w.used}%</span><span class="suffix">used</span>${pill}</div>
      <div class="bar"><span class="fill" style="width:${w.used}%;background:${accent}"></span>${mark}</div>
      ${detail}
    </div>`;
  }

  function spendCard(p) {
    const ds = p.spend.days;
    const today = ds[ds.length - 1];
    const total = ds.reduce((a, d) => a + d.cost, 0);
    const tok = ds.reduce((a, d) => a + d.tokens, 0);
    const max = Math.max(0.01, ...ds.map((d) => d.cost));
    const caption = p.spend.models.length
      ? p.spend.models.map(([m, share]) => `${m} ${money(total * share)}`).join(" · ")
      : "Hover a bar for details";
    const bars = ds.map((d, i) => {
      const h = d.cost > 0 ? Math.max(2, (d.cost / max) * 52) : 1;
      const col = d.cost > 0 ? rgb(p.brand, i === ds.length - 1 ? 0.9 : 0.55) : "var(--pp-track)";
      return `<i data-i="${i}" style="height:${h}px;background:${col}"></i>`;
    }).join("");
    return `<div class="card" id="spend">
      <div class="spend-head"><span class="card-title">Spend</span><span class="cad">API list-price estimate · local logs</span></div>
      <div class="stats">
        <div class="stat"><div class="l">Today</div><div class="v">${money(today.cost)}</div><div class="s">${tokens(today.tokens)} tokens</div></div>
        <div class="stat"><div class="l">30 days</div><div class="v">${money(total)}</div><div class="s">${tokens(tok)} tokens</div></div>
        <div class="stat"><div class="l">Daily avg</div><div class="v">${money(total / 30)}</div><div class="s">last 30 days</div></div>
      </div>
      <div class="spark">${bars}</div>
      <div class="caption" data-default="${caption}">${caption}</div>
    </div>`;
  }

  function bindSpend(p) {
    const card = $("#spend");
    if (!card) return;
    const cap = $(".caption", card);
    const bars = [...card.querySelectorAll(".spark i")];
    const ds = p.spend.days;
    const show = (i) => {
      bars.forEach((b, j) => {
        const d = ds[j];
        b.style.background = j === i ? rgb(p.brand) : d.cost > 0 ? rgb(p.brand, j === ds.length - 1 ? 0.9 : 0.55) : "var(--pp-track)";
      });
      if (i == null) { cap.textContent = cap.dataset.default; return; }
      const date = new Date(Date.now() - (ds.length - 1 - i) * 86400000);
      const label = date.toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric" }).replace(",", "");
      cap.textContent = `${label} · ${money(ds[i].cost)} · ${tokens(ds[i].tokens)} tokens`;
    };
    const spark = $(".spark", card);
    const pick = (x) => {
      const r = spark.getBoundingClientRect();
      return Math.min(ds.length - 1, Math.max(0, Math.floor(((x - r.left) / r.width) * ds.length)));
    };
    spark.addEventListener("pointermove", (e) => show(pick(e.clientX)));
    spark.addEventListener("pointerleave", () => show(null));
  }

  function renderPopup() {
    const p = providers[selected];
    const logo = $("#pp-logo");
    logo.style.setProperty("--src", `url(/icons/${selected}.svg)`);
    logo.style.color = rgb(p.brand);
    $("#pp-name").textContent = p.name;
    const plan = $("#pp-plan");
    plan.hidden = !p.plan;
    plan.textContent = p.plan || "";
    plan.style.setProperty("--c", rgb(p.brand));
    const st = $("#pp-status");
    st.textContent = p.status ? `${p.status} · updated just now` : "updated just now";
    $(".sdot").style.background = p.status ? GOOD : "var(--pp-faint)";
    $(".pp-foot .link").style.display = selected === "claude" ? "" : "none";
    renderTabs();
    let html = p.windows.map((w) => windowCard(p, w)).join("");
    if (p.spend) html += spendCard(p);
    $("#pp-cards").innerHTML = html;
    bindSpend(p);
  }

  // ───── hero tray icon ─────
  function renderTray() {
    const p = providers[selected];
    const [a, b] = p.windows;
    // Bars always draw session on top and weekly below (tray.rs), whatever window is tracked.
    svgInto($("#tray-svg"), trayIcon("bars", { primary: a.used, secondary: b ? b.used : null, brand: p.brand, provider: selected }));
    $("#tray-me").title = `${p.name} — ${trayWindow === 0 ? "session" : "weekly"} · left click: popup · middle click: refresh · scroll: switch`;
  }

  function initHero() {
    const me = $("#tray-me"), popup = $("#popup");
    me.classList.add("open");
    me.addEventListener("click", () => {
      popup.classList.toggle("closed");
      me.classList.toggle("open", !popup.classList.contains("closed"));
    });
    const refresh = () => {
      const btn = $("#pp-refresh");
      btn.classList.add("busy");
      me.classList.remove("spin"); void me.offsetWidth; me.classList.add("spin");
      $("#pp-status").textContent = "refreshing…";
      setTimeout(() => { btn.classList.remove("busy"); renderPopup(); }, 900);
    };
    me.addEventListener("auxclick", (e) => { if (e.button === 1) { e.preventDefault(); refresh(); } });
    me.addEventListener("mousedown", (e) => { if (e.button === 1) e.preventDefault(); });
    $("#pp-refresh").addEventListener("click", refresh);
    // Scroll cycles Claude session → Claude weekly → Codex session → …, like Tray::scroll.
    let wheelLock = 0;
    me.addEventListener("wheel", (e) => {
      e.preventDefault();
      if (Date.now() < wheelLock || e.deltaY === 0) return;
      wheelLock = Date.now() + 180;
      const views = order.flatMap((id) => [[id, 0], [id, 1]]);
      const cur = views.findIndex(([id, w]) => id === selected && w === trayWindow);
      const [id, w] = views[(cur + Math.sign(e.deltaY) + views.length) % views.length];
      trayWindow = w;
      if (id !== selected) { selected = id; renderPopup(); }
      renderTray();
    }, { passive: false });

    const tick = () => {
      $("#clock").textContent = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
    };
    tick();
    setInterval(tick, 15000);
    renderPopup();
    renderTray();
  }

  // ───── tray style playground ─────
  const styles = [
    ["bars", "Bars", "Session + weekly"],
    ["rings", "Rings", "Session + weekly"],
    ["number", "Number", "Percent + bar"],
    ["logo", "Logo", "Provider logo + bar"],
  ];
  const play = { session: 42, weekly: 71, provider: "claude", dark: true };

  function renderStyles() {
    const grid = $("#style-grid");
    if (!grid.children.length) {
      grid.innerHTML = styles.map(([id, name, sub]) => `
        <div class="style-card">
          <div class="style-stage" data-style="${id}">
            <svg aria-hidden="true"></svg>
            <div class="mini"><span></span><svg aria-hidden="true"></svg></div>
          </div>
          <div class="style-label"><b>${name}</b><span>${sub}</span></div>
        </div>`).join("");
    }
    const brand = providers[play.provider].brand;
    for (const stage of grid.querySelectorAll(".style-stage")) {
      const style = stage.dataset.style;
      stage.classList.toggle("light", !play.dark);
      const spec = { primary: play.session, secondary: play.weekly, brand, provider: play.provider, dark: play.dark };
      const [big, small] = stage.querySelectorAll("svg");
      svgInto(big, trayIcon(style, spec));
      svgInto(small, trayIcon(style, spec));
      $(".mini span", stage).textContent = new Date().toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
    }
    $("#v-session").textContent = `${play.session}%`;
    $("#v-weekly").textContent = `${play.weekly}%`;
  }

  function initStyles() {
    $("#s-session").addEventListener("input", (e) => { play.session = +e.target.value; renderStyles(); });
    $("#s-weekly").addEventListener("input", (e) => { play.weekly = +e.target.value; renderStyles(); });
    for (const seg of document.querySelectorAll(".controls .seg")) {
      seg.addEventListener("click", (e) => {
        const b = e.target.closest("button");
        if (!b) return;
        seg.querySelectorAll("button").forEach((x) => x.classList.toggle("on", x === b));
        if (b.dataset.prov) play.provider = b.dataset.prov;
        if (b.dataset.panel) play.dark = b.dataset.panel === "dark";
        renderStyles();
      });
    }
    renderStyles();
  }

  // ───── copy buttons ─────
  function initCopy() {
    for (const btn of document.querySelectorAll(".copy")) {
      btn.addEventListener("click", async () => {
        const text = btn.dataset.copy;
        try {
          await navigator.clipboard.writeText(text);
        } catch {
          const ta = document.createElement("textarea");
          ta.value = text; document.body.appendChild(ta); ta.select();
          try { document.execCommand("copy"); } catch { /* ignore */ }
          ta.remove();
        }
        btn.textContent = "Copied";
        btn.classList.add("done");
        setTimeout(() => { btn.textContent = "Copy"; btn.classList.remove("done"); }, 1600);
      });
    }
  }

  // ───── latest release ─────
  async function initRelease() {
    try {
      const r = await fetch("https://api.github.com/repos/gabors0/limitray/releases/latest", { headers: { Accept: "application/vnd.github+json" } });
      if (!r.ok) return;
      const j = await r.json();
      if (j && typeof j.tag_name === "string" && /^v?\d+\.\d+\.\d+/.test(j.tag_name)) {
        $("#release-text").textContent = `${j.tag_name} is out`;
      }
    } catch { /* keep the static fallback */ }
  }

  installMasks();
  initHero();
  initStyles();
  initCopy();
  initRelease();
})();

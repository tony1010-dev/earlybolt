const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const $ = (id) => document.getElementById(id);

let state = null;
let view = "home";
let T = I18N.en;

// Service badges (simple monograms in each service's color)
const BADGE = {
  claude: { label: "✳", bg: "#D97757" },
  codex: { label: ">_", bg: "#1f1f1f" },
  grok: { label: "G", bg: "#3b3b3f" },
  agy: { label: "A", bg: "linear-gradient(135deg,#4285F4,#34A853)" },
  muse: { label: "∞", bg: "#0866FF" },
  kimi: { label: "K", bg: "#16191E" },
};

const hhmm = (m) => `${String(Math.floor(m / 60) % 24).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
const clock = (ts) => { const d = new Date(ts * 1000); return hhmm(d.getHours() * 60 + d.getMinutes()); };
const toMin = (v) => { const [h, m] = v.split(":").map(Number); return h * 60 + m; };
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

function remaining(ts) {
  const m = Math.max(0, Math.floor((ts - state.now) / 60));
  return m >= 60 ? T.inH(Math.floor(m / 60), m % 60) : T.inM(m);
}

function when(ts) {
  const d = new Date(ts * 1000);
  return new Date().toDateString() === d.toDateString() ? clock(ts) : `${d.getMonth() + 1}/${d.getDate()} ${clock(ts)}`;
}

const badge = (id) => `<span class="badge" style="background:${BADGE[id].bg}">${BADGE[id].label}</span>`;

/** A tool is usable when it is installed and not known to be signed out */
const ready = (t) => t.installed && t.logged_in !== false;

/** Time until a reset: "2d 2h", "2h 53m", "12m" */
function until(ts) {
  const m = Math.max(0, Math.floor((ts - state.now) / 60));
  const d = Math.floor(m / 1440), h = Math.floor((m % 1440) / 60);
  return d ? T.dh(d, h) : h ? T.hm(h, m % 60) : T.m(m % 60);
}

/** The window closest to its limit decides what to show (ties keep the 5-hour window, listed first) */
const headline = (u) => u.windows.reduce((a, w) => (!a || w.used > a.used ? w : a), null);
const windowName = (w) => `${w.group ? `${w.group} ` : ""}${T.kind[w.kind] || w.kind}`;

/** The 5-hour and weekly windows to show; a service metering model groups separately shows its busiest group */
function shownWindows(u) {
  const top = headline(u);
  const order = { "5h": 0, week: 1, month: 2 };
  return u.windows.filter((w) => w.group === top.group).sort((a, b) => order[a.kind] - order[b.kind]);
}

function usageRow(w) {
  const pct = Math.round(w.used);
  const sev = pct >= 90 ? "bad" : pct >= 70 ? "warn" : "";
  return `<div class="urow"><span class="faint ukind">${windowName(w)}</span>`
    + `<div class="ubar"><div class="${sev}" style="width:${Math.max(pct, 1)}%"></div></div>`
    + `<b class="pct ${sev}">${pct}%</b><span class="muted utime">${w.resets_at ? until(w.resets_at) : ""}</span></div>`;
}

function renderUsage() {
  const cells = state.tools.filter((t) => t.installed && t.has_usage).map((t) => {
    const u = t.usage;
    let body, tip = "";
    if (!u) body = `<span class="faint">…</span>`;
    else if (u.error) {
      const msg = { stale: T.usageStale, signed_out: T.signInNeeded }[u.error] || T.usageError;
      body = `<span class="faint">⚠ ${msg}</span>`;
      tip = u.error;
    } else if (!u.windows.length) body = `<span class="faint">—</span>`;
    else {
      body = shownWindows(u).map(usageRow).join("");
      tip = u.windows.map((x) => `${windowName(x)}  ${Math.round(x.used)}%${x.resets_at ? ` · ${T.resets(when(x.resets_at))}` : ""}`).join("\n");
    }
    return `<div class="ucell" title="${esc(tip)}">${badge(t.id)}<div class="ulines">${body}</div></div>`;
  });
  $("usageBox").style.display = cells.length ? "" : "none";
  $("usage").innerHTML = cells.join("");
  $("usageRefresh").classList.toggle("spin", state.usage_busy);
}

function applyStatic() {
  document.documentElement.lang = state.lang;
  document.querySelectorAll("[data-i18n]").forEach((el) => { el.textContent = T[el.dataset.i18n]; });
  document.querySelectorAll("[data-i18n-title]").forEach((el) => { el.title = T[el.dataset.i18nTitle]; });
}

function renderHome() {
  const s = state;
  $("enabled").checked = s.enabled;

  let status = "";
  if (!s.enabled) status = T.off;
  else if (s.progress) status = T.pinging(s.progress.done + 1, s.progress.total, s.progress.current);
  else if (s.next_slot) status = T.next(clock(s.next_slot), remaining(s.next_slot));
  $("statusLine").textContent = status;

  $("bar").style.display = s.progress ? "" : "none";
  if (s.progress) $("barFill").style.width = `${((s.progress.done + 0.5) / s.progress.total) * 100}%`;

  const r = s.last_run;
  const sum = $("summaryLine");
  if (s.progress || !r || r.total === 0) {
    sum.textContent = s.enabled && s.current_window && !s.progress ? T.window(clock(s.current_window[0]), clock(s.current_window[1])) : "";
    sum.className = "small muted";
  } else if (r.failed.length === 0) {
    sum.textContent = T.allOk(r.ok, r.total, when(r.at));
    sum.className = "small ok";
  } else {
    sum.textContent = T.someFail(r.ok, r.total, r.failed.join(", "));
    sum.className = "small bad";
  }

  $("homeTools").innerHTML = s.tools.map((t) => {
    let stat;
    const cur = s.progress && s.progress.current === t.name;
    if (cur) stat = `<span class="muted">…</span>`;
    else if (t.busy) stat = `<span class="muted">${t.busy === "installing" ? T.installing : T.signingIn}</span>`;
    else if (t.installed === null) stat = `<span class="faint">…</span>`;
    else if (!ready(t)) stat = `<button class="link" data-goto="settings">${t.installed ? T.signInNeeded : T.notInstalled} · ${T.setup} ›</button>`;
    else if (t.result) stat = `<span class="${t.result.ok ? "ok" : "bad"}" title="${esc(t.result.message)}">${t.result.ok ? "✓" : "✕"} ${when(t.result.at)}</span>`;
    else stat = `<span class="faint">${T.noRecord}</span>`;
    return `<div class="trow ${ready(t) && t.on ? "" : "dim"}">
      ${badge(t.id)}<span class="name grow">${t.name}</span><span class="stat">${stat}</span>
      <label class="switch sm"><input type="checkbox" data-tool="${t.id}" ${t.on ? "checked" : ""} ${ready(t) ? "" : "disabled"}><span></span></label>
    </div>`;
  }).join("");

  renderUsage();

  const nextHm = s.next_slot ? clock(s.next_slot) : null;
  $("chips").innerHTML = s.schedule.map((m) => `<span class="chip ${hhmm(m) === nextHm && s.enabled ? "next" : ""}">${hhmm(m)}</span>`).join("");
  $("runNow").disabled = s.running || !s.tools.some((t) => ready(t) && t.on);
}

function renderSettings() {
  const s = state;
  // Keep a half-typed code across the 5-second refreshes
  const typed = $("code-agy") ? $("code-agy").value : "";
  const hadFocus = document.activeElement && document.activeElement.id === "code-agy";
  $("connTools").innerHTML = s.tools.map((t) => {
    let right, extra = "";
    if (t.busy === "awaiting_code") {
      // Antigravity: the browser shows a code that has to be pasted back within ~60 seconds
      const left = Math.max(0, (t.code_deadline || 0) - Math.floor(Date.now() / 1000));
      right = `<input class="code" id="code-agy" placeholder="${T.pasteCode}" spellcheck="false" autocomplete="off">
        <button class="mini primary" data-code="${t.id}">OK</button>
        <button class="mini ghost" data-reopen="1" title="${T.reopen}">↗</button>`;
      extra = `<div class="hint code-hint">${T.codeHint(left)}</div>`;
    } else if (t.busy) right = `<span class="muted">${({ installing: T.installing, verifying: T.verifying })[t.busy] || T.signingIn}</span>`;
    else if (t.installed === null) right = `<span class="faint">…</span>`;
    else if (!t.installed) right = `<button class="mini primary" data-install="${t.id}">${T.install}</button>`;
    else {
      const signed = t.logged_in === true || (t.logged_in === null && t.result && t.result.ok);
      right = signed
        ? `<span class="ok">✓ ${T.signedIn}</span><button class="mini ghost" data-login="${t.id}" title="${T.signIn}">↻</button>`
        // Antigravity's sign-in state can't be checked, so its button stays neutral until a ping succeeds
        : `<span class="faint">${T.installed}</span><button class="mini ${t.logged_in === false ? "primary" : ""}" data-login="${t.id}">${T.signIn}</button>`;
    }
    return `<div class="trow">${badge(t.id)}<span class="name grow">${t.name}</span><span class="stat row">${right}</span></div>${extra}`;
  }).join("");
  if ($("code-agy")) {
    $("code-agy").value = typed;
    if (hadFocus || !typed) $("code-agy").focus();
  }

  const mac = s.os === "macos";
  $("modeMini").textContent = mac ? T.miniMac : T.miniWin;
  $("modeLaptop").textContent = mac ? T.laptopMac : T.laptopWin;
  for (const b of $("mode").children) b.classList.toggle("on", b.dataset.mode === s.mode);
  $("modeHint").textContent = s.mode === "mini" ? T.miniHint : mac ? T.laptopHintMac : T.laptopHintWin;
  $("wakeBox").style.display = s.mode === "laptop" ? "" : "none";
  $("wakeMsg").textContent = s.wake_message;

  if (document.activeElement !== $("workStart")) $("workStart").value = hhmm(s.work_start);
  if (document.activeElement !== $("firstPing")) $("firstPing").value = hhmm(s.first_ping);
  $("applyRec").style.display = s.first_ping === s.recommended_first ? "none" : "";
  $("applyRec").textContent = T.use(hhmm(s.recommended_first));
  $("count").textContent = s.pings_per_day;
  $("minus").disabled = s.pings_per_day <= 1;
  $("plus").disabled = s.pings_per_day >= 4;

  $("autostart").checked = s.autostart;
  $("trayTime").checked = s.show_time_in_tray;
  $("trayTimeRow").style.display = mac ? "" : "none";
  $("trayUsage").checked = s.show_usage_in_tray;
  $("trayUsageRow").style.display = mac ? "" : "none";
  for (const b of $("lang").children) b.classList.toggle("on", b.dataset.lang === s.lang);
  $("version").textContent = T.version(s.version);
}

let lastHeight = 0;
function fit() {
  const h = Math.ceil(document.querySelector(".view.active").getBoundingClientRect().height);
  if (h && h !== lastHeight) {
    lastHeight = h;
    invoke("fit_panel", { height: h });
  }
}

function render() {
  T = I18N[state.lang] || I18N.en;
  applyStatic();
  renderHome();
  renderSettings();
  fit();
}

function show(v) {
  view = v;
  $("home").classList.toggle("active", v === "home");
  $("settings").classList.toggle("active", v === "settings");
  if (v === "settings") invoke("refresh_tools");
  fit();
}

async function refresh() {
  state = await invoke("get_state");
  render();
}

async function patch(p) {
  await invoke("update_settings", { patch: p });
  await refresh();
}

// Home
$("enabled").onchange = (e) => patch({ enabled: e.target.checked });
$("openSettings").onclick = () => show("settings");
$("runNow").onclick = async () => { await invoke("run_now"); setTimeout(refresh, 300); };
$("log").onclick = () => invoke("open_log");
$("guide").onclick = () => invoke("show_guide");
$("quit").onclick = () => invoke("quit");
$("usageRefresh").onclick = () => invoke("refresh_usage_cmd", { force: true });
$("homeTools").onclick = (e) => { if (e.target.dataset.goto) show("settings"); };
$("homeTools").onchange = (e) => {
  const id = e.target.dataset.tool;
  if (id) patch({ tools_on: { [id]: e.target.checked } });
};

// Settings
$("back").onclick = () => show("home");
$("connTools").onclick = (e) => {
  const d = e.target.dataset;
  if (d.install) invoke("install_tool", { id: d.install }).then(refresh);
  if (d.login) invoke("login_tool", { id: d.login }).then(refresh).catch((err) => alert(err));
  if (d.code) submitCode();
  if (d.reopen) invoke("reopen_sign_in");
};
$("connTools").onkeydown = (e) => { if (e.key === "Enter" && e.target.id === "code-agy") submitCode(); };

function submitCode() {
  const code = $("code-agy") ? $("code-agy").value : "";
  if (code.trim()) invoke("submit_code", { code }).then(refresh).catch((err) => alert(err));
}
$("mode").onclick = (e) => { const m = e.target.dataset.mode; if (m) patch({ mode: m }); };
$("wakeOn").onclick = async () => { await invoke("set_wake", { on: true }); refresh(); };
$("wakeOff").onclick = async () => { await invoke("set_wake", { on: false }); refresh(); };
$("workStart").onchange = (e) => e.target.value && patch({ work_start: toMin(e.target.value) });
$("firstPing").onchange = (e) => e.target.value && patch({ first_ping: toMin(e.target.value) });
$("applyRec").onclick = () => patch({ first_ping: state.recommended_first });
$("minus").onclick = () => patch({ pings_per_day: state.pings_per_day - 1 });
$("plus").onclick = () => patch({ pings_per_day: state.pings_per_day + 1 });
$("autostart").onchange = async (e) => { await invoke("set_autostart", { on: e.target.checked }); refresh(); };
$("trayTime").onchange = (e) => patch({ show_time_in_tray: e.target.checked });
$("trayUsage").onchange = (e) => patch({ show_usage_in_tray: e.target.checked });
$("lang").onclick = (e) => { const l = e.target.dataset.lang; if (l) patch({ lang: l }); };

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") view === "settings" ? show("home") : invoke("hide_panel");
});

listen("changed", refresh);
listen("panel-shown", () => {
  // First-time users land in Settings to connect their services
  // …and anyone mid-way through the Antigravity code step goes straight back to the code box
  if (state && (!state.tools.some(ready) || state.tools.some((t) => t.busy === "awaiting_code"))) show("settings");
  invoke("refresh_tools");
  invoke("refresh_usage_cmd", { force: false });
  refresh();
});
listen("show-view", (e) => show(e.payload));
setInterval(refresh, 5000);
// Tick the code countdown every second while waiting for it
setInterval(() => { if (state && state.tools.some((t) => t.busy === "awaiting_code")) refresh(); }, 1000);
refresh();

"use strict";

// ─── basics ─────────────────────────────────────────────────────────────────

const $ = (id) => document.getElementById(id);
const state = { timer: null };

function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "class") el.className = v;
    else if (k === "text") el.textContent = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, "");
    else el.setAttribute(k, v);
  }
  for (const kid of kids.flat()) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

let uid = 0;
const nextId = (p) => `${p}-${++uid}`;
const plural = (n, one, many) => `${n} ${n === 1 ? one : many || one + "s"}`;

function announce(text) {
  const el = $("announce");
  el.textContent = "";
  setTimeout(() => { el.textContent = text; }, 50);
}

function showError(text) {
  const el = $("alert");
  el.textContent = "";
  setTimeout(() => { el.textContent = text; }, 50);
}

function clearError() { $("alert").textContent = ""; }

async function api(method, path, body) {
  const opts = { method, headers: { "X-MG-Notify-Admin": "1" } };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  let resp;
  try { resp = await fetch(path, opts); } catch (_) {
    throw new Error("The page could not reach the MG Notify server.");
  }
  let data = {};
  try { data = await resp.json(); } catch (_) { /* empty */ }
  if (!resp.ok) throw new Error((typeof data.detail === "string" && data.detail) || `The server answered with an error (HTTP ${resp.status}).`);
  return data;
}

// MG Notify keeps times as ISO text; these take seconds.
const secs = (iso) => (iso ? Math.round(Date.parse(iso) / 1000) : 0);

// Times: the date and time here, and how long ago.
function when(secs) {
  if (!secs) return "Never";
  const d = new Date(secs * 1000);
  return d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

function ago(secs) {
  if (!secs) return "never";
  const s = Math.max(0, Math.round(Date.now() / 1000 - secs));
  if (s < 60) return "just now";
  if (s < 3600) return `${plural(Math.round(s / 60), "minute")} ago`;
  if (s < 86400) return `${plural(Math.round(s / 3600), "hour")} ago`;
  return `${plural(Math.round(s / 86400), "day")} ago`;
}

function duration(secs) {
  const s = Math.max(0, Math.round(Date.now() / 1000 - secs));
  if (s < 60) return plural(s, "second");
  if (s < 3600) return plural(Math.floor(s / 60), "minute");
  const hrs = Math.floor(s / 3600);
  if (hrs < 48) return `${plural(hrs, "hour")} ${plural(Math.floor((s % 3600) / 60), "minute")}`;
  return plural(Math.floor(hrs / 24), "day");
}

const PLATFORMS = { ios: "iPhone", iphone: "iPhone", ipados: "iPad", android: "Android", web: "Web browser", macos: "Mac" };
const platformText = (p) => PLATFORMS[String(p || "").toLowerCase()] || p || "Unknown";

function setView(title, current) {
  $("page-heading").textContent = title;
  document.title = title === "MG Notify admin" ? title : `${title} - MG Notify admin`;
  for (const a of $("tabs").querySelectorAll("a")) {
    if (a.getAttribute("href") === current) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
  const view = $("view");
  view.textContent = "";
  return view;
}

function field(label, input, help) {
  const id = input.id || (input.id = nextId("f"));
  const p = h("p", { class: "field" }, h("label", { for: id, text: label }), " ", input);
  if (help) {
    const hid = nextId("help");
    input.setAttribute("aria-describedby", hid);
    p.append(h("span", { id: hid, class: "help", text: help }));
  }
  return p;
}

function secret(label, help) {
  const input = h("input", { type: "password", autocomplete: "new-password", spellcheck: "false" });
  input.id = nextId("pw");
  const btn = h("button", { type: "button", "aria-pressed": "false", "aria-controls": input.id, "aria-label": `Show ${label.toLowerCase()}` }, "Show");
  btn.addEventListener("click", () => {
    const show = input.type === "password";
    input.type = show ? "text" : "password";
    btn.textContent = show ? "Hide" : "Show";
    btn.setAttribute("aria-pressed", String(show));
    btn.setAttribute("aria-label", `${show ? "Hide" : "Show"} ${label.toLowerCase()}`);
  });
  const p = field(label, input, help);
  p.append(" ", btn);
  return { el: p, input };
}

function table(caption, heads, rows) {
  return h("table", {},
    h("caption", { class: "visually-hidden", text: caption }),
    h("thead", {}, h("tr", {}, heads.map((t) => h("th", { scope: "col", text: t })))),
    h("tbody", {}, rows));
}

// Keeps focus on the same control when a list redraws.
function keepFocus(render) {
  const key = document.activeElement && document.activeElement.dataset ? document.activeElement.dataset.key : null;
  render();
  if (key) {
    const again = document.querySelector(`[data-key="${CSS.escape(key)}"]`);
    if (again) again.focus();
  }
}

// Copying: the modern clipboard where the browser offers it, else the older
// copy command (this page is often plain http).
async function copyText(text) {
  if (window.isSecureContext && navigator.clipboard) {
    try { await navigator.clipboard.writeText(text); return true; } catch (_) { /* fall back */ }
  }
  const back = document.activeElement;
  const ta = h("textarea", { readonly: true, "aria-hidden": "true", class: "offscreen" });
  ta.value = text;
  document.body.append(ta);
  ta.select();
  let ok = false;
  try { ok = document.execCommand("copy"); } catch (_) { ok = false; }
  ta.remove();
  if (back) back.focus();
  return ok;
}

async function act(path, body, then) {
  clearError();
  try {
    const r = await api("POST", path, body || {});
    if (then) await then(r);
    announce(r.message || "Done.");
    return r;
  } catch (e) {
    showError(e.message);
    return null;
  }
}

function flash(message, focusKey) {
  sessionStorage.setItem("mgn-admin-flash", message || "");
  sessionStorage.setItem("mgn-admin-focus", focusKey || "");
}


// ─── overview ───────────────────────────────────────────────────────────────

async function viewOverview() {
  const view = setView("MG Notify admin", "#/");
  const d = await api("GET", "/api/overview");
  const c = d.counts;
  const p = d.providers;
  const onOff = (v) => (v ? "on" : "off");
  view.append(
    h("p", { text: `MG Notify server ${d.version}, running for ${duration(secs(d.started))}.` }),
    h("h2", { text: "Now" }),
    h("ul", {},
      h("li", { text: `${plural(c.accounts, "account")} (${c.verified} verified), ${plural(c.sessions, "sign-in")}.` }),
      h("li", { text: `${plural(c.devices, "device")}: ${c.iphones} Apple, ${c.androids} Android, ${plural(c.browsers, "browser")}.` }),
      h("li", { text: `${plural(d.open_streams, "app")} with the live connection open.` }),
      h("li", { text: `${plural(c.api_keys, "API key")}.` }),
      h("li", { text: `${plural(c.notifications, "notification")} stored; ${c.notifications_day} in the last day, ${c.failed_day} of them not delivered.` })),
    h("h2", { text: "Delivery" }),
    h("ul", {},
      h("li", { text: `Apple push (APNs): ${onOff(p.apns)}.` }),
      h("li", { text: `Android push (Firebase): ${onOff(p.fcm)}.` }),
      h("li", { text: `Web Push: ${onOff(p.web_push)}.` }),
      h("li", { text: `Outgoing email: ${onOff(p.email)}.` }),
      h("li", { text: `Email to notifications (inbound SMTP): ${onOff(p.inbound_smtp)}.` })),
    h("h2", { text: "Settings" }),
    h("p", { class: "note", text: "From config.json; change them there (or with --configure) and restart the server. Secrets are not shown." }),
    table("Server settings", ["Setting", "Value"], d.settings.map(([k, v]) => h("tr", {},
      h("th", { scope: "row", text: k }),
      h("td", { text: v === null || v === undefined || v === "" ? "Not set" : String(v) })))));
}

// ─── accounts ───────────────────────────────────────────────────────────────

async function viewAccounts() {
  const view = setView("Accounts", "#/accounts");
  const listWrap = h("div");
  const email = h("input", { type: "email", autocomplete: "off", spellcheck: "false" });
  const pw = secret("Password", "At least 8 characters.");
  const name = h("input", { type: "text", autocomplete: "off", maxlength: 100 });
  const verified = h("input", { type: "checkbox", id: nextId("c") });
  verified.checked = true;
  const form = h("form", { novalidate: true },
    h("h2", { text: "Create an account" }),
    field("Email address", email), pw.el, field("Display name (optional)", name),
    h("p", { class: "check" }, verified, " ", h("label", { for: verified.id, text: "Mark the email address as verified (no confirmation email)" })),
    h("p", {}, h("button", { type: "submit" }, "Create the account")));
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    if (!email.value.includes("@")) { showError("Enter a valid email address."); email.focus(); return; }
    if (pw.input.value.length < 8) { showError("The password must be at least 8 characters long."); pw.input.focus(); return; }
    const r = await act("/api/accounts", { email: email.value.trim(), password: pw.input.value, verified: verified.checked, display_name: name.value.trim() || null });
    if (r) { flash(r.message, `acct:${r.account_id}`); route(); }
  });
  view.append(h("section", { "aria-labelledby": "list-h" }, h("h2", { id: "list-h", text: "All accounts" }), listWrap), form);
  const d = await api("GET", "/api/accounts");
  listWrap.append(h("p", { text: `${plural(d.accounts.length, "account")}, sorted by email.` }),
    table("Accounts, sorted by email", ["Email", "Name", "Verified", "Devices", "API keys", "Notifications", "Apps open now", "Last active", "Created"],
      d.accounts.map((a) => h("tr", {},
        h("th", { scope: "row" }, h("a", { href: `#/account/${encodeURIComponent(a.account_id)}`, "data-key": `acct:${a.account_id}` }, a.email)),
        h("td", { text: a.display_name || "None" }),
        h("td", { text: a.email_verified ? "Yes" : "No" }),
        h("td", { text: String(a.devices) }),
        h("td", { text: String(a.api_keys) }),
        h("td", { text: String(a.notifications) }),
        h("td", { text: a.open ? String(a.open) : "None" }),
        h("td", { text: a.last_active ? ago(secs(a.last_active)) : "Never" }),
        h("td", { text: when(secs(a.created_at)) })))));
}

const STATUS = { sent: "Delivered", failed: "Not delivered", stored: "Stored (no devices)" };

async function viewAccount(id) {
  let d;
  try { d = await api("GET", `/api/accounts/${encodeURIComponent(id)}`); } catch (e) {
    const view = setView("Account", "#/accounts");
    view.append(h("p", {}, e.message, " ", h("a", { href: "#/accounts" }, "Back to the accounts")));
    return;
  }
  const a = d.account;
  const view = setView(`Account ${a.email}`, "#/accounts");
  const base = `/api/accounts/${encodeURIComponent(id)}`;
  const reload = (message, key) => { flash(message, key); route(); };
  view.append(h("p", {}, h("a", { href: "#/accounts" }, "All accounts")),
    h("dl", {},
      h("dt", { text: "Email" }), h("dd", { text: `${a.email} (${a.email_verified ? "verified" : "not verified"})` }),
      h("dt", { text: "Display name" }), h("dd", { text: a.display_name || "None" }),
      h("dt", { text: "Created" }), h("dd", { text: d.summary ? when(secs(d.summary.created_at)) : "Unknown" }),
      h("dt", { text: "Unread notifications" }), h("dd", { text: String(d.unread) }),
      h("dt", { text: "Apps open now" }), h("dd", { text: String(d.streams.length) })));

  // Test send.
  const tTitle = h("input", { type: "text", value: "Test", maxlength: 200 });
  const tMsg = h("input", { type: "text", value: "A test notification from the admin page.", maxlength: 4000 });
  const tOut = h("div", { class: "result" });
  const tForm = h("form", { novalidate: true }, h("h2", { text: "Send a test notification" }),
    field("Title", tTitle), field("Message", tMsg),
    h("p", {}, h("button", { type: "submit" }, "Send to every device of this account")), tOut);
  tForm.addEventListener("submit", async (e) => {
    e.preventDefault();
    const r = await act(`${base}/test`, { title: tTitle.value, message: tMsg.value });
    if (!r) return;
    tOut.textContent = "";
    tOut.append(h("p", { text: r.message }));
    const devs = r.result.devices || [];
    if (devs.length) {
      tOut.append(h("ul", {}, devs.map((x) => h("li", { class: x.status === "sent" ? "" : "bad",
        text: `${x.device_name}: ${x.status === "sent" ? "delivered" : x.status === "removed" ? `removed (${x.error})` : `not delivered (${x.error})`}` }))));
    }
  });
  view.append(tForm);

  // Devices.
  view.append(h("section", { "aria-labelledby": "dev-h" },
    h("h2", { id: "dev-h", tabindex: "-1", "data-key": "dev-h", text: `Devices (${d.devices.length})` }),
    d.devices.length ? table("Devices", ["Name", "System", "Push through", "Sound", "Last seen", "From", "App open now", "Actions"], d.devices.map((x) => h("tr", {},
      h("th", { scope: "row", text: x.name }),
      h("td", { text: platformText(x.platform) }),
      h("td", { text: x.push }),
      h("td", { text: x.sound || "default" }),
      h("td", { text: x.last_seen ? ago(secs(x.last_seen)) : "Unknown" }),
      h("td", { text: x.last_ip_address || "Unknown" }),
      h("td", { text: x.live ? "Yes" : "No" }),
      h("td", {}, h("button", { type: "button", class: "danger", "aria-label": `Sign out ${x.name}`, "data-key": `dev:${x.device_id}`, onclick: () => {
        if (!confirm(`Sign out ${x.name}? It gets no more notifications until it signs in again.`)) return;
        act(`${base}/devices/${encodeURIComponent(x.device_id)}/sign-out`, {}, (r) => reload(r.message, "dev-h"));
      } }, "Sign out")))))
      : h("p", { text: "No devices. Notifications are stored but not pushed anywhere." })));

  // Sign-ins.
  const devName = Object.fromEntries(d.devices.map((x) => [x.device_id, x.name]));
  view.append(h("section", { "aria-labelledby": "ses-h" },
    h("h2", { id: "ses-h", tabindex: "-1", "data-key": "ses-h", text: `Sign-ins (${d.sessions.length})` }),
    d.sessions.length ? table("Sign-ins, most recently used first", ["Device", "Signed in", "Last used", "Actions"], d.sessions.map((x) => h("tr", {},
      h("th", { scope: "row", text: x.device_id ? (devName[x.device_id] || "A removed device") : "Website or app without a device" }),
      h("td", { text: when(secs(x.created_at)) }),
      h("td", { text: ago(secs(x.last_used_at)) }),
      h("td", {}, h("button", { type: "button", "data-key": `ses:${x.id}`, "aria-label": `End the sign-in from ${when(secs(x.created_at))}`, onclick: () => {
        if (!confirm("End this sign-in?")) return;
        act(`${base}/sessions/${x.id}/delete`, {}, (r) => reload(r.message, "ses-h"));
      } }, "End")))))
      : h("p", { text: "Not signed in anywhere." })));

  // API keys.
  const keyLabel = h("input", { type: "text", maxlength: 100 });
  const keyOut = h("div", { class: "result" });
  const keyForm = h("form", { novalidate: true }, field("Label for a new key", keyLabel), h("p", {}, h("button", { type: "submit" }, "Make an API key")), keyOut);
  keyForm.addEventListener("submit", async (e) => {
    e.preventDefault();
    if (!keyLabel.value.trim()) { showError("Give the key a label."); keyLabel.focus(); return; }
    const r = await act(`${base}/keys`, { label: keyLabel.value.trim() });
    if (r) {
      const copied = await copyText(r.api_key);
      flash(`${r.message} ${copied ? "The key is copied to the clipboard." : "Use Show to copy it."}`, `key:show:${r.key_id}`);
      route();
    }
  });
  view.append(h("section", { "aria-labelledby": "key-h" },
    h("h2", { id: "key-h", tabindex: "-1", "data-key": "key-h", text: `API keys (${d.api_keys.length})` }),
    d.api_keys.length ? table("API keys", ["Label", "Key", "Made", "Last used", "Actions"], d.api_keys.map((k) => {
      const cell = h("td", { text: k.start });
      return h("tr", {},
        h("th", { scope: "row", text: k.label }), cell,
        h("td", { text: when(secs(k.created_at)) }),
        h("td", { text: k.last_used_at ? ago(secs(k.last_used_at)) : "Never" }),
        h("td", {},
          h("button", { type: "button", "aria-label": `Show and copy the key ${k.label}`, "data-key": `key:show:${k.key_id}`, onclick: async () => {
            clearError();
            try {
              const r = await api("GET", `${base}/keys/${encodeURIComponent(k.key_id)}`);
              cell.textContent = r.api_key;
              const copied = await copyText(r.api_key);
              announce(`The key ${k.label} is ${r.api_key}. ${copied ? "Copied to the clipboard." : ""}`);
            } catch (e2) { showError(e2.message); }
          } }, "Show and copy"),
          h("button", { type: "button", class: "danger", "aria-label": `Revoke the key ${k.label}`, "data-key": `key:del:${k.key_id}`, onclick: () => {
            if (!confirm(`Revoke the key ${k.label}? Anything using it stops sending.`)) return;
            act(`${base}/keys/${encodeURIComponent(k.key_id)}/delete`, {}, (r) => reload(r.message, "key-h"));
          } }, "Revoke")));
    })) : h("p", { text: "No API keys." }),
    keyForm));

  // Notifications.
  view.append(h("section", { "aria-labelledby": "not-h" },
    h("h2", { id: "not-h", tabindex: "-1", "data-key": "not-h", text: `Notifications (${d.notifications_total})` }),
    d.notifications_total > d.notifications.length ? h("p", { class: "note", text: `The newest ${d.notifications.length} are shown.` }) : null,
    d.notifications.length ? [
      table("Notifications, newest first", ["Time", "Title", "Message", "Delivery", "Seen", "Actions"], d.notifications.map((n) => h("tr", {},
        h("th", { scope: "row", text: when(secs(n.created_at)) }),
        h("td", { text: n.title }),
        h("td", { text: n.message }),
        h("td", { class: n.status === "failed" ? "bad" : "", text: `${STATUS[n.status] || n.status}${n.email ? " (from email)" : ""}` }),
        h("td", { text: n.seen ? "Yes" : "No" }),
        h("td", {}, h("button", { type: "button", "aria-label": `Delete the notification ${n.title} from ${when(secs(n.created_at))}`, "data-key": `not:${n.notification_id}`, onclick: () => {
          act(`${base}/notifications/${encodeURIComponent(n.notification_id)}/delete`, {}, (r) => reload(r.message, "not-h"));
        } }, "Delete"))))),
      h("p", {}, h("button", { type: "button", class: "danger", onclick: () => {
        if (!confirm(`Delete all ${d.notifications_total} notifications of ${a.email}?`)) return;
        act(`${base}/notifications/clear`, {}, (r) => reload(r.message, "not-h"));
      } }, "Delete all notifications")),
    ] : h("p", { text: "No notifications." })));

  // Account details.
  const em = h("input", { type: "email", value: a.email, autocomplete: "off", spellcheck: "false" });
  const emVerified = h("input", { type: "checkbox", id: nextId("c") });
  emVerified.checked = true;
  const emForm = h("form", { novalidate: true }, h("h2", { text: "Email address" }), field("Email address", em),
    h("p", { class: "check" }, emVerified, " ", h("label", { for: emVerified.id, text: "Mark the new address as verified" })),
    h("p", {}, h("button", { type: "submit" }, "Save the email address"),
      a.email_verified ? null : [" ", h("button", { type: "button", onclick: () => act(`${base}/verify`, {}, (r) => reload(r.message, "")) }, "Mark the current address as verified")]));
  emForm.addEventListener("submit", (e) => { e.preventDefault(); act(`${base}/email`, { email: em.value.trim(), verified: emVerified.checked }, (r) => reload(r.message, "")); });
  const nm = h("input", { type: "text", value: a.display_name || "", maxlength: 100 });
  const nmForm = h("form", { novalidate: true }, h("h2", { text: "Display name" }), field("Display name", nm, "Leave empty and save to remove it."), h("p", {}, h("button", { type: "submit" }, "Save the display name")));
  nmForm.addEventListener("submit", (e) => { e.preventDefault(); act(`${base}/name`, { display_name: nm.value.trim() || null }, (r) => reload(r.message, "")); });
  const pw = secret("New password", "At least 8 characters. Signs the account out everywhere and removes its devices, as a password change does.");
  const pwForm = h("form", { novalidate: true }, h("h2", { text: "Password" }), pw.el, h("p", {}, h("button", { type: "submit" }, "Set the password")));
  pwForm.addEventListener("submit", (e) => {
    e.preventDefault();
    if (pw.input.value.length < 8) { showError("The password must be at least 8 characters long."); pw.input.focus(); return; }
    if (!confirm(`Set a new password for ${a.email}? It is signed out everywhere and its devices are removed.`)) return;
    act(`${base}/password`, { password: pw.input.value }, (r) => reload(r.message, ""));
  });
  view.append(emForm, nmForm, pwForm,
    h("section", { "aria-labelledby": "danger-h" }, h("h2", { id: "danger-h", text: "Sign out and delete" }),
      h("p", {}, h("button", { type: "button", class: "danger", onclick: () => {
        if (!confirm(`Sign ${a.email} out everywhere? Its devices are removed and stop getting notifications until they sign in again.`)) return;
        act(`${base}/sign-out-everywhere`, {}, (r) => reload(r.message, "dev-h"));
      } }, "Sign out everywhere")),
      h("p", {}, h("button", { type: "button", class: "danger", onclick: async () => {
        if (!confirm(`Delete the account ${a.email} with its devices, API keys and notifications?`)) return;
        if (!confirm(`Really delete ${a.email}? This can't be undone.`)) return;
        const r = await act(`${base}/delete`, {});
        if (r) { flash(r.message, ""); location.hash = "#/accounts"; }
      } }, `Delete ${a.email}`))));
}

// ─── open apps ──────────────────────────────────────────────────────────────

async function viewLive() {
  const view = setView("Open apps", "#/live");
  const wrap = h("div");
  view.append(h("p", { class: "note", text: "Apps and websites with the live connection open (they get changes at once). Updates every 5 seconds." }), wrap);
  let last = "";
  const render = (d) => {
    wrap.textContent = "";
    wrap.append(h("p", { text: `${plural(d.streams.length, "open app")}.` }),
      d.streams.length ? table("Open apps, oldest first", ["Account", "Device", "From", "App", "Open for"], d.streams.map((x) => h("tr", {},
        h("th", { scope: "row" }, h("a", { href: `#/account/${encodeURIComponent(x.stream.account_id)}` }, x.email || "A deleted account")),
        h("td", { text: x.device_name || (x.stream.device_id ? "A removed device" : "Website") }),
        h("td", { text: x.stream.address || "Unknown" }),
        h("td", { text: x.stream.user_agent || "Unknown" }),
        h("td", { text: duration(secs(x.stream.since)) })))) : null);
  };
  const refresh = async () => {
    try {
      const d = await api("GET", "/api/live");
      const json = JSON.stringify(d.streams.map((x) => [x.stream.account_id, x.stream.device_id, x.stream.since]));
      if (json !== last) { last = json; keepFocus(() => render(d)); }
    } catch (e) { showError(e.message); }
  };
  await refresh();
  state.timer = setInterval(refresh, 5000);
}

// ─── log ────────────────────────────────────────────────────────────────────

async function viewLog() {
  const view = setView("Log", "#/log");
  const lines = h("select", {}, [100, 300, 1000, 2000].map((n) => new Option(`Last ${n} lines`, n)));
  lines.value = "300";
  const filter = h("input", { type: "search", spellcheck: "false" });
  const adminOnly = h("input", { type: "checkbox", id: nextId("c") });
  const out = h("div");
  let all = [];
  const show = () => {
    const f = filter.value.trim().toLowerCase();
    const shown = all.filter((l) => (!adminOnly.checked || l.includes(" admin: ")) && (!f || l.toLowerCase().includes(f)));
    out.textContent = "";
    const id = nextId("logl");
    out.append(h("p", { id, text: `${plural(shown.length, "line")}, newest last.` }),
      shown.length ? h("pre", { tabindex: "0", "aria-labelledby": id, text: shown.join("\n") }) : null);
    return shown.length;
  };
  const load = async (say) => {
    clearError();
    try {
      all = (await api("GET", `/api/log?lines=${lines.value}`)).lines;
      const n = show();
      if (say) announce(`${plural(n, "line")} shown.`);
    } catch (e) { showError(e.message); }
  };
  filter.addEventListener("input", show);
  adminOnly.addEventListener("change", () => announce(`${plural(show(), "line")} shown.`));
  view.append(h("p", { class: "note", text: "The server's recent log lines, kept in memory (the last 2000). The full log is wherever the server's output goes (its service log)." }),
    field("Show", lines), field("Only lines containing", filter),
    h("p", { class: "check" }, adminOnly, " ", h("label", { for: adminOnly.id, text: "Only admin actions" })),
    h("p", {}, h("button", { type: "button", onclick: () => load(true) }, "Refresh")), out);
  lines.addEventListener("change", () => load(true));
  await load(false);
}

// ─── routing ────────────────────────────────────────────────────────────────

async function route() {
  clearInterval(state.timer);
  const parts = (location.hash || "#/").slice(2).split("/").map(decodeURIComponent);
  const flashMsg = sessionStorage.getItem("mgn-admin-flash");
  const focusKey = sessionStorage.getItem("mgn-admin-focus");
  sessionStorage.removeItem("mgn-admin-flash");
  sessionStorage.removeItem("mgn-admin-focus");
  if (flashMsg === null) clearError();
  try {
    if (parts[0] === "accounts") await viewAccounts();
    else if (parts[0] === "account" && parts[1]) await viewAccount(parts[1]);
    else if (parts[0] === "live") await viewLive();
    else if (parts[0] === "log") await viewLog();
    else await viewOverview();
  } catch (e) { showError(e.message); }
  if (flashMsg) announce(flashMsg);
  const target = focusKey && document.querySelector(`[data-key="${CSS.escape(focusKey)}"]`);
  (target || $("page-heading")).focus();
}

window.addEventListener("hashchange", route);
route();

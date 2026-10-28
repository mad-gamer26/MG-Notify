// The dashboard: notifications, sending, browser notifications, API keys,
// devices and account settings.
"use strict";

const state = {
    account: null,
    notifications: [],
    knownIds: null,
    /** Unread when this page saw them: marked "New" until the page is reloaded. */
    newIds: new Set(),
    /** Emails opened on this page: seen, even if a reload started before that says otherwise. */
    openedEmailIds: new Set(),
    /** An email whose browser notification was clicked, opened once it's loaded. */
    emailToOpen: new URLSearchParams(location.search).get("open"),
    devices: [],
    keys: [],
    view: null,
};

const $ = (id) => document.getElementById(id);
const DEVICE_ID_KEY = "mgn.webDeviceId";

function el(tag, attributes = {}, ...children) {
    const node = document.createElement(tag);
    for (const [name, value] of Object.entries(attributes)) {
        if (name === "class") node.className = value;
        else if (name === "text") node.textContent = value;
        else if (name.startsWith("on")) node.addEventListener(name.slice(2), value);
        else node.setAttribute(name, value);
    }
    node.append(...children.filter((child) => child !== null && child !== undefined));
    return node;
}

function button(label, onClick, { kind = "secondary", ariaLabel } = {}) {
    const node = el("button", { type: "button", class: "button small " + kind, onclick: onClick }, label);
    if (ariaLabel) node.setAttribute("aria-label", ariaLabel);
    return node;
}

/** Calls the API; a dead session goes back to sign-in, unverified email to the verify screen. */
async function call(method, path, body) {
    try {
        return await MG.api(method, path, body);
    } catch (error) {
        if (error.status === 401 && path !== "/account/change-email" && path !== "/account/change-password" && path !== "/account/delete") {
            signedOut();
        } else if (error.status === 403 && error.message === "Email verification is required") {
            showVerifyRequired();
        }
        throw error;
    }
}

function signedOut(reason = "signedout") {
    MG.clearSession();
    location.replace("/signin.html?" + reason + "=1");
}

function showOnly(id) {
    for (const section of ["loading", "unreachable", "verify-required", "dashboard"]) {
        $(section).hidden = section !== id;
    }
}

async function start() {
    if (!MG.token()) {
        location.replace("/signin.html");
        return;
    }
    let me;
    try {
        me = await call("GET", "/auth/me");
    } catch (error) {
        if (error.status !== 401) {
            showOnly("unreachable");
            $("unreachable-title").focus();
        }
        return;
    }
    setAccount(me.account);
    state.stream = MG.openEventStream(handleEvent);
    if ("serviceWorker" in navigator) {
        // A push arriving while this page is in front: the service worker
        // hands it here instead of showing it. Usually the event stream has
        // already brought it, so it's only fetched if it's new.
        navigator.serviceWorker.addEventListener("message", (event) => {
            const id = event.data && event.data.notification_id;
            if (event.data && event.data.type === "mg-notify-open-email" && id) {
                state.emailToOpen = id;
                if (!openRequestedEmail()) loadNotifications(true);
                return;
            }
            if (event.data && event.data.type === "mg-notify-push" && id && state.knownIds && !state.knownIds.has(id)) {
                loadNotifications(true);
            }
        });
    }
}

/** Shows the account's name everywhere, and the screen its verification calls for. */
function setAccount(account) {
    const wasVerified = state.account && state.account.email_verified;
    state.account = account;
    for (const node of document.querySelectorAll(".account-email")) node.textContent = account.email;
    for (const node of document.querySelectorAll(".account-name")) node.textContent = account.display_name || account.email;
    const nameInput = $("display-name");
    if (nameInput && document.activeElement !== nameInput) nameInput.value = account.display_name || "";
    if (!account.email_verified) {
        if ($("verify-required").hidden) showVerifyRequired();
        return;
    }
    if ($("dashboard").hidden) {
        showOnly("dashboard");
        route(wasVerified === false);
        loadNotifications(false);
        if (wasVerified === false) MG.announce("Your email is verified.");
    }
}

/** Everything the server tells this page as it happens (see MG.openEventStream). */
function handleEvent(name, data) {
    switch (name) {
        case "ready": {
            // Also sent after every reconnect: catch up on anything missed.
            const wasShowing = !$("dashboard").hidden;
            MG.show($("notifications-message"), "");
            setAccount(data.account);
            if (wasShowing && state.account.email_verified) {
                loadNotifications(true);
                refreshCurrentView();
            }
            break;
        }
        case "notification":
            receiveNotification(data.notification);
            break;
        case "notification_deleted":
            removeNotification(data.notification_id);
            break;
        case "notifications_seen":
            for (const n of state.notifications) {
                if (data.notification_ids.includes(n.notification_id)) n.seen = true;
            }
            break;
        case "notifications_cleared":
            state.notifications = [];
            state.knownIds = new Set();
            renderNotifications();
            break;
        case "devices_updated":
            if (state.view === "devices") loadDevices();
            if (state.view === "browser") refreshBrowserState();
            break;
        case "api_keys_updated":
            if (state.view === "keys") loadKeys();
            break;
        case "account_updated":
            setAccount(data.account);
            break;
        case "resync":
            loadNotifications(true);
            refreshCurrentView();
            break;
        case "signed_out":
        case "unauthorized":
            signedOut("signedout");
            break;
        case "account_deleted":
            signedOut("deleted");
            break;
        case "disconnected":
            if (data.failures === 1 && !$("dashboard").hidden) {
                MG.show($("notifications-message"), "Lost the connection to MG Notify. Reconnecting…", "info");
            }
            break;
    }
}

function refreshCurrentView() {
    if (state.view === "keys") loadKeys();
    if (state.view === "devices") loadDevices();
    if (state.view === "browser") refreshBrowserState();
}

/** A notification that just arrived: list it, and announce it while the page is in view. */
function receiveNotification(notification) {
    if (!state.knownIds || state.knownIds.has(notification.notification_id)) return;
    state.knownIds.add(notification.notification_id);
    if (notification.seen === false) state.newIds.add(notification.notification_id);
    state.notifications = [notification, ...state.notifications];
    renderNotifications();
    announceNotification(notification);
}

function announceNotification(notification) {
    if (document.visibilityState !== "visible") return;
    const body = notification.email ? "email" : notification.message;
    const text = `${displayTitle(notification)}, ${body}`;
    MG.announce("New notification: " + (text.length > 400 ? text.slice(0, 400) + "…" : text));
    MG.playNotificationSound();
}

function removeNotification(id) {
    if (!state.knownIds || !state.knownIds.has(id)) return;
    state.knownIds.delete(id);
    state.notifications = state.notifications.filter((n) => n.notification_id !== id);
    renderNotifications();
}

function showVerifyRequired() {
    showOnly("verify-required");
    document.title = "Verify your email – MG Notify";
    $("verify-required-title").focus();
}

// Views

const VIEW_TITLES = {
    notifications: "Notifications",
    send: "Send a notification",
    browser: "Browser notifications",
    keys: "API keys",
    devices: "Devices",
    account: "Account",
};

function route(moveFocus = true) {
    const requested = location.hash.slice(1);
    const view = VIEW_TITLES[requested] ? requested : "notifications";
    state.view = view;
    for (const section of document.querySelectorAll("[data-view]")) section.hidden = section.dataset.view !== view;
    for (const link of document.querySelectorAll("[data-view-link]")) {
        if (link.dataset.viewLink === view) link.setAttribute("aria-current", "page");
        else link.removeAttribute("aria-current");
    }
    document.title = VIEW_TITLES[view] + " – MG Notify";
    if (moveFocus) document.querySelector(`[data-view="${view}"] h1`).focus();
    if (view === "keys") loadKeys();
    if (view === "devices") loadDevices();
    if (view === "browser") refreshBrowserState();
    if (view === "notifications") scheduleMarkSeen();
}

window.addEventListener("hashchange", () => {
    if (!$("dashboard").hidden) route(true);
});

// Notifications

function emailParts(notification) {
    const title = notification.title;
    const start = title.indexOf("<");
    const end = start >= 0 ? title.indexOf(">", start) : -1;
    const tidy = (text) => text.replace(/ {2}/g, " ").replace(/ :/g, ":").trim();
    const address = end > start ? title.slice(start + 1, end).trim() : "";
    const name = end > start ? tidy(title.slice(0, start)) || address : tidy(title.split(":")[0]);
    const colon = title.indexOf(":");
    const subject = colon >= 0 ? title.slice(colon + 1).trim() || title : title;
    const display = end > start ? tidy(title.slice(0, start) + title.slice(end + 1)) : tidy(title);
    return { name, address, subject, display };
}

function displayTitle(notification) {
    return notification.email ? emailParts(notification).display : notification.title;
}

async function loadNotifications(quietly) {
    let result;
    try {
        result = await call("GET", "/notifications");
    } catch (error) {
        if (!quietly) MG.show($("notifications-message"), error.message);
        return;
    }
    MG.show($("notifications-message"), "");
    const items = result.notifications;
    const fresh = state.knownIds && quietly ? items.filter((n) => !state.knownIds.has(n.notification_id)) : [];
    state.knownIds = new Set(items.map((n) => n.notification_id));
    for (const n of items) if (state.openedEmailIds.has(n.notification_id)) n.seen = true;
    for (const n of items) if (n.seen === false) state.newIds.add(n.notification_id);
    state.notifications = items;
    renderNotifications();
    openRequestedEmail();
    // Ones that arrived while the connection was down.
    if (fresh.length === 1) announceNotification(fresh[0]);
    else if (fresh.length > 1 && document.visibilityState === "visible") {
        MG.announce(fresh.length + " new notifications");
        MG.playNotificationSound();
    }
}

function renderNotifications() {
    const query = $("search").value.trim().toLowerCase();
    const all = state.notifications;
    const matches = query
        ? all.filter((n) =>
              [n.title, n.message, MG.formatDate(n.created_at)].some((text) => text.toLowerCase().includes(query)))
        : all;
    const count = $("notification-count");
    count.textContent = all.length ? String(all.length) : "";
    $("clear-all").hidden = all.length === 0;
    $("search").closest(".field").hidden = all.length === 0;
    const noun = (n) => (n === 1 ? "notification" : "notifications");
    $("search-summary").textContent = all.length === 0
        ? ""
        : query
          ? `${matches.length} of ${all.length} ${noun(all.length)} match.`
          : `You currently have ${all.length} ${noun(all.length)}.`;
    $("notifications-empty").hidden = all.length !== 0;

    const list = $("notification-list");
    const focusedId = document.activeElement && document.activeElement.closest("[data-id]")?.dataset.id;
    list.textContent = "";
    if (all.length && !matches.length) {
        list.append(el("li", { class: "muted" }, "No matching notifications."));
    }
    for (const notification of matches) list.append(notificationItem(notification));
    if (focusedId) {
        const again = list.querySelector(`[data-id="${CSS.escape(focusedId)}"] h2`);
        if (again) again.focus();
    }
    scheduleMarkSeen();
}

let markSeenTimer = null;

/**
 * Looking at the list counts as seeing it: once it has been in view for a
 * moment, unread notifications are marked seen everywhere (which also
 * updates iPhone badges). The "New" markers stay until the page reloads.
 */
function scheduleMarkSeen() {
    clearTimeout(markSeenTimer);
    markSeenTimer = setTimeout(async () => {
        const inView = document.visibilityState === "visible" && state.view === "notifications" && !$("dashboard").hidden &&
            !$("email-viewer").open;
        if (!inView || !state.notifications.some((n) => n.seen === false)) return;
        try {
            await call("POST", "/notifications/mark-seen", {});
            for (const n of state.notifications) n.seen = true;
        } catch {}
    }, 1500);
}

document.addEventListener("visibilitychange", scheduleMarkSeen);

function notificationItem(notification) {
    const id = "n-" + notification.notification_id;
    const title = displayTitle(notification);
    const heading = el("h2", { class: "item-title", id, tabindex: "-1" }, title,
        state.newIds.has(notification.notification_id) ? el("span", { class: "badge new", text: "New" }) : null);
    const date = el("time", { datetime: notification.created_at, text: MG.formatDate(notification.created_at) });
    const meta = el("p", { class: "item-meta" }, date, notification.email ? el("span", { class: "badge", text: "Email" }) : null);
    let body = null;
    if (!notification.email) {
        body = el("p", { class: "item-body" });
        MG.renderMessage(body, notification.message);
    }
    const actions = el("div", { class: "actions" });
    if (notification.email) {
        actions.append(button("Open email", () => openEmail(notification), { kind: "", ariaLabel: "Open email: " + title }));
    } else {
        actions.append(button("Copy", () => copyNotification(notification), { ariaLabel: "Copy: " + title }));
    }
    actions.append(button("Delete", (event) => deleteNotification(notification, event.currentTarget), { kind: "danger", ariaLabel: "Delete: " + title }));
    return el("li", { "data-id": notification.notification_id }, el("article", { "aria-labelledby": id }, heading, body, meta, actions));
}

async function copyText(text, what) {
    try {
        await navigator.clipboard.writeText(text);
        MG.announce(what + " copied.");
    } catch {
        MG.announce("Couldn't copy. Select the text and copy it instead.", { assertive: true });
    }
}

function copyNotification(notification) {
    copyText([notification.title, notification.message, MG.formatDate(notification.created_at)].join("\n"), "Notification");
}

async function deleteNotification(notification, trigger) {
    await MG.busy(trigger, async () => {
        try {
            await call("DELETE", "/notifications/" + encodeURIComponent(notification.notification_id));
        } catch (error) {
            MG.show($("notifications-message"), error.message);
            return;
        }
        const item = trigger.closest("li");
        const next = item.nextElementSibling || item.previousElementSibling;
        state.notifications = state.notifications.filter((n) => n.notification_id !== notification.notification_id);
        state.knownIds.delete(notification.notification_id);
        renderNotifications();
        MG.announce("Notification deleted.");
        const target = next && $("notification-list").querySelector(`[data-id="${CSS.escape(next.dataset.id || "")}"] h2`);
        (target || $("notifications-title")).focus();
    });
}

/** The email from a clicked browser notification, if it's loaded: shown, and true. */
function openRequestedEmail() {
    const id = state.emailToOpen;
    const notification = id && state.notifications.find((n) => n.notification_id === id);
    if (!notification) return false;
    state.emailToOpen = null;
    if (new URLSearchParams(location.search).has("open")) history.replaceState(null, "", "/app.html#notifications");
    if (state.view !== "notifications") {
        location.hash = "notifications";
        route(false);
    }
    if ($("email-viewer").open) $("email-viewer").close();
    openEmail(notification);
    return true;
}

/** Opening an email counts as seeing it: here, everywhere else, and its "New" marker goes. */
function markEmailSeen(notification) {
    const id = notification.notification_id;
    state.openedEmailIds.add(id);
    state.newIds.delete(id);
    document.querySelector(`[data-id="${CSS.escape(id)}"] .badge.new`)?.remove();
    if (notification.seen !== false) return;
    notification.seen = true;
    call("POST", "/notifications/mark-seen", { notification_ids: [id] }).catch(() => {});
}

function openEmail(notification) {
    markEmailSeen(notification);
    const parts = emailParts(notification);
    $("email-subject").textContent = parts.subject;
    $("email-from").textContent = parts.address ? `${parts.name} <${parts.address}>` : parts.name;
    $("email-date").textContent = MG.formatDate(notification.created_at);
    const html = (notification.html || notification.message || "").trim();
    const full = /<html|<!doctype html/i.test(html);
    // The email is shown in a sandbox: no scripts, links open in a new tab.
    $("email-frame").srcdoc = full
        ? html
        : `<!doctype html><html><head><meta charset="utf-8"><base target="_blank">` +
          `<link rel="stylesheet" href="${location.origin}/assets/email.css"></head><body>${html}</body></html>`;
    const dialog = $("email-viewer");
    dialog.showModal();
    $("email-subject").setAttribute("tabindex", "-1");
    $("email-subject").focus();
}

// Sending

async function sendNotification(event) {
    event.preventDefault();
    const message = $("send-message");
    const title = $("send-title-input");
    const body = $("send-body");
    if (!title.value.trim() || !body.value.trim()) {
        MG.show(message, "Enter a title and a message.");
        (title.value.trim() ? body : title).focus();
        return;
    }
    await MG.busy(event.submitter || $("send-form").querySelector("button"), async () => {
        try {
            const result = await call("POST", "/account/notify", { title: title.value.trim(), message: body.value });
            const delivered = result.delivery.delivered;
            MG.show(message, delivered === 1 ? "Sent to 1 device." : `Sent to ${delivered} devices.`, "success");
            body.value = "";
            loadNotifications(true);
        } catch (error) {
            MG.show(message, error.message);
        }
    });
}

// Browser notifications

function browserDeviceId() {
    let id = localStorage.getItem(DEVICE_ID_KEY);
    if (!id) {
        id = "web-" + (crypto.randomUUID ? crypto.randomUUID() : String(Date.now()) + Math.random().toString(16).slice(2));
        localStorage.setItem(DEVICE_ID_KEY, id);
    }
    return id;
}

function browserName() {
    const ua = navigator.userAgent;
    const browser = /Edg\//.test(ua) ? "Edge" : /Firefox\//.test(ua) ? "Firefox" : /OPR\//.test(ua) ? "Opera"
        : /Chrome\//.test(ua) ? "Chrome" : /Safari\//.test(ua) ? "Safari" : "Browser";
    const system = /Windows/.test(ua) ? "Windows" : /Android/.test(ua) ? "Android" : /iPhone|iPad/.test(ua) ? "iOS"
        : /Mac OS X/.test(ua) ? "macOS" : /CrOS/.test(ua) ? "ChromeOS" : /Linux/.test(ua) ? "Linux" : "";
    return system ? `${browser} on ${system}` : browser;
}

const pushSupported = () => "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;

async function currentSubscription() {
    const registration = await navigator.serviceWorker.register("/sw.js");
    await navigator.serviceWorker.ready;
    return { registration, subscription: await registration.pushManager.getSubscription() };
}

async function refreshBrowserState() {
    const stateText = $("browser-state");
    const show = (enable, disable) => {
        $("browser-enable").hidden = !enable;
        $("browser-disable").hidden = !disable;
        $("browser-test").hidden = !disable;
    };
    if (!pushSupported()) {
        stateText.textContent = /iPhone|iPad/.test(navigator.userAgent)
            ? "To get notifications on iPhone or iPad, add this site to your Home Screen (Share, then Add to Home Screen), open it from there and sign in. Or use the MG Notify app."
            : "This browser can't receive push notifications.";
        show(false, false);
        return;
    }
    if (Notification.permission === "denied") {
        stateText.textContent = "Notifications are blocked for this site. Allow them in your browser's site settings, then come back here.";
        show(false, false);
        return;
    }
    const { subscription } = await currentSubscription();
    let devices = [];
    try {
        devices = (await call("GET", "/devices")).devices;
    } catch {}
    const registered = devices.some((d) => d.device_id === browserDeviceId());
    if (subscription && registered && Notification.permission === "granted") {
        stateText.textContent = "Browser notifications are on for this browser.";
        show(false, true);
    } else {
        stateText.textContent = "Browser notifications are off for this browser.";
        show(true, false);
    }
}

/** Waits up to `seconds` for the notification permission to stop being "default". */
async function permissionDecision(seconds) {
    for (let i = 0; i < seconds * 2; i++) {
        if (Notification.permission !== "default") return Notification.permission;
        await new Promise((resolve) => setTimeout(resolve, 500));
    }
    return Notification.permission;
}

function base64UrlToBytes(text) {
    const padded = (text + "===".slice((text.length + 3) % 4)).replace(/-/g, "+").replace(/_/g, "/");
    return Uint8Array.from(atob(padded), (c) => c.charCodeAt(0));
}

async function enableBrowserNotifications(event) {
    const message = $("browser-message");
    await MG.busy(event.currentTarget, async () => {
        try {
            let permission = await Notification.requestPermission();
            if (permission === "default") {
                // Some browsers show a quiet prompt and answer before the person
                // has chosen; carry on as soon as they allow it.
                MG.show(message, "Allow notifications when your browser asks (look near the address bar).", "info");
                permission = await permissionDecision(60);
            }
            if (permission !== "granted") {
                MG.show(message, permission === "denied"
                    ? "Notifications are blocked for this site. Allow them in your browser's site settings (the icon next to the address), then choose Turn on again."
                    : "Notifications weren't allowed yet. Choose Turn on again when you're ready.");
                return;
            }
            const { public_key: publicKey } = await call("GET", "/web-push/public-key");
            const { registration } = await currentSubscription();
            let subscription = await registration.pushManager.getSubscription();
            if (subscription) await subscription.unsubscribe();
            subscription = await registration.pushManager.subscribe({
                userVisibleOnly: true,
                applicationServerKey: base64UrlToBytes(publicKey),
            });
            await call("POST", "/devices/register", {
                device_id: browserDeviceId(),
                device_name: browserName(),
                platform: "web",
                web_push: subscription.toJSON(),
            });
            MG.show(message, "Browser notifications are on.", "success");
        } catch (error) {
            MG.show(message, error.message || "Browser notifications couldn't be turned on.");
        }
        await refreshBrowserState();
        $("browser-title").focus();
    });
}

async function disableBrowserNotifications(event) {
    const message = $("browser-message");
    await MG.busy(event.currentTarget, async () => {
        try {
            await call("DELETE", "/devices/" + encodeURIComponent(browserDeviceId())).catch((error) => {
                if (error.status !== 404) throw error;
            });
            const { subscription } = await currentSubscription();
            if (subscription) await subscription.unsubscribe();
            MG.show(message, "Browser notifications are off for this browser.", "success");
        } catch (error) {
            MG.show(message, error.message);
        }
        await refreshBrowserState();
        $("browser-title").focus();
    });
}

async function sendTest(event) {
    await MG.busy(event.currentTarget, async () => {
        try {
            await call("POST", "/account/notify", { title: "MG Notify", message: "Browser notifications are working." });
            MG.show($("browser-message"), "Test notification sent.", "success");
            loadNotifications(true);
        } catch (error) {
            MG.show($("browser-message"), error.message);
        }
    });
}

// API keys

async function loadKeys() {
    try {
        state.keys = (await call("GET", "/api-keys")).api_keys;
        renderKeys();
    } catch (error) {
        MG.show($("keys-message"), error.message);
    }
}

function renderKeys() {
    const list = $("key-list");
    list.textContent = "";
    $("keys-empty").hidden = state.keys.length !== 0;
    $("delete-all-keys").hidden = state.keys.length === 0;
    for (const key of state.keys) {
        const id = "k-" + key.key_id;
        const used = key.last_used_at ? "Last used " + MG.formatDate(key.last_used_at) : "Never used";
        const created = key.created_at ? "Created " + MG.formatDate(key.created_at) : "";
        list.append(
            el("li", { "data-id": key.key_id },
                el("article", { "aria-labelledby": id },
                    el("h3", { class: "item-title", id, tabindex: "-1", text: key.label }),
                    el("code", { class: "key-value", text: key.api_key }),
                    el("p", { class: "item-meta", text: [created, used].filter(Boolean).join(". ") + "." }),
                    el("div", { class: "actions" },
                        button("Copy key", () => copyText(key.api_key, "API key"), { ariaLabel: "Copy key: " + key.label }),
                        button("Copy link", () => copyText(`${location.origin}/api/notify?key=${encodeURIComponent(key.api_key)}&msg=Hello`, "Notification link"),
                            { ariaLabel: "Copy notification link for " + key.label }),
                        button("Copy email address", () => copyText(`${key.api_key}@notify.mad-gamer.com`, "Email address"),
                            { ariaLabel: "Copy email address for " + key.label }),
                        button("Delete", (event) => deleteKey(key, event.currentTarget), { kind: "danger", ariaLabel: "Delete key: " + key.label }),
                    ),
                ),
            ),
        );
    }
}

async function createKey(event) {
    event.preventDefault();
    const label = $("key-label").value.trim() || "Default";
    await MG.busy(event.submitter || $("key-form").querySelector("button"), async () => {
        try {
            const created = (await call("POST", "/api-keys/create", { label })).api_key;
            state.keys.push({ ...created, last_used_at: null });
            renderKeys();
            MG.show($("keys-message"), `API key "${created.label}" created.`, "success");
            $("key-label").value = "Default";
            $("k-" + created.key_id).focus();
        } catch (error) {
            MG.show($("keys-message"), error.message);
        }
    });
}

async function deleteKey(key, trigger) {
    if (!(await MG.confirmAction(`Delete the API key "${key.label}"? Anything using it will stop working.`, "Delete key"))) {
        trigger.focus();
        return;
    }
    try {
        await call("POST", "/api-keys/delete", { key_id: key.key_id });
        state.keys = state.keys.filter((k) => k.key_id !== key.key_id);
        renderKeys();
        MG.announce(`API key "${key.label}" deleted.`);
        $("keys-title").focus();
    } catch (error) {
        MG.show($("keys-message"), error.message);
    }
}

async function deleteAllKeys() {
    if (!(await MG.confirmAction("Are you sure you wish to delete all API keys?", "Delete all API keys"))) return;
    try {
        await call("POST", "/api-keys/delete-all");
        state.keys = [];
        renderKeys();
        MG.show($("keys-message"), "All API keys deleted.", "success");
        $("keys-title").focus();
    } catch (error) {
        MG.show($("keys-message"), error.message);
    }
}

// Devices

const PLATFORM_NAMES = { ios: "iPhone or iPad", android: "Android", web: "Browser" };

async function loadDevices() {
    try {
        state.devices = (await call("GET", "/devices")).devices;
        renderDevices();
    } catch (error) {
        MG.show($("devices-message"), error.message);
    }
}

function renderDevices() {
    const list = $("device-list");
    list.textContent = "";
    const mine = browserDeviceId();
    const thisBrowserRegistered = state.devices.some((d) => d.device_id === mine);
    const others = state.devices.filter((d) => d.device_id !== mine);
    $("devices-empty").hidden = state.devices.length !== 0;
    $("sign-out-others").hidden = others.length === 0;
    $("sign-out-others").textContent = thisBrowserRegistered ? "Sign out other devices" : "Sign out all devices";
    for (const device of state.devices) {
        const id = "d-" + device.device_id;
        const isThis = device.device_id === mine;
        const details = [PLATFORM_NAMES[device.platform] || device.platform, device.last_ip_address, "last seen " + MG.formatDate(device.last_seen)]
            .filter(Boolean)
            .join(" • ");
        list.append(
            el("li", {},
                el("article", { "aria-labelledby": id },
                    el("h3", { class: "item-title", id, tabindex: "-1" }, device.device_name, isThis ? el("span", { class: "badge", text: "This browser" }) : null),
                    el("p", { class: "item-meta", text: details }),
                    isThis
                        ? el("p", { class: "small muted" }, "Turn this off under ", el("a", { href: "#browser", text: "Browser notifications" }), ".")
                        : el("div", { class: "actions" },
                              button("Sign out", (event) => signOutDevice(device, event.currentTarget), { kind: "danger", ariaLabel: "Sign out " + device.device_name })),
                ),
            ),
        );
    }
}

async function signOutDevice(device, trigger) {
    if (!(await MG.confirmAction(`Sign out ${device.device_name}?`, "Sign out"))) {
        trigger.focus();
        return;
    }
    try {
        await call("POST", `/devices/${encodeURIComponent(device.device_id)}/sign-out`);
        state.devices = state.devices.filter((d) => d.device_id !== device.device_id);
        renderDevices();
        MG.announce(device.device_name + " signed out.");
        $("devices-title").focus();
    } catch (error) {
        MG.show($("devices-message"), error.message);
    }
}

async function signOutOthers() {
    const label = $("sign-out-others").textContent;
    if (!(await MG.confirmAction(`Are you sure you wish to ${label.toLowerCase()}?`, label))) return;
    try {
        await call("POST", "/devices/sign-out-other");
        await loadDevices();
        MG.show($("devices-message"), "Signed out.", "success");
        $("devices-title").focus();
    } catch (error) {
        MG.show($("devices-message"), error.message);
    }
}

// Account

async function signOut() {
    if (!(await MG.confirmAction("Are you sure you wish to sign out?", "Sign out"))) return;
    try {
        if (pushSupported()) {
            const registration = await navigator.serviceWorker.getRegistration();
            const subscription = registration && (await registration.pushManager.getSubscription());
            if (subscription) await subscription.unsubscribe();
        }
        await MG.api("POST", "/auth/logout");
    } catch {}
    MG.clearSession();
    location.assign("/");
}

async function accountForm(event, work) {
    event.preventDefault();
    const form = event.target;
    const message = $("account-message");
    const empty = [...form.querySelectorAll("input[required]")].find((input) => !input.value.trim());
    if (empty) {
        empty.setAttribute("aria-invalid", "true");
        MG.show(message, "Fill in every field.");
        empty.focus();
        return;
    }
    for (const input of form.querySelectorAll("input")) input.removeAttribute("aria-invalid");
    await MG.busy(event.submitter || form.querySelector("button[type=submit]"), async () => {
        try {
            await work();
        } catch (error) {
            MG.show(message, error.message);
        }
    });
}

function changeEmail(event) {
    accountForm(event, async () => {
        const result = await call("POST", "/account/change-email", { new_email: $("new-email").value.trim(), password: $("email-password").value });
        MG.show($("verify-required-message"), "We've sent a verification link to " + result.account.email + ".", "info");
        setAccount(result.account);
    });
}

function changePassword(event) {
    accountForm(event, async () => {
        if ($("new-password").value.length < 8) throw new Error("Your new password must be at least 8 characters.");
        await call("POST", "/account/change-password", { current_password: $("current-password").value, new_password: $("new-password").value });
        signedOut("password-changed");
    });
}

function deleteAccount(event) {
    event.preventDefault();
    accountForm(event, async () => {
        if (!(await MG.confirmAction("Permanently delete your MG Notify account? This can't be undone.", "Delete my account"))) return;
        await call("POST", "/account/delete", { password: $("delete-password").value });
        signedOut("deleted");
    });
}

async function saveDisplayName(event) {
    event.preventDefault();
    const input = $("display-name");
    await MG.busy(event.submitter || $("display-name-form").querySelector("button"), async () => {
        try {
            const result = await call("POST", "/account/display-name", { display_name: input.value.trim() });
            setAccount(result.account);
            MG.show($("account-message"), result.account.display_name ? `Display name saved: ${result.account.display_name}.` : "Display name removed.", "success");
        } catch (error) {
            MG.show($("account-message"), error.message);
        }
    });
}

/** The sound choices for this browser: Chime, No sound, and imported sounds. */
async function renderSoundList() {
    const list = $("sound-list");
    let imported = [];
    try {
        imported = await MG.sounds.list();
    } catch {
        MG.show($("sounds-message"), "This browser can't store sounds (private browsing may block it).");
    }
    const options = [
        { value: "chime", title: "Chime (default)", imported: false },
        { value: "none", title: "No sound", imported: false },
        ...imported.map((name) => ({ value: "custom:" + name, title: name, imported: true })),
    ];
    let current = MG.soundChoice();
    if (!options.some((option) => option.value === current)) {
        current = "chime";
        MG.setSoundChoice(current);
    }
    list.textContent = "";
    options.forEach((option, index) => {
        const id = "sound-" + index;
        const radio = el("input", { type: "radio", name: "sound", id, value: option.value });
        radio.checked = option.value === current;
        radio.addEventListener("change", () => {
            if (!radio.checked) return;
            MG.setSoundChoice(option.value);
            MG.playNotificationSound(option.value);
        });
        const item = el("li", {}, el("label", { class: "check", for: id }, radio, " " + option.title));
        if (option.value !== "none") {
            item.append(button("Play", () => MG.playNotificationSound(option.value), { ariaLabel: "Play " + option.title }));
        }
        if (option.imported) {
            item.append(button("Delete", async () => {
                await MG.sounds.remove(option.title);
                await renderSoundList();
                MG.announce(`${option.title} deleted.`);
                $("sounds-title").focus();
            }, { kind: "danger", ariaLabel: "Delete " + option.title }));
        }
        list.append(item);
    });
    $("sound-delete-all").hidden = imported.length === 0;
}

function setUpSounds() {
    $("sounds-title").setAttribute("tabindex", "-1");
    $("sound-add").addEventListener("click", () => $("sound-files").click());
    $("sound-files").addEventListener("change", async (event) => {
        const files = [...event.target.files];
        event.target.value = "";
        if (!files.length) return;
        const problems = [];
        let added = 0;
        for (const file of files) {
            const problem = await MG.sounds.add(file).catch(() => `${file.name} couldn't be saved.`);
            if (problem) problems.push(problem);
            else added += 1;
        }
        await renderSoundList();
        const summary = added ? `Added ${added} sound${added === 1 ? "" : "s"}.` : "No sounds were added.";
        MG.show($("sounds-message"), [summary, ...problems].join(" "), problems.length ? "error" : "success");
    });
    $("sound-delete-all").addEventListener("click", async () => {
        if (!(await MG.confirmAction("Are you sure you wish to delete all sounds?", "Delete all"))) return;
        await MG.sounds.clear();
        await renderSoundList();
        MG.show($("sounds-message"), "All sounds deleted.", "success");
        $("sounds-title").focus();
    });
    renderSoundList();
}

async function resendVerification(event) {
    await MG.busy(event.currentTarget, async () => {
        try {
            await call("POST", "/auth/resend-verification");
            MG.show($("verify-required-message"), "A new verification email has been sent.", "success");
        } catch (error) {
            MG.show($("verify-required-message"), error.message, "error");
        }
    });
}

async function checkVerification(event) {
    await MG.busy(event.currentTarget, async () => {
        try {
            const me = await call("GET", "/auth/me");
            if (me.account.email_verified) {
                location.reload();
            } else {
                MG.show($("verify-required-message"), "Your email isn't verified yet. Open the link in the verification email.", "info");
            }
        } catch (error) {
            MG.show($("verify-required-message"), error.message, "error");
        }
    });
}

document.addEventListener("DOMContentLoaded", () => {
    $("retry").addEventListener("click", () => location.reload());
    $("refresh").addEventListener("click", async (event) => {
        await MG.busy(event.currentTarget, () => loadNotifications(false));
        MG.announce("Notifications refreshed.");
    });
    $("search").addEventListener("input", renderNotifications);
    $("clear-all").addEventListener("click", async () => {
        if (!(await MG.confirmAction("Are you sure you wish to delete all your notifications?", "Delete all notifications"))) return;
        try {
            await call("DELETE", "/notifications");
            state.notifications = [];
            state.knownIds = new Set();
            renderNotifications();
            MG.announce("All notifications deleted.");
            $("notifications-title").focus();
        } catch (error) {
            MG.show($("notifications-message"), error.message);
        }
    });
    $("email-close").addEventListener("click", () => $("email-viewer").close());
    $("email-viewer").addEventListener("close", () => {
        $("email-frame").srcdoc = "";
        // The list is back in view.
        scheduleMarkSeen();
    });
    $("send-form").addEventListener("submit", sendNotification);
    $("browser-enable").addEventListener("click", enableBrowserNotifications);
    $("browser-disable").addEventListener("click", disableBrowserNotifications);
    $("browser-test").addEventListener("click", sendTest);
    $("key-form").addEventListener("submit", createKey);
    $("delete-all-keys").addEventListener("click", deleteAllKeys);
    $("sign-out-others").addEventListener("click", signOutOthers);
    $("email-form").addEventListener("submit", changeEmail);
    $("password-form").addEventListener("submit", changePassword);
    $("delete-form").addEventListener("submit", deleteAccount);
    $("resend-verification").addEventListener("click", resendVerification);
    $("display-name-form").addEventListener("submit", saveDisplayName);
    setUpSounds();
    $("check-verification").addEventListener("click", checkVerification);
    for (const node of document.querySelectorAll("[data-sign-out]")) node.addEventListener("click", signOut);
    for (const toggle of document.querySelectorAll("[data-show-passwords]")) {
        toggle.addEventListener("change", () => {
            for (const input of $(toggle.dataset.showPasswords).querySelectorAll("input[autocomplete$='password']")) {
                input.type = toggle.checked ? "text" : "password";
            }
        });
    }
    start();
});

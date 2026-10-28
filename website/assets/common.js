// Shared by every page: the API, the signed-in session, announcements.
"use strict";

const MG = (() => {
    const API = "/api";
    const TOKEN_KEY = "mgn.session";
    const EMAIL_KEY = "mgn.email";

    function token() {
        try {
            return localStorage.getItem(TOKEN_KEY) || sessionStorage.getItem(TOKEN_KEY) || "";
        } catch {
            return "";
        }
    }

    /** Keeps the session in this tab only, or across visits when remembered. */
    function saveSession(sessionToken, email, remember) {
        clearSession();
        try {
            (remember ? localStorage : sessionStorage).setItem(TOKEN_KEY, sessionToken);
            (remember ? localStorage : sessionStorage).setItem(EMAIL_KEY, email);
        } catch {}
    }

    function clearSession() {
        for (const store of [localStorage, sessionStorage]) {
            try {
                store.removeItem(TOKEN_KEY);
                store.removeItem(EMAIL_KEY);
            } catch {}
        }
    }

    class ApiError extends Error {
        constructor(status, message) {
            super(message);
            this.status = status;
        }
    }

    /** Calls the API. Throws ApiError with the server's message on failure. */
    async function api(method, path, body, { auth = true } = {}) {
        const headers = { Accept: "application/json" };
        if (body !== undefined) headers["Content-Type"] = "application/json";
        if (auth && token()) headers.Authorization = "Bearer " + token();
        let response;
        try {
            response = await fetch(API + path, {
                method,
                headers,
                body: body === undefined ? undefined : JSON.stringify(body),
                cache: "no-store",
            });
        } catch {
            throw new ApiError(0, "MG Notify could not reach the server. Check your connection and try again.");
        }
        let data = null;
        try {
            data = await response.json();
        } catch {}
        if (!response.ok) {
            let message = "Something went wrong. Please try again.";
            const detail = data && data.detail;
            if (typeof detail === "string") message = detail;
            else if (detail && typeof detail.message === "string") message = detail.message;
            else if (Array.isArray(detail) && detail[0] && detail[0].msg) message = detail[0].msg;
            if (response.status >= 502 && response.status <= 504 && !detail) {
                message = "MG Notify's server is unavailable right now. Please try again in a moment.";
            }
            throw new ApiError(response.status, message);
        }
        return data;
    }

    /** Says something to screen reader users without moving focus. */
    function announce(text, { assertive = false } = {}) {
        const region = document.getElementById(assertive ? "alert-region" : "status-region");
        if (!region) return;
        region.textContent = "";
        setTimeout(() => {
            region.textContent = text;
        }, 50);
    }

    /** Shows a message in a message box (role=alert or status set in the HTML). */
    function show(box, text, kind = "error") {
        if (!box) return;
        box.className = "message " + kind;
        box.textContent = text;
        if (text && kind === "error") box.scrollIntoView({ block: "nearest" });
    }

    /** Marks a button busy while `work` runs. */
    async function busy(button, work) {
        if (button.getAttribute("aria-busy") === "true") return;
        button.setAttribute("aria-busy", "true");
        button.setAttribute("aria-disabled", "true");
        try {
            return await work();
        } finally {
            button.removeAttribute("aria-busy");
            button.removeAttribute("aria-disabled");
        }
    }

    /** A confirmation dialog; resolves true if confirmed. Cancel has focus first. */
    function confirmAction(question, confirmLabel, { danger = true } = {}) {
        return new Promise((resolve) => {
            const dialog = document.createElement("dialog");
            dialog.setAttribute("aria-labelledby", "confirm-title");
            const heading = document.createElement("h2");
            heading.id = "confirm-title";
            heading.textContent = question;
            heading.style.fontSize = "1.2rem";
            const actions = document.createElement("div");
            actions.className = "actions";
            const cancel = document.createElement("button");
            cancel.type = "button";
            cancel.className = "button secondary";
            cancel.textContent = "Cancel";
            const confirm = document.createElement("button");
            confirm.type = "button";
            confirm.className = danger ? "button danger solid" : "button";
            confirm.textContent = confirmLabel;
            actions.append(cancel, confirm);
            dialog.append(heading, actions);
            document.body.append(dialog);
            let result = false;
            cancel.addEventListener("click", () => dialog.close());
            confirm.addEventListener("click", () => {
                result = true;
                dialog.close();
            });
            dialog.addEventListener("close", () => {
                dialog.remove();
                resolve(result);
            });
            dialog.showModal();
            cancel.focus();
        });
    }

    const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

    function formatDate(iso) {
        const date = new Date(iso);
        return isNaN(date) ? iso : dateFormat.format(date);
    }

    const SAFE_SCHEMES = /^(https?:|mailto:|tel:)/i;
    const MARKDOWN_LINK = /\[([^\]]+)\]\(([^)\s]+(?: [^)]*)?)\)/g;
    const URL_PATTERN = /\b(?:https?:\/\/|mailto:|tel:)[^\s<>()]+/gi;
    const TRIM = /[.,;:!?)\]}>"']+$/;

    function appendLink(parent, text, href) {
        const link = document.createElement("a");
        link.href = href;
        link.textContent = text;
        link.rel = "noopener noreferrer";
        link.target = "_blank";
        parent.append(link);
    }

    function appendPlain(parent, text) {
        let position = 0;
        for (const match of text.matchAll(URL_PATTERN)) {
            let url = match[0];
            const trimmed = url.replace(TRIM, "");
            parent.append(text.slice(position, match.index));
            appendLink(parent, trimmed, trimmed);
            position = match.index + trimmed.length;
        }
        parent.append(text.slice(position));
    }

    /**
     * Fills `element` with a notification's text, turning Markdown links and
     * web, mail and phone links into real links, as the apps do. Everything
     * is added as text nodes, so messages can never inject markup.
     */
    function renderMessage(element, text) {
        element.textContent = "";
        let position = 0;
        for (const match of text.matchAll(MARKDOWN_LINK)) {
            const href = match[2].trim().replace(/ /g, "%20");
            if (!SAFE_SCHEMES.test(href)) continue;
            appendPlain(element, text.slice(position, match.index));
            appendLink(element, match[1], href);
            position = match.index + match[0].length;
        }
        appendPlain(element, text.slice(position));
    }

    /** Marks the current page's link in the site navigation, and shows the right links for signed-in visitors. */
    function setUpHeader() {
        const signedIn = Boolean(token());
        for (const link of document.querySelectorAll("[data-when-signed-in]")) link.hidden = !signedIn;
        for (const link of document.querySelectorAll("[data-when-signed-out]")) link.hidden = signedIn;
    }

    /**
     * Listens to the server's event stream (GET /api/events) for this session.
     * `onEvent(name, data)` gets every event, plus "disconnected" when the
     * connection drops (it reconnects by itself, sooner when the browser comes
     * back online) and "unauthorized" when the session is no longer valid.
     * Nothing is polled: the server sends changes as they happen.
     */
    function openEventStream(onEvent) {
        let stopped = false;
        let controller = null;
        let wake = null;
        const pause = (ms) => new Promise((resolve) => {
            wake = resolve;
            setTimeout(resolve, ms);
        });
        window.addEventListener("online", () => wake && wake());

        async function run() {
            let failures = 0;
            while (!stopped) {
                controller = new AbortController();
                try {
                    const response = await fetch(API + "/events", {
                        headers: { Authorization: "Bearer " + token(), Accept: "text/event-stream" },
                        cache: "no-store",
                        signal: controller.signal,
                    });
                    if (response.status === 401) {
                        stopped = true;
                        onEvent("unauthorized", {});
                        return;
                    }
                    if (!response.ok || !response.body) throw new Error("stream unavailable");
                    failures = 0;
                    const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
                    let buffer = "";
                    while (!stopped) {
                        const { value, done } = await reader.read();
                        if (done) break;
                        buffer += value.replace(/\r\n/g, "\n");
                        let end;
                        while ((end = buffer.indexOf("\n\n")) >= 0) {
                            const block = buffer.slice(0, end);
                            buffer = buffer.slice(end + 2);
                            let name = null;
                            const data = [];
                            for (const line of block.split("\n")) {
                                if (line.startsWith("event:")) name = line.slice(6).trim();
                                else if (line.startsWith("data:")) data.push(line.slice(5).trim());
                            }
                            if (!name) continue;
                            let parsed = {};
                            try {
                                parsed = JSON.parse(data.join("\n") || "{}");
                            } catch {}
                            if (name === "signed_out" || name === "account_deleted") stopped = true;
                            onEvent(name, parsed);
                        }
                    }
                } catch {}
                if (stopped) return;
                failures += 1;
                onEvent("disconnected", { failures });
                await pause(Math.min(30000, 1000 * 2 ** Math.min(failures - 1, 5)));
            }
        }
        run();
        return {
            stop() {
                stopped = true;
                if (controller) controller.abort();
            },
        };
    }

    const SOUND_KEY = "mgn.sound";

    /**
     * This browser's sound for notifications while the site is open: "chime"
     * (the default), "none", or "custom:<name>" for an imported sound.
     */
    function soundChoice() {
        try {
            const choice = localStorage.getItem(SOUND_KEY);
            return choice === "none" || (choice && choice.startsWith("custom:")) ? choice : "chime";
        } catch {
            return "chime";
        }
    }

    function setSoundChoice(choice) {
        try {
            localStorage.setItem(SOUND_KEY, choice);
        } catch {}
    }

    /**
     * Plays the notification sound. Browsers can't play the system's own
     * notification sound, so "default" here is MG Notify's chime. Browsers
     * only allow sound after you've interacted with the page; until then it
     * stays silent rather than failing.
     */
    let playing = null;

    async function playNotificationSound(choice = soundChoice()) {
        if (choice === "none") return;
        let source = "/assets/sounds/notification.wav";
        let objectURL = null;
        if (choice.startsWith("custom:")) {
            const blob = await sounds.get(choice.slice(7)).catch(() => null);
            if (blob) {
                objectURL = URL.createObjectURL(blob);
                source = objectURL;
            }
        }
        if (playing) playing.pause();
        const audio = new Audio(source);
        playing = audio;
        if (objectURL) audio.addEventListener("ended", () => URL.revokeObjectURL(objectURL), { once: true });
        audio.play().catch(() => {});
    }

    /**
     * Sounds imported into this browser, kept in its own storage (IndexedDB):
     * nothing is uploaded. Like the apps, each must be 30 seconds or less.
     */
    const sounds = (() => {
        const MAX_SECONDS = 30;
        let database = null;

        function open() {
            if (database) return database;
            database = new Promise((resolve, reject) => {
                const request = indexedDB.open("mg-notify", 1);
                request.onupgradeneeded = () => request.result.createObjectStore("sounds", { keyPath: "name" });
                request.onsuccess = () => resolve(request.result);
                request.onerror = () => reject(request.error);
            });
            return database;
        }

        async function run(mode, work) {
            const db = await open();
            return new Promise((resolve, reject) => {
                const transaction = db.transaction("sounds", mode);
                const request = work(transaction.objectStore("sounds"));
                transaction.oncomplete = () => resolve(request && request.result);
                transaction.onerror = () => reject(transaction.error);
            });
        }

        /** How long a sound file is, or null if this browser can't play it. */
        function duration(file) {
            return new Promise((resolve) => {
                const url = URL.createObjectURL(file);
                const audio = new Audio();
                const done = (value) => {
                    URL.revokeObjectURL(url);
                    resolve(value);
                };
                audio.preload = "metadata";
                audio.addEventListener("loadedmetadata", () => done(Number.isFinite(audio.duration) ? audio.duration : null), { once: true });
                audio.addEventListener("error", () => done(null), { once: true });
                audio.src = url;
            });
        }

        return {
            async list() {
                const all = (await run("readonly", (store) => store.getAll())) || [];
                return all.map((item) => item.name).sort((a, b) => a.localeCompare(b, undefined, { sensitivity: "base" }));
            },
            async get(name) {
                const item = await run("readonly", (store) => store.get(name));
                return item ? item.blob : null;
            },
            /** Adds a file; resolves to null on success or the reason it was skipped. */
            async add(file) {
                const seconds = await duration(file);
                if (seconds === null) return `${file.name} isn't a sound this browser can play.`;
                if (seconds > MAX_SECONDS) return `${file.name} is longer than 30 seconds.`;
                const name = file.name.replace(/\.[^.]+$/, "") || file.name;
                await run("readwrite", (store) => store.put({ name, blob: file }));
                return null;
            },
            remove(name) {
                return run("readwrite", (store) => store.delete(name));
            },
            clear() {
                return run("readwrite", (store) => store.clear());
            },
        };
    })();

    document.addEventListener("DOMContentLoaded", setUpHeader);

    return {
        api, ApiError, token, saveSession, clearSession, announce, show, busy, confirmAction, formatDate, renderMessage,
        openEventStream, soundChoice, setSoundChoice, playNotificationSound, sounds,
    };
})();

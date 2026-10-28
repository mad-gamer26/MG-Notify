// MG Notify's service worker: shows Web Push notifications from the server.
"use strict";

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("push", (event) => {
    let data = {};
    try {
        data = event.data ? event.data.json() : {};
    } catch {
        data = { body: event.data ? event.data.text() : "" };
    }
    const info = data.mg_notify || {};
    event.waitUntil(
        (async () => {
            const windows = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
            // The dashboard is open and in front: it announces the notification
            // and plays its sound itself, so no pop-up.
            const inFront = windows.find(
                (client) => client.focused && client.visibilityState === "visible" && new URL(client.url).pathname === "/app.html",
            );
            if (inFront) {
                inFront.postMessage({ type: "mg-notify-push", notification_id: info.notification_id });
                return;
            }
            await self.registration.showNotification(data.title || "MG Notify", {
                body: data.body || "",
                icon: "/assets/icon-192.png",
                badge: "/assets/badge-96.png",
                tag: info.notification_id || undefined,
                data: {
                    // An email opens straight to it.
                    url: info.email && info.notification_id
                        ? "/app.html?open=" + encodeURIComponent(info.notification_id) + "#notifications"
                        : "/app.html#notifications",
                    ...info,
                },
            });
            for (const client of windows) {
                client.postMessage({ type: "mg-notify-push", notification_id: info.notification_id });
            }
        })(),
    );
});

self.addEventListener("notificationclick", (event) => {
    event.notification.close();
    const data = event.notification.data || {};
    const url = new URL(data.url || "/app.html", self.location.origin).href;
    event.waitUntil(
        (async () => {
            for (const client of await self.clients.matchAll({ type: "window", includeUncontrolled: true })) {
                if (new URL(client.url).pathname === "/app.html") {
                    await client.focus();
                    if (data.email && data.notification_id) {
                        client.postMessage({ type: "mg-notify-open-email", notification_id: data.notification_id });
                    } else {
                        client.postMessage({ type: "mg-notify-push" });
                    }
                    return;
                }
            }
            await self.clients.openWindow(url);
        })(),
    );
});

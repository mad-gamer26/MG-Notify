// Links in MG Notify's emails point at /?action=...&token=...; send them on.
"use strict";
(() => {
    const params = new URLSearchParams(location.search);
    const action = params.get("action");
    const token = params.get("token");
    const targets = { "verify-email": "/verify.html", reset: "/reset.html" };
    if (action && targets[action] && token) {
        location.replace(targets[action] + "?token=" + encodeURIComponent(token));
    } else if (action === "login" || action === "register" || action === "forgot") {
        location.replace("/signin.html?mode=" + action);
    }
})();

// The sign-in, password reset and email verification pages.
"use strict";

document.addEventListener("DOMContentLoaded", () => {
    const page = document.body.dataset.page;
    const showPassword = document.getElementById("show-password");
    if (showPassword) {
        showPassword.addEventListener("change", () => {
            for (const input of document.querySelectorAll("input[autocomplete$='password']")) {
                input.type = showPassword.checked ? "text" : "password";
            }
        });
    }
    if (page === "signin") setUpSignIn();
    if (page === "reset") setUpReset();
    if (page === "verify") verify();
});

function setUpSignIn() {
    const params = new URLSearchParams(location.search);
    if (MG.token() && !params.has("mode")) {
        location.replace("/app.html");
        return;
    }
    const tabs = [...document.querySelectorAll("[role=tab]")];
    const panel = document.getElementById("auth-panel");
    const form = document.getElementById("auth-form");
    const message = document.getElementById("auth-message");
    const email = document.getElementById("email");
    const password = document.getElementById("password");
    const submit = document.getElementById("submit");
    const titles = { login: "Sign in to MG Notify", register: "Create your MG Notify account", forgot: "Reset your password" };
    const tabTitles = { login: "Sign in", register: "Create account", forgot: "Reset password" };
    const buttons = { login: "Sign in", register: "Create account", forgot: "Send reset email" };
    let mode = "login";

    function select(newMode, focus) {
        mode = newMode;
        for (const tab of tabs) {
            const selected = tab.dataset.mode === mode;
            tab.setAttribute("aria-selected", String(selected));
            tab.tabIndex = selected ? 0 : -1;
            if (selected) {
                panel.setAttribute("aria-labelledby", tab.id);
                if (focus) tab.focus();
            }
        }
        document.getElementById("page-title").textContent = titles[mode];
        document.title = tabTitles[mode] + " – MG Notify";
        const needsPassword = mode !== "forgot";
        for (const id of ["password-field", "show-password-row", "remember-row"]) {
            document.getElementById(id).hidden = !needsPassword;
        }
        password.required = needsPassword;
        password.autocomplete = mode === "register" ? "new-password" : "current-password";
        document.getElementById("password-hint").hidden = mode !== "register";
        submit.textContent = buttons[mode];
        MG.show(message, "");
        history.replaceState(null, "", mode === "login" ? "/signin.html" : "/signin.html?mode=" + mode);
    }

    for (const tab of tabs) {
        tab.addEventListener("click", () => select(tab.dataset.mode, false));
        tab.addEventListener("keydown", (event) => {
            const index = tabs.indexOf(tab);
            const next = { ArrowRight: index + 1, ArrowLeft: index - 1, Home: 0, End: tabs.length - 1 }[event.key];
            if (next === undefined) return;
            event.preventDefault();
            select(tabs[(next + tabs.length) % tabs.length].dataset.mode, true);
        });
    }

    form.addEventListener("submit", (event) => {
        event.preventDefault();
        email.removeAttribute("aria-invalid");
        password.removeAttribute("aria-invalid");
        if (!email.value.trim()) {
            email.setAttribute("aria-invalid", "true");
            MG.show(message, "Enter your email address.");
            email.focus();
            return;
        }
        if (mode !== "forgot" && !password.value) {
            password.setAttribute("aria-invalid", "true");
            MG.show(message, "Enter your password.");
            password.focus();
            return;
        }
        if (mode === "register" && password.value.length < 8) {
            password.setAttribute("aria-invalid", "true");
            MG.show(message, "Your password must be at least 8 characters.");
            password.focus();
            return;
        }
        MG.busy(submit, async () => {
            try {
                if (mode === "forgot") {
                    await MG.api("POST", "/auth/request-password-reset", { email: email.value.trim() }, { auth: false });
                    MG.show(message, "If an account uses that address, a password reset email is on its way. The link works for one hour.", "success");
                    return;
                }
                const path = mode === "register" ? "/auth/register" : "/auth/login";
                const result = await MG.api("POST", path, { email: email.value.trim(), password: password.value }, { auth: false });
                MG.saveSession(result.session.session_token, result.account.email, document.getElementById("remember").checked);
                location.assign("/app.html");
            } catch (error) {
                MG.show(message, error.message);
            }
        });
    });

    const requested = params.get("mode");
    select(["register", "forgot"].includes(requested) ? requested : "login", false);
    const notices = {
        signedout: "You have been signed out. Please sign in again.",
        "password-changed": "Your password has been changed and you've been signed out everywhere. Sign in with your new password.",
        deleted: "Your account has been deleted.",
    };
    for (const [param, text] of Object.entries(notices)) {
        if (params.get(param)) MG.show(message, text, param === "deleted" ? "success" : "info");
    }
}

function setUpReset() {
    const token = new URLSearchParams(location.search).get("token");
    const form = document.getElementById("reset-form");
    const message = document.getElementById("reset-message");
    const input = document.getElementById("new-password");
    if (!token) {
        MG.show(message, "This page needs the link from your password reset email. Request a new one from the sign-in page.");
        form.hidden = true;
        document.getElementById("reset-done").hidden = false;
        return;
    }
    form.addEventListener("submit", (event) => {
        event.preventDefault();
        if (input.value.length < 8) {
            input.setAttribute("aria-invalid", "true");
            MG.show(message, "Your password must be at least 8 characters.");
            input.focus();
            return;
        }
        MG.busy(form.querySelector("button"), async () => {
            try {
                await MG.api("POST", "/auth/reset-password", { token, new_password: input.value }, { auth: false });
                MG.clearSession();
                form.hidden = true;
                MG.show(message, "Your password has been changed. Sign in with your new password on each of your devices.", "success");
                document.getElementById("reset-done").hidden = false;
            } catch (error) {
                MG.show(message, error.message);
            }
        });
    });
}

async function verify() {
    const title = document.getElementById("verify-title");
    const message = document.getElementById("verify-message");
    const token = new URLSearchParams(location.search).get("token");
    const finish = (heading, text, kind) => {
        title.textContent = heading;
        document.title = heading + " – MG Notify";
        MG.show(message, text, kind);
        document.getElementById("verify-next").hidden = false;
        title.focus();
    };
    if (!token) {
        finish("Verification link missing", "Open the link from your verification email, or request a new one from your dashboard.", "error");
        return;
    }
    try {
        await MG.api("POST", "/auth/verify-email", { token }, { auth: false });
        finish("Email verified", "Thanks! Your email address is verified and MG Notify is ready to use.", "success");
    } catch (error) {
        finish("Couldn't verify your email", error.message + ". You can request a new verification email from your dashboard or the app.", "error");
    }
}

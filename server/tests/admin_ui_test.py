"""Clicks through the admin page in headless Chromium (Playwright), against a
throwaway server (never the live one):

    mg-notify-server --config T/config.json      # public :18750, admin :18751
    python tests/admin_ui_test.py http://127.0.0.1:18751 http://127.0.0.1:18750

A fake iPhone (registered through the public API) and an open event stream
give the account page and Open apps something real to show.
"""

import json
import re
import sys
import threading
import time
import urllib.request
import uuid

from playwright.sync_api import expect, sync_playwright

ADMIN, PUBLIC = sys.argv[1].rstrip("/"), sys.argv[2].rstrip("/")
results = []


def ok(what, cond, detail=""):
    results.append(bool(cond))
    print(("PASS " if cond else "FAIL ") + what + ("" if cond else f" ({detail})"), flush=True)


def public(method, path, body=None, token=None):
    req = urllib.request.Request(PUBLIC + path, data=json.dumps(body).encode() if body is not None else None, method=method,
                                 headers={"Content-Type": "application/json", **({"Authorization": f"Bearer {token}"} if token else {})})
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, json.loads(r.read() or b"null")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"null")


class Stream:
    def __init__(self, token):
        req = urllib.request.Request(PUBLIC + "/events", headers={"Authorization": f"Bearer {token}", "User-Agent": "MG Notify test app"})
        self.r = urllib.request.urlopen(req, timeout=30)
        self.closed = threading.Event()
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        try:
            for _ in self.r:
                pass
        except Exception:
            pass
        self.closed.set()


def unlabeled(page):
    return page.evaluate("""() => [...document.querySelectorAll('input, select, button')]
      .filter(el => !el.closest('[hidden]') && !el.classList.contains('offscreen'))
      .filter(el => el.tagName === 'BUTTON' ? !(el.getAttribute('aria-label') || el.textContent.trim())
        : !((el.labels && el.labels.length) || el.getAttribute('aria-label')))
      .map(el => el.outerHTML.slice(0, 80))""")


def wait_announce(page, pattern, timeout=10):
    end = time.time() + timeout
    t = ""
    while time.time() < end:
        t = page.locator("#announce").text_content() or ""
        if re.search(pattern, t):
            return t
        time.sleep(0.2)
    return t


with sync_playwright() as p:
    browser = p.chromium.launch()
    page = browser.new_page()
    seen = []
    page.on("dialog", lambda d: (seen.append(d.message), d.accept()))

    page.goto(ADMIN + "/")
    page.wait_for_selector("text=MG Notify server")
    ok("the overview shows the version and counts", "running for" in page.locator("#view").text_content())
    ok("the delivery list says APNs is off here", page.locator("li", has_text="Apple push (APNs): off").count() == 1)
    ok("the smtp password is hidden", page.locator("td", has_text="(set, hidden)").count() == 1)

    # Create an account.
    page.goto(ADMIN + "/#/accounts")
    page.get_by_label("Email address", exact=True).fill("kim@example.com")
    page.get_by_label("Password", exact=True).fill("password123")
    page.get_by_label("Display name (optional)").fill("Kim")
    ok("every control on the accounts view has a name", not unlabeled(page), unlabeled(page))
    page.get_by_role("button", name="Create the account").click()
    msg = wait_announce(page, "Created the account kim@example.com")
    ok("creating is announced", "Created the account" in msg, msg)
    expect(page.get_by_role("link", name="kim@example.com")).to_be_focused(timeout=5000)
    ok("focus lands on the new account", True)

    # The person signs in on a phone, gets an API key, opens the app.
    code, body = public("POST", "/auth/login", {"email": "kim@example.com", "password": "password123"})
    ok("the new account signs in (verified)", code == 200 and body["account"]["email_verified"], body)
    token = body["session"]["session_token"]
    dev = {"device_id": str(uuid.uuid4()), "device_name": "Kim's iPhone", "apns_token": "abcd", "apns_environment": "production",
           "platform": "ios", "notification_sound": "default"}
    public("POST", "/devices/register", dev, token)
    key = public("POST", "/api-keys/create", {"label": "Home server"}, token)[1]["api_key"]["api_key"]
    urllib.request.urlopen(PUBLIC + "/notify?" + urllib.parse.urlencode({"key": key, "title": "Backup", "msg": "Disk is full"}))
    stream = Stream(token)
    time.sleep(0.5)

    page.goto(ADMIN + "/#/live")
    page.wait_for_selector("text=1 open app.")
    row = page.locator("tr", has_text="kim@example.com").text_content()
    ok("Open apps shows the account, device, address and app", all(x in row for x in ("Kim's iPhone", "127.0.0.1", "MG Notify test app")), row)

    page.goto(ADMIN + "/#/account/" + "x")
    page.goto(ADMIN + "/#/accounts")
    page.get_by_role("link", name="kim@example.com").click()
    page.wait_for_selector("text=Devices (1)")
    ok("the account shows its device with Apple push", "Apple" in page.locator("section[aria-labelledby=dev-h] tr", has_text="Kim's iPhone").text_content())
    ok("and its notification", page.locator("td", has_text="Disk is full").count() == 1)
    ok("every control on the account view has a name", not unlabeled(page), unlabeled(page))

    # API key: hidden, then Show.
    cell = page.locator("tr", has_text="Home server").locator("td").first
    ok("the key is hidden", cell.text_content() == key[:8] + "…", cell.text_content())
    page.get_by_role("button", name="Show and copy the key Home server").click()
    expect(cell).to_have_text(key, timeout=5000)
    ok("Show reveals the whole key and announces it", key in wait_announce(page, "The key Home server is"))

    # Test send.
    page.get_by_label("Title").fill("From the UI test")
    page.get_by_role("button", name="Send to every device of this account").click()
    msg = wait_announce(page, "of 1 device")
    ok("a test send reports per device", "Sent to 0 of 1 device" in msg, msg)
    ok("and says why it wasn't delivered", page.locator("li.bad", has_text="Kim's iPhone").count() == 1)

    # Revoke the key.
    page.get_by_role("button", name="Revoke the key Home server").click()
    msg = wait_announce(page, "Revoked")
    ok("revoking is announced", "Revoked the API key Home server" in msg, msg)
    try:
        urllib.request.urlopen(PUBLIC + "/notify?" + urllib.parse.urlencode({"key": key, "msg": "after"}))
        ok("the revoked key stops working", False)
    except urllib.error.HTTPError as e:
        ok("the revoked key stops working", e.code == 401)

    # Sign the device out: the open app closes.
    page.get_by_role("button", name="Sign out Kim's iPhone").click()
    msg = wait_announce(page, "Signed out")
    ok("signing a device out is announced", "Signed out kim@example.com's device Kim's iPhone" in msg, msg)
    page.wait_for_selector("text=Devices (0)")
    ok("the device is gone", True)

    # Sign out everywhere: the open app's sign-in ends.
    page.get_by_role("button", name="Sign out everywhere").click()
    msg = wait_announce(page, "out everywhere")
    ok("signing out everywhere is announced", "Signed kim@example.com out everywhere" in msg, msg)
    ok("the open app closed", stream.closed.wait(5))
    ok("the sign-in no longer works", public("GET", "/auth/me", token=token)[0] == 401)

    # Log.
    page.goto(ADMIN + "/#/log")
    page.get_by_label("Only admin actions").check()
    text = page.locator("pre").text_content()
    ok("the log shows the admin actions", all(x in text for x in ("created the account kim@example.com", "revoked kim@example.com's API key")), text[-400:])

    # Delete.
    page.goto(ADMIN + "/#/accounts")
    page.get_by_role("link", name="kim@example.com").click()
    page.get_by_role("button", name="Delete kim@example.com").click()
    page.wait_for_url(re.compile("#/accounts$"))
    msg = wait_announce(page, "Deleted the account")
    ok("deleting asks twice and is announced", "Deleted the account kim@example.com" in msg and sum("delete" in m.lower() for m in seen) >= 2, msg)
    ok("the account is gone", page.get_by_role("link", name="kim@example.com").count() == 0)
    browser.close()

print(f"\n{sum(results)}/{len(results)} checks passed")
sys.exit(0 if all(results) else 1)

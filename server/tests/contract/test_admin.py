"""The admin page (the Rust server's own admin listener), checked over HTTP
against the same throwaway server as the contract tests. run.py sets
ADMIN_URL. Only for servers that have an admin page.
"""
import json
import time
import unittest
import urllib.error
import urllib.request
import uuid

import test_api
from test_api import ContractTest, EventStream, request

ADMIN_URL = ""


def admin(method, path, body=None, headers=None, page=True):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(ADMIN_URL + path, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if page and method != "GET":
        req.add_header("Origin", ADMIN_URL)
        req.add_header("X-MG-Notify-Admin", "1")
    for name, value in (headers or {}).items():
        req.add_header(name, value)
    try:
        with urllib.request.urlopen(req) as response:
            raw = response.read()
            return response.status, (json.loads(raw) if raw.startswith(b"{") else raw.decode()), dict(response.headers)
    except urllib.error.HTTPError as error:
        raw = error.read()
        return error.code, (json.loads(raw) if raw.startswith(b"{") else raw.decode()), dict(error.headers)


class AdminTests(ContractTest):
    def a(self, method, path, body=None, status=200, **kw):
        code, data, _ = admin(method, path, body, **kw)
        self.assertEqual(code, status, data)
        return data

    def account_id(self, email):
        return next(a["account_id"] for a in self.a("GET", "/api/accounts")["accounts"] if a["email"] == email)

    def test_page_and_guard(self):
        code, body, headers = admin("GET", "/")
        self.assertEqual(code, 200)
        self.assertIn("MG Notify admin", body)
        self.assertIn("frame-ancestors 'none'", headers.get("content-security-policy", ""))
        self.assertEqual(headers.get("cache-control"), "no-cache")
        email = test_api.unique_email("guard")
        for headers in ({}, {"Origin": ADMIN_URL}, {"X-MG-Notify-Admin": "1"},
                        {"Origin": "http://evil.example", "X-MG-Notify-Admin": "1"}):
            code, _, _ = admin("POST", "/api/accounts", {"email": email, "password": "password123"}, headers=headers, page=False)
            self.assertEqual(code, 403, headers)
        self.assertFalse(any(a["email"] == email for a in self.a("GET", "/api/accounts")["accounts"]))
        # The public listener has no admin page.
        self.assertEqual(request("GET", "/api/overview")[0], 404)

    def test_create_and_sign_in(self):
        email = test_api.unique_email("made")
        r = self.a("POST", "/api/accounts", {"email": email, "password": "password123", "verified": True, "display_name": "Made Here"})
        self.a("POST", "/api/accounts", {"email": email.upper(), "password": "password123"}, status=409)
        self.a("POST", "/api/accounts", {"email": "nope", "password": "password123"}, status=422)
        self.a("POST", "/api/accounts", {"email": test_api.unique_email(), "password": "short"}, status=422)
        body = self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 200)
        self.assertTrue(body["account"]["email_verified"])
        self.assertEqual(body["account"]["display_name"], "Made Here")
        info = self.a("GET", f"/api/accounts/{r['account_id']}")
        self.assertEqual(info["account"]["email"], email)

    def test_devices_keys_notifications_and_test_send(self):
        email, token, account_id = self.register()
        ios = {"device_id": str(uuid.uuid4()), "device_name": "Test iPhone", "apns_token": "abcd",
               "apns_environment": "sandbox", "platform": "ios", "notification_sound": "default"}
        self.expect(request("POST", "/devices/register", ios, token), 200)
        key = self.api_key(token, "Server alerts")["api_key"]
        self.expect(self.notify(key, "Disk is full", "Backup"), 200)
        info = self.a("GET", f"/api/accounts/{account_id}")
        self.assertEqual([d["name"] for d in info["devices"]], ["Test iPhone"])
        self.assertEqual(info["devices"][0]["push"], "Apple (development)" if ios["apns_environment"] == "development" else "Apple")
        self.assertEqual(info["api_keys"][0]["label"], "Server alerts")
        self.assertNotIn(key, json.dumps(info), "keys are listed only by their beginning")
        self.assertEqual(info["api_keys"][0]["start"], key[:8] + "…")
        self.assertEqual(info["notifications"][0]["message"], "Disk is full")
        # Show reveals the whole key.
        key_id = info["api_keys"][0]["key_id"]
        self.assertEqual(self.a("GET", f"/api/accounts/{account_id}/keys/{key_id}")["api_key"], key)
        # A test send is stored (APNs isn't set up here, so it isn't delivered).
        r = self.a("POST", f"/api/accounts/{account_id}/test", {"title": "Hi", "message": "From the admin page"})
        self.assertIn("of 1 device", r["message"])
        titles = [n["title"] for n in self.expect(request("GET", "/notifications", token=token), 200)["notifications"]]
        self.assertIn("Hi", titles)
        self.a("POST", f"/api/accounts/{account_id}/test", {"title": "", "message": " "}, status=422)
        # Revoke the key: it stops working.
        self.a("POST", f"/api/accounts/{account_id}/keys/{key_id}/delete", {})
        self.assertEqual(self.notify(key, "After")[0], 401)
        # A new key from the admin page works.
        made = self.a("POST", f"/api/accounts/{account_id}/keys", {"label": "From admin"})
        self.expect(self.notify(made["api_key"], "Works"), 200)
        # Delete one notification, then clear them all.
        nid = self.a("GET", f"/api/accounts/{account_id}")["notifications"][0]["notification_id"]
        self.a("POST", f"/api/accounts/{account_id}/notifications/{nid}/delete", {})
        self.a("POST", f"/api/accounts/{account_id}/notifications/{nid}/delete", {}, status=404)
        self.a("POST", f"/api/accounts/{account_id}/notifications/clear", {})
        self.assertEqual(self.expect(request("GET", "/notifications", token=token), 200)["notifications"], [])
        # Sign the device out: its sign-in ends.
        self.a("POST", f"/api/accounts/{account_id}/devices/{ios['device_id']}/sign-out", {})
        self.assertEqual(self.a("GET", f"/api/accounts/{account_id}")["devices"], [])

    def test_open_apps_and_signing_out(self):
        email, token, account_id = self.register()
        stream = EventStream(token)
        stream.expect("ready")
        live = self.a("GET", "/api/live")["streams"]
        mine = [s for s in live if s["stream"]["account_id"] == account_id]
        self.assertEqual(len(mine), 1, live)
        self.assertEqual(mine[0]["email"], email)
        self.assertEqual(mine[0]["stream"]["address"], "127.0.0.1")
        self.assertEqual(self.a("GET", f"/api/accounts/{account_id}")["streams"][0]["account_id"], account_id)
        # Ending the sign-in closes its open app.
        sid = self.a("GET", f"/api/accounts/{account_id}")["sessions"][0]["id"]
        self.a("POST", f"/api/accounts/{account_id}/sessions/{sid}/delete", {})
        stream.expect("signed_out")
        self.assertTrue(stream.closed.wait(5))
        self.assertEqual(request("GET", "/auth/me", token=token)[0], 401)
        deadline = time.time() + 5
        while time.time() < deadline and any(s["stream"]["account_id"] == account_id for s in self.a("GET", "/api/live")["streams"]):
            time.sleep(0.1)
        self.assertFalse(any(s["stream"]["account_id"] == account_id for s in self.a("GET", "/api/live")["streams"]),
                         "a closed app disappears from Open apps")

    def test_password_email_name_and_delete(self):
        email, token, account_id = self.register()
        stream = EventStream(token)
        stream.expect("ready")
        self.a("POST", f"/api/accounts/{account_id}/password", {"password": "new password 1"})
        stream.expect("signed_out")
        self.assertEqual(request("GET", "/auth/me", token=token)[0], 401)
        self.expect(request("POST", "/auth/login", {"email": email, "password": "new password 1"}), 200)
        other = test_api.unique_email("taken")
        self.register_with(other)
        self.a("POST", f"/api/accounts/{account_id}/email", {"email": other}, status=409)
        new = test_api.unique_email("moved")
        self.a("POST", f"/api/accounts/{account_id}/email", {"email": new, "verified": True})
        body = self.expect(request("POST", "/auth/login", {"email": new, "password": "new password 1"}), 200)
        self.assertTrue(body["account"]["email_verified"])
        self.a("POST", f"/api/accounts/{account_id}/name", {"display_name": "Kim"})
        self.assertEqual(self.a("GET", f"/api/accounts/{account_id}")["account"]["display_name"], "Kim")
        self.a("POST", f"/api/accounts/{account_id}/delete", {})
        self.assertEqual(request("POST", "/auth/login", {"email": new, "password": "new password 1"})[0], 401)
        self.a("GET", f"/api/accounts/{account_id}", status=404)

    def register_with(self, email):
        self.expect(request("POST", "/auth/register", {"email": email, "password": "password123"}), 200)

    def test_overview_and_log(self):
        o = self.a("GET", "/api/overview")
        self.assertIn("accounts", o["counts"])
        values = json.dumps(o["settings"])
        self.assertNotIn("secret-smtp", values)
        email = test_api.unique_email("logged")
        self.a("POST", "/api/accounts", {"email": email, "password": "password123"})
        lines = self.a("GET", "/api/log?lines=2000")["lines"]
        self.assertTrue(any(f"admin: created the account {email}" in l for l in lines), lines[-5:])


if __name__ == "__main__":
    unittest.main()

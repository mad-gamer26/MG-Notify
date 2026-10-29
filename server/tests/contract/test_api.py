"""The MG Notify server's API contract, checked over HTTP.

Every server (today's Python one, the Rust one replacing it) must pass these
unchanged. Run through run.py, which starts a server and a mail sink and sets
BASE_URL and SINK below.
"""
import json
import queue
import re
import smtplib
import threading
import time
import unittest
import urllib.error
import urllib.parse
import urllib.request
import uuid

BASE_URL = ""
SINK = None
SMTP_PORT = None  # the built-in SMTP receiver, when the server has one
WEBHOOK_SECRET = "testsecret"


def request(method, path, body=None, token=None, headers=None):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(BASE_URL + path, data=data, method=method)
    if data is not None:
        req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    for name, value in (headers or {}).items():
        req.add_header(name, value)
    try:
        with urllib.request.urlopen(req) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def form_request(path, fields):
    """A POST with an HTML-form body, as PowerCom sends."""
    data = urllib.parse.urlencode(fields).encode()
    req = urllib.request.Request(BASE_URL + path, data=data, method="POST",
                                 headers={"Content-Type": "application/x-www-form-urlencoded", "User-Agent": "PowerCom"})
    try:
        with urllib.request.urlopen(req) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


def unique_email(prefix="user"):
    return f"{prefix}-{uuid.uuid4().hex[:10]}@example.com"


def mailed_token(address, action):
    for _ in range(50):
        body = SINK.latest_body(address)
        if body:
            match = re.search(r"action=" + action + r"&token=(\S+)", body)
            if match:
                return match.group(1)
        time.sleep(0.1)
    raise AssertionError(f"no {action} email for {address}")


class ContractTest(unittest.TestCase):
    maxDiff = None

    def expect(self, result, status, detail=None):
        self.assertEqual(result[0], status, result[1])
        if detail is not None:
            self.assertEqual(result[1], {"detail": detail})
        return result[1]

    def register(self, verified=True, password="password123"):
        email = unique_email()
        body = self.expect(request("POST", "/auth/register", {"email": email, "password": password}), 200)
        token = body["session"]["session_token"]
        if verified:
            self.expect(request("POST", "/auth/verify-email", {"token": mailed_token(email, "verify-email")}), 200)
        return email, token, body["account"]["account_id"]

    def api_key(self, token, label="Default"):
        return self.expect(request("POST", "/api-keys/create", {"label": label}, token), 200)["api_key"]

    def notify(self, key, msg="Hello", title=None):
        query = {"key": key, "msg": msg}
        if title is not None:
            query["title"] = title
        return request("GET", "/notify?" + urllib.parse.urlencode(query))


class HealthTests(ContractTest):
    def test_health(self):
        body = self.expect(request("GET", "/health"), 200)
        self.assertEqual(body["status"], "ok")
        self.assertEqual(set(body), {"status", "apns_enabled", "fcm_enabled", "email_enabled"})
        self.assertTrue(body["email_enabled"])


class AuthTests(ContractTest):
    def test_register_response(self):
        email = unique_email()
        body = self.expect(request("POST", "/auth/register", {"email": email.upper(), "password": "password123"}), 200)
        self.assertEqual(set(body), {"account", "session"})
        self.assertEqual(set(body["account"]), {"account_id", "email", "email_verified", "display_name"})
        self.assertEqual(body["account"]["email"], email)
        self.assertFalse(body["account"]["email_verified"])
        self.assertTrue(body["session"]["session_token"].startswith("mgsess_"))

    def test_register_duplicate(self):
        email, _, _ = self.register(verified=False)
        self.expect(request("POST", "/auth/register", {"email": email, "password": "password123"}), 409, "Account already exists")

    def test_register_short_password(self):
        status, body = request("POST", "/auth/register", {"email": unique_email(), "password": "short"})
        self.assertEqual(status, 422)
        self.assertIn("detail", body)

    def test_login(self):
        email, _, account_id = self.register()
        body = self.expect(request("POST", "/auth/login", {"email": " " + email.upper() + " ", "password": "password123"}), 200)
        self.assertEqual(body["account"], {"account_id": account_id, "email": email, "email_verified": True, "display_name": None})
        me = self.expect(request("GET", "/auth/me", token=body["session"]["session_token"]), 200)
        self.assertEqual(me, {"account": {"account_id": account_id, "email": email, "email_verified": True, "display_name": None}})

    def test_login_wrong_password(self):
        email, _, _ = self.register(verified=False)
        self.expect(request("POST", "/auth/login", {"email": email, "password": "wrong-password"}), 401, "Invalid email or password")
        self.expect(request("POST", "/auth/login", {"email": unique_email(), "password": "x"}), 401, "Invalid email or password")

    def test_missing_or_invalid_session(self):
        self.expect(request("GET", "/auth/me"), 401, "Missing bearer token")
        self.expect(request("GET", "/auth/me", token="mgsess_nope"), 401, "Invalid session")

    def test_unverified_account_is_limited(self):
        _, token, _ = self.register(verified=False)
        self.assertFalse(self.expect(request("GET", "/auth/me", token=token), 200)["account"]["email_verified"])
        for method, path in [("GET", "/notifications"), ("GET", "/devices"), ("GET", "/api-keys")]:
            self.expect(request(method, path, token=token), 403, "Email verification is required")

    def test_verify_email_tokens(self):
        email, token, _ = self.register(verified=False)
        self.expect(request("POST", "/auth/resend-verification", token=token), 200)
        verification = mailed_token(email, "verify-email")
        self.assertEqual(self.expect(request("POST", "/auth/verify-email", {"token": verification}), 200), {"verified": True})
        self.expect(request("POST", "/auth/verify-email", {"token": verification}), 400, "Verification token has already been used")
        self.expect(request("POST", "/auth/verify-email", {"token": "mgverify_nope"}), 404, "Verification token not found")
        self.assertEqual(self.expect(request("POST", "/auth/resend-verification", token=token), 200), {"accepted": True})

    def test_logout(self):
        _, token, _ = self.register()
        self.assertEqual(self.expect(request("POST", "/auth/logout", token=token), 200), {"logged_out": True})
        self.expect(request("GET", "/auth/me", token=token), 401, "Invalid session")


class PasswordTests(ContractTest):
    def test_change_password(self):
        email, token, _ = self.register()
        other_token = self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 200)["session"]["session_token"]
        self.expect(request("POST", "/account/change-password", {"current_password": "wrong", "new_password": "newpassword1"}, token), 401, "Current password is incorrect")
        body = self.expect(request("POST", "/account/change-password", {"current_password": "password123", "new_password": "newpassword1"}, token), 200)
        self.assertEqual(body, {"password_changed": True})
        for session in (token, other_token):
            self.expect(request("GET", "/auth/me", token=session), 401, "Invalid session")
        self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 401)
        self.expect(request("POST", "/auth/login", {"email": email, "password": "newpassword1"}), 200)

    def test_password_reset(self):
        email, token, _ = self.register()
        self.assertEqual(self.expect(request("POST", "/auth/request-password-reset", {"email": unique_email()}), 200), {"accepted": True})
        self.assertEqual(self.expect(request("POST", "/auth/request-password-reset", {"email": email}), 200), {"accepted": True})
        reset = mailed_token(email, "reset")
        body = self.expect(request("POST", "/auth/reset-password", {"token": reset, "new_password": "resetpass1"}), 200)
        self.assertEqual(body, {"password_reset": True})
        self.expect(request("POST", "/auth/reset-password", {"token": reset, "new_password": "resetpass2"}), 400, "Reset token has already been used")
        self.expect(request("POST", "/auth/reset-password", {"token": "mgreset_nope", "new_password": "resetpass2"}), 404, "Reset token not found")
        self.expect(request("GET", "/auth/me", token=token), 401, "Invalid session")
        self.expect(request("POST", "/auth/login", {"email": email, "password": "resetpass1"}), 200)


class AccountTests(ContractTest):
    def test_change_email(self):
        email, token, account_id = self.register()
        new_email = unique_email("new")
        self.expect(request("POST", "/account/change-email", {"new_email": new_email, "password": "wrong"}, token), 401, "Password is incorrect")
        taken, _, _ = self.register(verified=False)
        self.expect(request("POST", "/account/change-email", {"new_email": taken, "password": "password123"}, token), 409, "Account already exists")
        body = self.expect(request("POST", "/account/change-email", {"new_email": new_email, "password": "password123"}, token), 200)
        self.assertEqual(body, {"account": {"account_id": account_id, "email": new_email, "email_verified": False, "display_name": None}})
        self.expect(request("GET", "/notifications", token=token), 403, "Email verification is required")
        self.expect(request("POST", "/auth/verify-email", {"token": mailed_token(new_email, "verify-email")}), 200)
        self.expect(request("GET", "/notifications", token=token), 200)
        self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 401)

    def test_delete_account(self):
        email, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        self.expect(request("POST", "/account/delete", {"password": "wrong"}, token), 401, "Password is incorrect")
        self.assertEqual(self.expect(request("POST", "/account/delete", {"password": "password123"}, token), 200), {"deleted": True})
        self.expect(request("GET", "/auth/me", token=token), 401, "Invalid session")
        self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 401)
        self.expect(self.notify(key), 401, "Invalid API key")
        self.register()  # the address is free again
        self.expect(request("POST", "/auth/register", {"email": email, "password": "password123"}), 200)


class APIKeyTests(ContractTest):
    def test_lifecycle(self):
        _, token, _ = self.register()
        created = self.api_key(token, "  Build bot  ")
        self.assertEqual(set(created), {"key_id", "label", "api_key", "created_at"})
        self.assertEqual(created["label"], "Build bot")
        self.assertTrue(created["api_key"].startswith("mgkey_"))
        listed = self.expect(request("GET", "/api-keys", token=token), 200)["api_keys"]
        self.assertEqual(listed, [dict(created, last_used_at=None)])
        self.expect(request("POST", "/api-keys/delete", {"key_id": "nope"}, token), 404, "API key not found")
        self.assertEqual(self.expect(request("POST", "/api-keys/delete", {"key_id": created["key_id"]}, token), 200), {"deleted": True})
        self.api_key(token)
        self.api_key(token)
        self.assertEqual(self.expect(request("POST", "/api-keys/delete-all", token=token), 200), {"deleted": True, "deleted_count": 2})
        self.assertEqual(self.expect(request("GET", "/api-keys", token=token), 200), {"api_keys": []})

    def test_keys_are_per_account(self):
        _, token, _ = self.register()
        _, other, _ = self.register()
        key = self.api_key(token)
        self.expect(request("POST", "/api-keys/delete", {"key_id": key["key_id"]}, other), 404, "API key not found")


class NotificationTests(ContractTest):
    def test_notify(self):
        _, token, account_id = self.register()
        key = self.api_key(token)["api_key"]
        self.expect(self.notify("mgkey_nope"), 401, "Invalid API key")
        body = self.expect(self.notify(key, "Disk at 91%", "Server alert"), 200)
        self.assertEqual(body, {
            "status": "sent",
            "title": "Server alert",
            "message": "Disk at 91%",
            "delivery": {"delivered": 0, "devices": [], "stored": True},
        })
        self.assertEqual(self.expect(self.notify(key, "No title"), 200)["title"], "MG Notify")
        self.assertIsNotNone(self.expect(request("GET", "/api-keys", token=token), 200)["api_keys"][0]["last_used_at"])
        items = self.expect(request("GET", "/notifications", token=token), 200)["notifications"]
        self.assertEqual(len(items), 2)
        alert = next(item for item in items if item["title"] == "Server alert")
        self.assertEqual(set(alert), {"notification_id", "account_id", "title", "message", "created_at", "status", "apns_id", "email", "html", "seen"})
        self.assertFalse(alert["seen"])
        self.assertEqual(alert["account_id"], account_id)
        self.assertEqual(alert["status"], "stored")
        self.assertIsNone(alert["apns_id"])
        self.assertFalse(alert["email"])
        self.assertIsNone(alert["html"])
        self.assertRegex(alert["created_at"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")

    def test_notify_several_keys(self):
        _, token, _ = self.register()
        _, other, _ = self.register()
        first, second = self.api_key(token)["api_key"], self.api_key(other)["api_key"]
        same_account = self.api_key(token)["api_key"]
        body = self.expect(self.notify(f"{first}, {second},{same_account},mgkey_nope", "Hi"), 200)
        self.assertEqual(body["requested_keys"], 4)
        self.assertEqual(body["valid_keys"], 3)
        self.assertEqual(body["ignored_keys"], ["mgkey_nope"])
        self.assertEqual(body["delivered_accounts"], 2)
        self.assertEqual(len(body["deliveries"]), 2)
        self.assertEqual(set(body["deliveries"][0]), {"account_id", "delivery"})

    def test_notify_needs_verified_account(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        email = unique_email()
        self.expect(request("POST", "/account/change-email", {"new_email": email, "password": "password123"}, token), 200)
        self.expect(self.notify(key), 401, "Invalid API key")

    def test_notify_form_post(self):
        # As PowerCom and HTML forms send it: a form-encoded POST, not JSON.
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        body = self.expect(form_request("/notify", {"key": key, "msg": "From a form", "title": "PowerCom"}), 200)
        self.assertEqual((body["status"], body["title"], body["message"]), ("sent", "PowerCom", "From a form"))
        latest = self.expect(request("GET", "/notifications", token=token), 200)["notifications"][0]
        self.assertEqual((latest["title"], latest["message"]), ("PowerCom", "From a form"))
        self.expect(form_request("/notify", {"key": key}), 422, "msg is required")
        self.expect(form_request("/notify", {"key": "mgkey_nope", "msg": "Hi"}), 401, "Invalid API key")

    def test_form_bodies_work_everywhere(self):
        email, _, _ = self.register()
        body = self.expect(form_request("/auth/login", {"email": email, "password": "password123"}), 200)
        self.assertTrue(body["session"]["session_token"])
        req = urllib.request.Request(BASE_URL + "/notify", data=b"{not json", method="POST", headers={"Content-Type": "application/json"})
        try:
            urllib.request.urlopen(req)
            self.fail("bad JSON was accepted")
        except urllib.error.HTTPError as error:
            self.assertEqual((error.code, json.loads(error.read())), (422, {"detail": "The request body must be JSON."}))

    def test_account_notify(self):
        _, token, _ = self.register()
        body = self.expect(request("POST", "/account/notify", {"title": "T", "message": "M"}, token), 200)
        self.assertEqual(body, {"status": "sent", "title": "T", "message": "M",
                                "delivery": {"delivered": 0, "devices": [], "stored": True}})

    def test_list_delete_clear(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        self.notify(key, "first")
        time.sleep(1.1)
        self.notify(key, "second")
        items = self.expect(request("GET", "/notifications", token=token), 200)["notifications"]
        self.assertEqual([item["message"] for item in items], ["second", "first"])
        self.assertEqual(self.expect(request("GET", "/api/notifications", token=token), 200)["notifications"], items)
        self.expect(request("DELETE", "/notifications/nope", token=token), 404, "Notification not found")
        _, other, _ = self.register()
        self.expect(request("DELETE", "/notifications/" + items[0]["notification_id"], token=other), 404, "Notification not found")
        self.assertEqual(self.expect(request("DELETE", "/notifications/" + items[0]["notification_id"], token=token), 200), {"deleted": True})
        self.notify(key, "third")
        self.assertEqual(self.expect(request("DELETE", "/api/notifications", token=token), 200), {"deleted_count": 2})
        self.assertEqual(self.expect(request("GET", "/notifications", token=token), 200), {"notifications": [], "unread_count": 0})


class DeviceTests(ContractTest):
    def ios_device(self, device_id=None, sound="default"):
        return {"device_id": device_id or str(uuid.uuid4()), "device_name": "Test iPhone", "apns_token": "abcd",
                "apns_environment": "sandbox", "platform": "ios", "notification_sound": sound}

    def test_register_and_list(self):
        _, token, account_id = self.register()
        ios = self.ios_device(sound="Chime.caf")
        device = self.expect(request("POST", "/devices/register", ios, token), 200)["device"]
        self.assertEqual(device, {
            "device_id": ios["device_id"], "account_id": account_id, "device_name": "Test iPhone", "platform": "ios",
            "last_ip_address": device["last_ip_address"], "last_seen": device["last_seen"],
            "apns_token": "abcd", "apns_environment": "sandbox", "notification_sound": "Chime.caf",
        })
        android = {"device_id": "android-1", "device_name": "Pixel", "fcm_token": "tok", "platform": "android"}
        self.assertEqual(self.expect(request("POST", "/devices/register", android, token), 200)["device"]["fcm_token"], "tok")
        devices = self.expect(request("GET", "/devices", token=token), 200)["devices"]
        self.assertEqual({item["device_id"] for item in devices}, {ios["device_id"], "android-1"})

    def test_register_requires_push_token(self):
        _, token, _ = self.register()
        self.expect(request("POST", "/devices/register", {"device_id": "a", "device_name": "P", "platform": "android"}, token),
                    400, "fcm_token is required for Android devices")
        self.expect(request("POST", "/devices/register", {"device_id": "i", "device_name": "P", "platform": "ios"}, token),
                    400, "apns_token is required for iOS devices")

    def test_sound(self):
        _, token, _ = self.register()
        ios = self.ios_device()
        self.expect(request("POST", "/devices/register", ios, token), 200)
        body = self.expect(request("POST", "/devices/sound", {"device_id": ios["device_id"], "notification_sound": "__none__"}, token), 200)
        self.assertEqual(body["device"]["notification_sound"], "__none__")
        self.expect(request("POST", "/devices/sound", {"device_id": "nope", "notification_sound": "x"}, token), 404, "Device not found")

    def test_sign_out_device_ends_its_sessions(self):
        email, token, _ = self.register()
        phone = self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 200)["session"]["session_token"]
        ios = self.ios_device()
        self.expect(request("POST", "/devices/register", ios, phone), 200)
        self.assertEqual(self.expect(request("POST", f"/devices/{ios['device_id']}/sign-out", token=token), 200), {"signed_out": True})
        self.expect(request("GET", "/auth/me", token=phone), 401, "Invalid session")
        self.expect(request("GET", "/auth/me", token=token), 200)
        self.expect(request("POST", "/devices/nope/sign-out", token=token), 404, "Device not found")

    def test_sign_out_other_devices(self):
        email, token, _ = self.register()
        mine = self.ios_device()
        self.expect(request("POST", "/devices/register", mine, token), 200)
        sessions = []
        for _ in range(2):
            session = self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 200)["session"]["session_token"]
            self.expect(request("POST", "/devices/register", self.ios_device(), session), 200)
            sessions.append(session)
        for path in ("/devices/sign-out-other", "/devices/sign-out-all"):
            body = self.expect(request("POST", path, token=token), 200)
            self.assertEqual(set(body), {"signed_out", "deleted_devices", "deleted_sessions"})
        devices = self.expect(request("GET", "/devices", token=token), 200)["devices"]
        self.assertEqual([item["device_id"] for item in devices], [mine["device_id"]])
        for session in sessions:
            self.expect(request("GET", "/auth/me", token=session), 401)

    def test_delete_device_and_logout(self):
        _, token, _ = self.register()
        ios = self.ios_device()
        self.expect(request("POST", "/devices/register", ios, token), 200)
        self.assertEqual(self.expect(request("DELETE", f"/devices/{ios['device_id']}", token=token), 200), {"deleted": True})
        self.expect(request("DELETE", f"/devices/{ios['device_id']}", token=token), 404, "Device not found")
        self.expect(request("POST", "/devices/register", ios, token), 200)
        self.expect(request("POST", "/auth/logout", token=token), 200)
        _, token_two, _ = self.register()
        self.assertEqual(self.expect(request("GET", "/devices", token=token_two), 200), {"devices": []})

    def test_same_device_in_two_accounts(self):
        _, first, _ = self.register()
        _, second, _ = self.register()
        device = self.ios_device()
        self.expect(request("POST", "/devices/register", device, first), 200)
        self.expect(request("POST", "/devices/register", device, second), 200)
        self.expect(request("DELETE", f"/devices/{device['device_id']}", token=first), 200)
        self.assertEqual(len(self.expect(request("GET", "/devices", token=second), 200)["devices"]), 1)


class InboundEmailTests(ContractTest):
    def inbound(self, items, secret=WEBHOOK_SECRET):
        headers = {"X-MG-Notify-Webhook-Secret": secret} if secret else {}
        return request("POST", "/email/inbound", {"items": items}, headers=headers)

    def test_secret(self):
        self.expect(self.inbound([], secret=None), 401, "Invalid webhook secret")
        self.expect(self.inbound([], secret="wrong"), 401, "Invalid webhook secret")

    def test_delivery(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        message_id = f"<{uuid.uuid4()}@example.com>"
        item = {
            "MessageId": message_id,
            "From": {"Name": "Jane Example", "Address": "jane@example.com"},
            "Recipients": [f"{key.upper()}@notify.mad-gamer.com", "someone@elsewhere.com"],
            "Subject": "Weekly   report",
            "RawTextBody": "Hello! See https://example.com/report",
            "RawHtmlBody": "<h2>Weekly</h2><p onclick='x()'>Hi <a href='javascript:x()'>bad</a> <a href='https://example.com'>good</a></p><script>alert(1)</script>",
        }
        body = self.expect(self.inbound([item]), 200)
        self.assertEqual(body, {"status": "accepted", "processed_messages": 1, "duplicate_messages": 0,
                                "delivered_accounts": 1, "ignored_recipients": 1})
        self.assertEqual(self.expect(self.inbound([item]), 200)["duplicate_messages"], 1)
        email = self.expect(request("GET", "/notifications", token=token), 200)["notifications"][0]
        self.assertTrue(email["email"])
        self.assertEqual(email["title"], "Jane Example <jane@example.com>: Weekly report")
        self.assertEqual(email["message"], "Hello! See https://example.com/report")
        self.assertEqual(email["html"], '<h2>Weekly</h2><p>Hi <a>bad</a> <a href="https://example.com">good</a></p>')

    def test_plain_text_email(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        item = {"From": "Bob <bob@example.com>", "To": [f"{key}@notify.mad-gamer.com"], "Subject": "",
                "RawTextBody": "Line one\nhttps://example.com/a?b=1&c=2\n\n\n\nPara two"}
        self.expect(self.inbound([item]), 200)
        email = self.expect(request("GET", "/notifications", token=token), 200)["notifications"][0]
        self.assertEqual(email["title"], "Bob <bob@example.com>: (no subject)")
        self.assertEqual(email["message"], "Line one\nhttps://example.com/a?b=1&c=2\n\nPara two")
        self.assertEqual(email["html"], '<p>Line one<br><a href="https://example.com/a?b=1&amp;c=2">'
                                        'https://example.com/a?b=1&amp;c=2</a></p><p>Para two</p>')


class SMTPTests(ContractTest):
    """Mail handed straight to the server's own SMTP receiver."""

    def setUp(self):
        if not SMTP_PORT:
            self.skipTest("this server has no SMTP receiver")

    def smtp(self):
        connection = smtplib.SMTP("127.0.0.1", SMTP_PORT, timeout=10)
        connection.ehlo("tests.example.com")
        return connection

    def test_delivery(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        message = (
            "From: Jane Example <jane@example.com>\r\n"
            "To: undisclosed-recipients:;\r\n"
            "Subject: Weekly   report\r\n"
            f"Message-ID: <{uuid.uuid4()}@example.com>\r\n"
            "MIME-Version: 1.0\r\n"
            "Content-Type: multipart/alternative; boundary=b\r\n\r\n"
            "--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello! See https://example.com/report\r\n"
            "--b\r\nContent-Type: text/html\r\n\r\n<h2>Weekly</h2><p onclick='x()'>Hi</p><script>alert(1)</script>\r\n"
            "--b--\r\n"
        )
        with self.smtp() as connection:
            # The envelope decides (a Bcc recipient works); case is ignored.
            refused = connection.sendmail("jane@example.com", [f"{key.upper()}@notify.mad-gamer.com"], message)
            self.assertEqual(refused, {})
        # Delivered before the server says 250, so it's there already.
        email = self.expect(request("GET", "/notifications", token=token), 200)["notifications"][0]
        self.assertTrue(email["email"])
        self.assertEqual(email["title"], "Jane Example <jane@example.com>: Weekly report")
        self.assertEqual(email["message"], "Hello! See https://example.com/report")
        self.assertEqual(email["html"], "<h2>Weekly</h2><p>Hi</p>")

    def test_plain_text_and_dot_stuffing(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        message = "From: bob@example.com\r\nSubject: Dots\r\n\r\nLine one\r\n.Leading dot\r\n"
        with self.smtp() as connection:
            connection.sendmail("bob@example.com", [f"{key}@notify.mad-gamer.com"], message)
        email = self.expect(request("GET", "/notifications", token=token), 200)["notifications"][0]
        self.assertEqual(email["title"], "Unknown sender <bob@example.com>: Dots")
        self.assertEqual(email["message"], "Line one\n.Leading dot")

    def test_only_api_key_addresses(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        with self.smtp() as connection:
            connection.mail("someone@example.com")
            for address in ("mgkey_nope@notify.mad-gamer.com", f"{key}@elsewhere.example", "postmaster@notify.mad-gamer.com"):
                self.assertEqual(connection.rcpt(address)[0], 550, address)
            self.assertEqual(connection.rcpt(f"{key}@notify.mad-gamer.com")[0], 250)

    def test_duplicates_and_order(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        message = f"From: a@example.com\r\nSubject: Once\r\nMessage-ID: <{uuid.uuid4()}@example.com>\r\n\r\nHi\r\n"
        with self.smtp() as connection:
            self.assertEqual(connection.docmd("RCPT", f"TO:<{key}@notify.mad-gamer.com>")[0], 503)
            self.assertEqual(connection.docmd("DATA")[0], 503)
            connection.sendmail("a@example.com", [f"{key}@notify.mad-gamer.com"], message)
            connection.sendmail("a@example.com", [f"{key}@notify.mad-gamer.com"], message)
        notifications = self.expect(request("GET", "/notifications", token=token), 200)["notifications"]
        self.assertEqual([n["title"] for n in notifications], ["Unknown sender <a@example.com>: Once"])


class EventStream:
    """GET /events read in the background; events land in a queue."""

    def __init__(self, token):
        self.events = queue.Queue()
        self.closed = threading.Event()
        req = urllib.request.Request(BASE_URL + "/events", headers={"Authorization": "Bearer " + token})
        self.response = urllib.request.urlopen(req, timeout=30)
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        name, data = None, []
        try:
            for raw in self.response:
                line = raw.decode().rstrip("\r\n")
                if line.startswith("event:"):
                    name = line[6:].strip()
                elif line.startswith("data:"):
                    data.append(line[5:].strip())
                elif line == "" and name:
                    self.events.put((name, json.loads("\n".join(data) or "null")))
                    name, data = None, []
        except Exception:
            pass
        self.closed.set()

    def next(self, timeout=5):
        return self.events.get(timeout=timeout)

    def expect(self, wanted, timeout=5):
        """The next event called `wanted`, skipping others (like devices_updated)."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            name, data = self.next(max(0.1, deadline - time.time()))
            if name == wanted:
                return data
        raise AssertionError("no " + wanted + " event")

    def assert_quiet(self, unwanted, wait=1.0):
        deadline = time.time() + wait
        while time.time() < deadline:
            try:
                name, _ = self.next(max(0.05, deadline - time.time()))
            except queue.Empty:
                return
            if name == unwanted:
                raise AssertionError("unexpected " + unwanted)

    def close(self):
        self.response.close()


class DisplayNameTests(ContractTest):
    def test_display_name(self):
        _, token, account_id = self.register()
        body = self.expect(request("POST", "/account/display-name", {"display_name": "  Matthew   D  "}, token), 200)
        self.assertEqual(body["account"]["display_name"], "Matthew D")
        self.assertEqual(self.expect(request("GET", "/auth/me", token=token), 200)["account"]["display_name"], "Matthew D")
        cleared = self.expect(request("POST", "/account/display-name", {"display_name": ""}, token), 200)
        self.assertIsNone(cleared["account"]["display_name"])
        status, _ = request("POST", "/account/display-name", {"display_name": "x" * 61}, token)
        self.assertEqual(status, 422)


class EventTests(ContractTest):
    def test_requires_session(self):
        with self.assertRaises(urllib.error.HTTPError) as caught:
            EventStream("mgsess_nope")
        self.assertEqual(caught.exception.code, 401)

    def test_notifications_and_account_changes(self):
        _, token, account_id = self.register()
        stream = EventStream(token)
        ready = stream.expect("ready")
        self.assertEqual(ready["account"]["account_id"], account_id)
        key = self.api_key(token)["api_key"]
        stream.expect("api_keys_updated")
        self.notify(key, "Disk at 91%", "Server alert")
        created = stream.expect("notification")["notification"]
        self.assertEqual((created["title"], created["message"]), ("Server alert", "Disk at 91%"))
        self.expect(request("DELETE", "/notifications/" + created["notification_id"], token=token), 200)
        self.assertEqual(stream.expect("notification_deleted"), {"notification_id": created["notification_id"]})
        self.notify(key, "again")
        stream.expect("notification")
        self.expect(request("DELETE", "/notifications", token=token), 200)
        stream.expect("notifications_cleared")
        self.expect(request("POST", "/account/display-name", {"display_name": "Kitchen"}, token), 200)
        self.assertEqual(stream.expect("account_updated")["account"]["display_name"], "Kitchen")
        stream.close()

    def test_sign_out_reaches_only_that_device(self):
        email, token, _ = self.register()
        phone = self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 200)["session"]["session_token"]
        device = {"device_id": "phone-1", "device_name": "Phone", "apns_token": "abcd", "apns_environment": "sandbox"}
        self.expect(request("POST", "/devices/register", device, phone), 200)
        phone_stream, my_stream = EventStream(phone), EventStream(token)
        phone_stream.expect("ready")
        my_stream.expect("ready")
        self.expect(request("POST", "/devices/phone-1/sign-out", token=token), 200)
        phone_stream.expect("signed_out")
        self.assertTrue(phone_stream.closed.wait(5), "stream stays open after sign-out")
        my_stream.expect("devices_updated")
        my_stream.assert_quiet("signed_out")
        my_stream.close()

    def test_password_change_signs_out_every_stream(self):
        email, token, _ = self.register()
        other = self.expect(request("POST", "/auth/login", {"email": email, "password": "password123"}), 200)["session"]["session_token"]
        streams = [EventStream(token), EventStream(other)]
        for stream in streams:
            stream.expect("ready")
        self.expect(request("POST", "/account/change-password", {"current_password": "password123", "new_password": "newpassword1"}, token), 200)
        for stream in streams:
            stream.expect("signed_out")
            self.assertTrue(stream.closed.wait(5))

    def test_account_deletion(self):
        _, token, _ = self.register()
        stream = EventStream(token)
        stream.expect("ready")
        self.expect(request("POST", "/account/delete", {"password": "password123"}, token), 200)
        stream.expect("account_deleted")
        self.assertTrue(stream.closed.wait(5))


class UnreadTests(ContractTest):
    def test_unread_and_mark_seen(self):
        _, token, _ = self.register()
        key = self.api_key(token)["api_key"]
        stream = EventStream(token)
        stream.expect("ready")
        self.notify(key, "one")
        self.notify(key, "two")
        listed = self.expect(request("GET", "/notifications", token=token), 200)
        self.assertEqual(listed["unread_count"], 2)
        self.assertTrue(all(not n["seen"] for n in listed["notifications"]))
        first = listed["notifications"][0]["notification_id"]
        body = self.expect(request("POST", "/notifications/mark-seen", {"notification_ids": [first]}, token), 200)
        self.assertEqual(body, {"marked": 1, "unread_count": 1, "badge": None})
        self.assertEqual(stream.expect("notifications_seen"), {"notification_ids": [first]})
        self.assertEqual(self.expect(request("POST", "/notifications/mark-seen", {}, token), 200)["unread_count"], 0)
        self.assertEqual(self.expect(request("POST", "/notifications/mark-seen", {}, token), 200)["marked"], 0)
        stream.close()

    def test_badge_counts_every_account_on_the_device(self):
        _, first, _ = self.register()
        _, second, _ = self.register()
        device = {"device_id": "shared-phone", "device_name": "Phone", "apns_token": "abcd", "apns_environment": "sandbox", "badges": True}
        self.expect(request("POST", "/devices/register", device, first), 200)
        self.expect(request("POST", "/devices/register", device, second), 200)
        for token, count in [(first, 2), (second, 3)]:
            for _ in range(count):
                self.expect(request("POST", "/account/notify", {"title": "T", "message": "M"}, token), 200)
        # Seen on the first account: the phone still has the second's 3.
        self.assertEqual(self.expect(request("POST", "/notifications/mark-seen", {}, first), 200)["badge"], 3)
        newest = self.expect(request("GET", "/notifications", token=second), 200)["notifications"][0]
        self.expect(request("DELETE", "/notifications/" + newest["notification_id"], token=second), 200)
        self.assertEqual(self.expect(request("GET", "/notifications", token=second), 200)["unread_count"], 2)
        self.assertEqual(self.expect(request("POST", "/notifications/mark-seen", {}, second), 200)["badge"], 0)

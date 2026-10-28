"""Runs the contract tests against a freshly started server.

    python run.py --python-server path/to/server.py [--python-exe venv/bin/python]   (the old Python server)
    python run.py --command "path/to/mg-notify-server" (a server reading config.json from --workdir)

The server gets an empty data folder, a webhook secret and a mail sink, so
nothing touches real data or sends real email.
"""
import argparse
import json
import pathlib
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request

import mail_sink
import test_api


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--python-server", help="server.py to test")
    parser.add_argument("--python-exe", default=sys.executable)
    parser.add_argument("--command", help="server executable to test; run in the work folder")
    parser.add_argument("-k", dest="pattern", help="only tests matching this")
    args = parser.parse_args()

    sink = mail_sink.MailSink()
    port = free_port()
    workdir = pathlib.Path(tempfile.mkdtemp(prefix="mgn-contract-"))
    (workdir / "config.json").write_text(json.dumps({
        "server": {"data_dir": "./data", "bind_host": "127.0.0.1", "bind_port": str(port), "log_level": "WARNING"},
        "apns": {"team_id": "", "key_id": "", "bundle_id": "", "auth_key_path": ""},
        "fcm": {"project_id": "", "service_account_path": ""},
        "smtp": {"host": "127.0.0.1", "port": str(sink.port), "username": "", "password": "",
                 "from_email": "test@example.com", "from_name": "MG Notify", "use_tls": "false"},
        "web": {"base_url": "https://notify.example.com"},
    }))
    (workdir / "brevo-webhook-secret").write_text(test_api.WEBHOOK_SECRET)
    if args.python_server:
        shutil.copy(args.python_server, workdir / "server.py")
        command = [args.python_exe, "server.py"]
    else:
        command = [args.command]
    log = open(workdir / "server.log", "w")
    server = subprocess.Popen(command, cwd=workdir, stdout=log, stderr=subprocess.STDOUT)
    try:
        base = f"http://127.0.0.1:{port}"
        for _ in range(100):
            try:
                urllib.request.urlopen(base + "/health")
                break
            except OSError:
                time.sleep(0.1)
        else:
            sys.exit("server did not start; see " + str(workdir / "server.log"))
        test_api.BASE_URL = base
        test_api.SINK = sink
        loader = unittest.TestLoader()
        if args.pattern:
            loader.testNamePatterns = [f"*{args.pattern}*"]
        result = unittest.TextTestRunner(verbosity=2).run(loader.loadTestsFromModule(test_api))
        sys.exit(0 if result.wasSuccessful() else 1)
    finally:
        server.terminate()
        server.wait()
        log.close()


if __name__ == "__main__":
    main()

"""A minimal SMTP server for tests: accepts every message and keeps it in memory."""
import email
import email.policy
import socketserver
import threading


class MailSink:
    def __init__(self):
        self.messages = []
        self.lock = threading.Lock()
        sink = self

        class Handler(socketserver.StreamRequestHandler):
            def send(self, line):
                self.wfile.write((line + "\r\n").encode())

            def handle(self):
                self.send("220 sink")
                while line := self.rfile.readline():
                    command = line.decode(errors="replace").strip().upper()
                    if command == "DATA":
                        self.send("354 go ahead")
                        data = b""
                        while (chunk := self.rfile.readline()) not in (b".\r\n", b".\n", b""):
                            data += chunk[1:] if chunk.startswith(b"..") else chunk
                        with sink.lock:
                            sink.messages.append(email.message_from_bytes(data, policy=email.policy.default))
                        self.send("250 stored")
                    elif command == "QUIT":
                        self.send("221 bye")
                        return
                    else:
                        self.send("250 ok")

        socketserver.ThreadingTCPServer.allow_reuse_address = True
        self.server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
        self.port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def latest_body(self, to):
        with self.lock:
            for message in reversed(self.messages):
                if message["To"] == to:
                    return message.get_body().get_content()
        return None

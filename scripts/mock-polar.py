#!/usr/bin/env python3
"""A stand-in for Polar's public license-key endpoints, for the apps' tests.

Serves POST /v1/customer-portal/license-keys/{activate,validate,deactivate}
on 127.0.0.1 the way Polar answers them (docs/architecture.md, "License"),
with the response bodies in testdata/polar/. Point an app's Polar address at
it (each app has a development override) and try every answer:

    python3 scripts/mock-polar.py                       # port 8787, 5 seats
    python3 scripts/mock-polar.py --limit 1             # the second computer is full
    python3 scripts/mock-polar.py --key INPUTWEAVE-…    # another key that exists
    python3 scripts/mock-polar.py --revoke INPUTWEAVE-… # a refunded key
    python3 scripts/mock-polar.py --status 429 --retry-after 2
    python3 scripts/mock-polar.py --cloudflare          # Cloudflare's own 403
    python3 scripts/mock-polar.py --slow 3              # every answer 3 s late
    python3 scripts/mock-polar.py --down                # connections close unanswered
    python3 scripts/mock-polar.py --self-test

The key in testdata/polar always exists. Beyond Polar, it checks what
InputWeave promises to send: a User-Agent starting with "InputWeave/"
(otherwise it answers as Cloudflare does), the organization id, a label,
and no "conditions". It answers at most 3 requests a second, then 429, as
Polar does for unauthenticated calls. Only the standard library is used.
"""

import argparse
import copy
import datetime
import json
import os
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ORGANIZATION = "95b9d4a7-5dad-4df5-b309-2cd3383420ae"
FIXTURES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "testdata", "polar")
PREFIX = "/v1/customer-portal/license-keys/"


def fixture(name):
    with open(os.path.join(FIXTURES, name), encoding="utf-8") as f:
        return json.load(f)


def now():
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S.%fZ")


def is_uuid(text):
    try:
        uuid.UUID(str(text))
        return True
    except ValueError:
        return False


class Polar:
    """The keys and their activations, shared by every request."""

    def __init__(self, options):
        self.options = options
        self.lock = threading.Lock()
        self.activate_template = fixture("activate-200.json")
        self.validate_template = fixture("validate-200.json")
        default_key = self.validate_template["key"]
        self.keys = {}
        for key in [default_key] + list(options.key or []):
            self.keys[key] = {"id": str(uuid.uuid4()), "activations": {}, "validations": 0}
        self.keys[default_key]["id"] = self.validate_template["id"]
        self.revoked = set(options.revoke or [])
        for key in self.revoked:
            self.keys.setdefault(key, {"id": str(uuid.uuid4()), "activations": {}, "validations": 0})
        self.recent = []

    def rate_limited(self):
        with self.lock:
            moment = time.monotonic()
            self.recent = [t for t in self.recent if moment - t < 1.0]
            if len(self.recent) >= 3 and not self.options.no_rate_limit:
                return True
            self.recent.append(moment)
            return False

    def license_key(self, key):
        record = self.keys[key]
        body = copy.deepcopy(self.validate_template)
        body.pop("activation", None)
        body.update({
            "id": record["id"],
            "key": key,
            "display_key": "****-" + key[-12:],
            "limit_activations": self.options.limit,
            "validations": record["validations"],
            "status": "revoked" if key in self.revoked else "granted",
            "modified_at": now(),
        })
        return body

    def activate(self, request):
        key, label = request.get("key"), request.get("label")
        if not isinstance(label, str) or not label:
            return 422, error_body("label", "Field required")
        if "conditions" in request:
            return 422, error_body("conditions", "InputWeave sends no conditions (mock-polar)")
        if request.get("organization_id") != self.options.organization or key not in self.keys:
            return 404, fixture("404-not-found.json")
        if key in self.revoked:
            return 403, fixture("activate-403-revoked.json")
        with self.lock:
            record = self.keys[key]
            if len(record["activations"]) >= self.options.limit:
                return 403, fixture("activate-403-limit.json")
            activation = str(uuid.uuid4())
            record["activations"][activation] = {"label": label, "created_at": now()}
            body = copy.deepcopy(self.activate_template)
            body.update({
                "id": activation,
                "license_key_id": record["id"],
                "label": label,
                "meta": request.get("meta") or {},
                "created_at": record["activations"][activation]["created_at"],
            })
            body["license_key"] = self.license_key(key)
            return 200, body

    def validate(self, request):
        key, activation = request.get("key"), request.get("activation_id")
        if activation is not None and not is_uuid(activation):
            return 422, fixture("422-validation.json")
        if request.get("organization_id") != self.options.organization or key not in self.keys:
            return 404, fixture("404-not-found.json")
        if key in self.revoked:
            return 404, fixture("404-no-longer-active.json")
        with self.lock:
            record = self.keys[key]
            if activation is not None and activation not in record["activations"]:
                return 404, fixture("404-not-found.json")
            record["validations"] += 1
            body = self.license_key(key)
            body["last_validated_at"] = now()
            if activation is None:
                body["activation"] = None
            else:
                seen = record["activations"][activation]
                body["activation"] = {
                    "id": activation,
                    "license_key_id": record["id"],
                    "label": seen["label"],
                    "meta": {},
                    "created_at": seen["created_at"],
                    "modified_at": None,
                }
            return 200, body

    def deactivate(self, request):
        key, activation = request.get("key"), request.get("activation_id")
        if not is_uuid(activation):
            return 422, fixture("422-validation.json")
        if request.get("organization_id") != self.options.organization or key not in self.keys:
            return 404, fixture("404-not-found.json")
        with self.lock:
            if self.keys[key]["activations"].pop(activation, None) is None:
                return 404, fixture("404-not-found.json")
        return 204, None


def error_body(field, message):
    return {
        "error": "RequestValidationError",
        "detail": [{"type": "value_error", "loc": ["body", field], "msg": message, "input": None}],
    }


class Handler(BaseHTTPRequestHandler):
    server_version = "mock-polar"
    sys_version = ""

    def handle(self):
        if self.server.polar.options.down:
            # Accept and close: the app sees the connection fail.
            self.close_connection = True
            return
        super().handle()

    def log_message(self, fmt, *args):
        sys.stderr.write("mock-polar: " + (fmt % args) + "\n")

    def answer(self, status, body, headers=()):
        payload = b"" if body is None else (json.dumps(body) + "\n").encode()
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        if body is not None:
            self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        self.answer(405, {"error": "MethodNotAllowed", "detail": "Method Not Allowed"})

    def do_POST(self):
        polar = self.server.polar
        options = polar.options
        if options.slow:
            time.sleep(options.slow)
        if options.cloudflare or not self.headers.get("User-Agent", "").startswith("InputWeave/"):
            return self.answer(403, fixture("403-cloudflare.json"))
        if options.status == 429 or polar.rate_limited():
            return self.answer(429, fixture("429-rate-limited.json"),
                               [("Retry-After", str(options.retry_after))])
        if options.status:
            return self.answer(options.status, {"error": "MockStatus", "detail": "set with --status"})
        length = int(self.headers.get("Content-Length") or 0)
        try:
            request = json.loads(self.rfile.read(length) or b"{}")
            assert isinstance(request, dict)
        except (ValueError, AssertionError):
            return self.answer(422, error_body("body", "JSON object required"))
        action = self.path[len(PREFIX):] if self.path.startswith(PREFIX) else ""
        handlers = {"activate": polar.activate, "validate": polar.validate,
                    "deactivate": polar.deactivate}
        if action not in handlers:
            return self.answer(404, {"error": "ResourceNotFound", "detail": "Not Found"})
        if not isinstance(request.get("key"), str):
            return self.answer(422, error_body("key", "Field required"))
        status, body = handlers[action](request)
        self.answer(status, body)


def serve(options, port):
    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    server.polar = Polar(options)
    return server


def self_test(options):
    import urllib.error
    import urllib.request

    options.limit = 2
    options.no_rate_limit = True
    server = serve(options, 0)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = "http://127.0.0.1:%d%s" % (server.server_address[1], PREFIX)
    key = fixture("validate-200.json")["key"]

    def post(action, body, agent="InputWeave/1.0.0"):
        request = urllib.request.Request(base + action, data=json.dumps(body).encode(),
                                         headers={"User-Agent": agent,
                                                  "Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(request) as response:
                raw = response.read()
                return response.status, json.loads(raw) if raw else None, response.headers
        except urllib.error.HTTPError as e:
            raw = e.read()
            return e.code, json.loads(raw) if raw else None, e.headers

    org = {"organization_id": ORGANIZATION}
    status, body, _ = post("activate", {"key": key, "label": "MacBook Pro", **org})
    assert status == 200 and body["license_key"]["customer"]["email"], (status, body)
    first = body["id"]
    status, body, headers = post("validate", {"key": key, "activation_id": first, **org})
    assert status == 200 and body["activation"]["id"] == first and headers["Date"], (status, body)
    assert post("activate", {"key": key, "label": "Windows PC", **org})[0] == 200
    status, body, _ = post("activate", {"key": key, "label": "Mac mini", **org})
    assert status == 403 and body["error"] == "NotPermitted", (status, body)
    status, body, _ = post("validate", {"key": key, **org})
    assert status == 200 and body["activation"] is None, (status, body)
    assert post("deactivate", {"key": key, "activation_id": first, **org})[0] == 204
    assert post("deactivate", {"key": key, "activation_id": first, **org})[0] == 404
    assert post("validate", {"key": key, "activation_id": first, **org})[0] == 404
    assert post("validate", {"key": key, "activation_id": "xyz", **org})[0] == 422
    assert post("validate", {"key": "INPUTWEAVE-UNKNOWN", **org})[0] == 404
    assert post("activate", {"key": key, "label": "x", "conditions": {}, **org})[0] == 422
    status, body, _ = post("activate", {"key": key, "label": "x", **org}, agent="Python-urllib/3")
    assert status == 403 and body.get("cloudflare_error") and "error" not in body, (status, body)
    assert post("validate", {"key": key, "organization_id": str(uuid.uuid4())})[0] == 404
    server.polar.options.no_rate_limit = False
    server.polar.recent = []
    codes = [post("validate", {"key": key, **org})[0] for _ in range(4)]
    assert codes[:3] == [200, 200, 200] and codes[3] == 429, codes
    server.shutdown()
    print("mock-polar: self-test passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument("--limit", type=int, default=5, help="activations a key allows")
    parser.add_argument("--key", action="append", help="another key that exists")
    parser.add_argument("--revoke", action="append", help="a key that was refunded or revoked")
    parser.add_argument("--organization", default=ORGANIZATION)
    parser.add_argument("--status", type=int, help="answer every request with this status")
    parser.add_argument("--retry-after", type=int, default=2, help="seconds, with --status 429")
    parser.add_argument("--cloudflare", action="store_true",
                        help="answer every request with Cloudflare's own 403")
    parser.add_argument("--slow", type=float, default=0, help="seconds before every answer")
    parser.add_argument("--down", action="store_true", help="close connections unanswered")
    parser.add_argument("--no-rate-limit", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    options = parser.parse_args()
    if options.self_test:
        return self_test(options)
    server = serve(options, options.port)
    print("mock-polar: http://127.0.0.1:%d (organization %s, %d seats)"
          % (server.server_address[1], options.organization, options.limit), file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Run an HTTPS workflow acceptance test against a private TLS server."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys
import tempfile
import threading


APPLY_PATH = "/tenant/v1/apply/"
TOKEN = "kortty-https-apply-fixture-token"


def certificates(directory):
    def openssl(*args):
        result = subprocess.run(["openssl", *args], cwd=directory, capture_output=True)
        if result.returncode:
            raise RuntimeError(result.stderr.decode("utf-8", errors="replace"))

    (directory / "ca.cnf").write_text(
        "[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ca\n"
        "[dn]\nCN=kortty HTTPS apply test CA\n"
        "[ca]\nbasicConstraints=critical,CA:TRUE\nkeyUsage=critical,keyCertSign,cRLSign\n"
    )
    (directory / "leaf.ext").write_text(
        "basicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature,keyEncipherment\n"
        "extendedKeyUsage=serverAuth\nsubjectAltName=IP:127.0.0.1\n"
    )
    openssl("req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            "-config", "ca.cnf", "-keyout", "ca.key", "-out", "ca.pem")
    openssl("req", "-new", "-newkey", "rsa:2048", "-nodes", "-config", "ca.cnf",
            "-subj", "/CN=127.0.0.1",
            "-keyout", "leaf.key", "-out", "leaf.csr")
    openssl("x509", "-req", "-days", "1", "-in", "leaf.csr", "-CA", "ca.pem",
            "-CAkey", "ca.key", "-CAcreateserial", "-extfile", "leaf.ext", "-out", "leaf.pem")
    openssl("x509", "-in", "ca.pem", "-outform", "DER", "-out", "ca.der")


def run(command, scenario):
    with tempfile.TemporaryDirectory(prefix="https-apply-") as directory:
        root = Path(directory)
        certificates(root)
        expected = root / "approved.json"
        endpoint_path = APPLY_PATH
        if scenario == "--analysis-split":
            endpoint_path = "/tenant/v1/workflow/"
            responses = [
                (expected, True, {"summary": "Verified the reviewed code."}),
                (expected.with_suffix(".split.json"), False, {
                    "recommended": True, "rationale": "Extract the helper.",
                    "files": [
                        {"path": "entry.sh", "purpose": "Entry point", "symbols": [],
                         "executable": True, "entryPoint": True},
                        {"path": "helper.sh", "purpose": "Helper", "symbols": [],
                         "executable": False, "entryPoint": False},
                    ],
                }),
            ]
        elif scenario == "--journal-summary":
            endpoint_path = "/v1/chat/completions"
            responses = [
                (None, False, {"ready": True}),
                (None, True, None),
                (None, True, {"title": "Build output", "summary": "build.sh exited 2.",
                              "keywords": ["build.sh"]}),
            ]
        elif scenario == "--journal-question":
            endpoint_path = "/tenant/v1/journal-question/"
            responses = [(expected, True, {"findings": [
                {"citation": 1, "quote": "The deploy failed with exit 7."},
            ]})]
        elif scenario == "--planning":
            endpoint_path = "/tenant/v1/planning/"
            responses = [(expected, False, {
                "status": "questions",
                "summary": "Identify the documentation audience",
                "userMessage": "Choose the intended readers",
                "questions": [{
                    "id": "audience", "question": "Who will read the documentation?",
                    "options": ["Users", "Maintainers"], "allowCustomAnswer": False,
                }],
            })]
        else:
            responses = [(expected, True, {"lines": ["echo 'gamma'"], "changes": [
                {"id": "F3", "reason": "Quote the command argument.", "anchor": "echo 'gamma'"}
            ]})]
        verified = []
        failures = []

        class Fixture(BaseHTTPRequestHandler):
            def do_POST(self):
                try:
                    assert len(verified) < len(responses), "unexpected extra request"
                    approval, streaming, answer = responses[len(verified)]
                    assert self.path == endpoint_path, "exact HTTPS route changed"
                    assert self.headers.get("Authorization") == f"Bearer {TOKEN}", "wrong credential"
                    length = int(self.headers["Content-Length"])
                    assert 0 < length <= 1024 * 1024, "unexpected request size"
                    body = json.loads(self.rfile.read(length))
                    assert "model" not in body and body.get("stream", False) == streaming, "wrong request options"
                    assert len(body["messages"]) == 2, "unexpected message count"
                    actual = [message["content"] for message in body["messages"]]
                    if approval is not None:
                        assert actual == json.loads(approval.read_text(encoding="utf-8")), "request differs from review"
                        if scenario == "--journal-question":
                            assert "fixture-secret" not in actual[1] and TOKEN not in actual[1], "protected evidence transmitted"
                            assert "The deploy failed with exit 7. ***" in actual[1], "masked evidence missing"
                        if scenario == "--planning":
                            assert TOKEN not in actual[1] and "reference ***" in actual[1], "credential masking changed"
                    elif not streaming:
                        assert actual[1] == "{}", "probe contains journal evidence"
                    else:
                        assert "build.sh exited 2, token=***" in actual[1], "reviewed evidence missing"
                        assert "private-test-token" not in actual[1], "protected evidence transmitted"
                    if answer is None:
                        assert "response_format" in body, "expected reviewed structured request"
                        payload = {"error": {"code": "unsupported_response_format"}}
                        status = 400
                    else:
                        if scenario == "--journal-summary" and streaming:
                            assert "response_format" not in body, "retry retained the rejected format"
                        payload = {"choices": [{"message": {"content": json.dumps(answer)}}]}
                        status = 200
                    response = json.dumps(payload).encode()
                    verified.append(self.connection.version())
                    self.send_response(status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(response)))
                    self.end_headers()
                    self.wfile.write(response)
                except (AssertionError, OSError, KeyError, ValueError) as error:
                    failures.append(str(error))
                    self.send_error(400)

            def log_message(self, *args):
                pass

        with ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as server:
            context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            context.load_cert_chain(root / "leaf.pem", root / "leaf.key")
            server.socket = context.wrap_socket(server.socket, server_side=True)
            thread = threading.Thread(target=server.serve_forever)
            thread.start()
            try:
                environment = dict(os.environ,
                    KORTTY_TEST_HTTPS_APPLY_URL=f"https://127.0.0.1:{server.server_port}{endpoint_path}",
                    KORTTY_TEST_HTTPS_APPLY_CA_DER=str(root / "ca.der"),
                    KORTTY_TEST_HTTPS_APPLY_EXPECTED=str(expected),
                    KORTTY_TEST_HTTPS_APPLY_KEY=TOKEN,
                )
                result = subprocess.run(command, env=environment, timeout=600)
            finally:
                server.shutdown()
                thread.join()
        if failures:
            raise RuntimeError("; ".join(failures))
        if result.returncode:
            return result.returncode
        if len(verified) != len(responses):
            raise RuntimeError(f"Expected {len(responses)} verified requests, observed {len(verified)}")
        print(f"{verified[0]} verified {len(verified)} exact reviewed request(s) and route with no model field.")
        return 0


if __name__ == "__main__":
    arguments = sys.argv[1:]
    scenarios = {"--analysis-split", "--journal-summary", "--journal-question", "--planning"}
    scenario = arguments.pop(0) if arguments and arguments[0] in scenarios else None
    if not arguments:
        sys.exit("Pass the compiled acceptance test command and its arguments.")
    sys.exit(run(arguments, scenario))

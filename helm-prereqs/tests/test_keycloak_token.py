# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Exercise token diagnostics with real curl and an isolated HTTP endpoint."""

from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import threading
import unittest
from urllib.parse import parse_qs


SCRIPT = Path(__file__).resolve().parents[1] / "keycloak/get-token.sh"
SECRET = "synthetic-token-test-&=+%secret"


class KeycloakTokenTest(unittest.TestCase):
    def test_token_response(self):
        """Only a successful token reaches stdout; errors never echo credentials."""
        cases = [
            ("success", 200, {"access_token": "synthetic-access-token"}, "", 0),
            ("credential echo", 401,
             {"error": "invalid_client", "error_description": SECRET},
             "invalid_client); check that the client exists in the realm and its secret matches", 1),
            ("unknown error code", 400, {"error": SECRET}, "HTTP 400)", 1),
            ("structured error code", 400, {"error": {"detail": SECRET}}, "HTTP 400)", 1),
            ("non-JSON body", 502, "upstream rejected " + SECRET, "HTTP 502)", 1),
            ("non-object JSON", 400, [SECRET], "HTTP 400)", 1),
            ("token in error response", 401, {"access_token": SECRET}, "HTTP 401)", 1),
            ("error in successful response", 200,
             {"error": "invalid_grant", "access_token": SECRET},
             "invalid_grant); check the grant credentials and account setup", 1),
            ("missing token", 200, {"error_description": SECRET}, "HTTP 200)", 1),
            ("null token", 200, {"access_token": None}, "HTTP 200)", 1),
            ("structured token", 200, {"access_token": {"detail": SECRET}}, "HTTP 200)", 1),
        ]
        operator_realms = {
            "empty realm uses default": "",
            "realm with whitespace": "test realm",
            "realm with quote": "test'realm",
            "realm with backslash": r"test\realm",
            "realm with control character": "test\nrealm",
            "non-ASCII realm": "test-réalm",
            "realm with shell metacharacters": "test;$(printf unsafe)",
        }
        cases.extend((name, 401, {"error": "invalid_client", "error_description": SECRET},
                      "invalid_client)", 1) for name in operator_realms)
        requests = []

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers["Content-Length"]))
                requests.append(parse_qs(body.decode()))
                # An upstream-controlled reason phrase must stay out of diagnostics too.
                self.send_response(self.server.response_status, SECRET)
                self.end_headers()
                self.wfile.write(self.server.response_body.encode())

            def log_message(self, *args):
                pass

        with tempfile.TemporaryDirectory() as tmp, ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
            tmp_path = Path(tmp)
            # Replace only the Kubernetes pod launch. Curl and the production script
            # still perform the request, parse the HTTP status, and render the result.
            kubectl = tmp_path / "kubectl"
            kubectl.write_text(f"#!{sys.executable}\n" + """\
import json, os, pathlib, sys
pathlib.Path(os.environ["TEST_KUBECTL_ARGS"]).write_text(json.dumps(sys.argv[1:]))
args = sys.argv[sys.argv.index("--") + 1:]
args[-1] = os.environ["TEST_TOKEN_URL"]
os.execvp("curl", ["curl", *args])
""")
            kubectl.chmod(0o755)
            env = {
                **os.environ,
                "PATH": str(tmp_path) + os.pathsep + os.environ["PATH"],
                "KEYCLOAK_NS": "test-namespace",
                "KEYCLOAK_REALM": "test-realm",
                "KEYCLOAK_CLIENT_ID": "test-client&+",
                "KEYCLOAK_CLIENT_SECRET": SECRET,
                "TEST_KUBECTL_ARGS": str(tmp_path / "args.json"),
                "TEST_TOKEN_URL": f"http://127.0.0.1:{server.server_port}/token",
            }
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                for name, status, body, diagnostic, exit_code in cases:
                    with self.subTest(name=name):
                        env["KEYCLOAK_REALM"] = operator_realms.get(name, "test-realm")
                        realm = env["KEYCLOAK_REALM"] or "nico"
                        server.response_status = status
                        server.response_body = body if isinstance(body, str) else json.dumps(body)
                        result = subprocess.run(["bash", str(SCRIPT)], env=env,
                                                capture_output=True, text=True, timeout=15)
                        self.assertEqual(result.returncode, exit_code, result.stderr)
                        self.assertNotIn(SECRET, result.stdout + result.stderr)
                        if exit_code:
                            self.assertEqual(result.stdout, "")
                            self.assertIn(diagnostic, result.stderr)
                        else:
                            self.assertEqual(result.stdout, "synthetic-access-token\n")
                            self.assertEqual(result.stderr, "")
                        if isinstance(body, dict) and body.get("error") == "invalid_client":
                            self.assertIn(f"Client: 'test-client&+'; realm: {realm!r}", result.stderr)
                            command = result.stderr.split("List clients: ", 1)[1].strip()
                            self.assertEqual(shlex.split(command), [
                                "kubectl", "-n", "test-namespace", "exec", "deployment/keycloak", "--",
                                "/opt/keycloak/bin/kcadm.sh", "get", "clients", "-r", realm,
                                "--fields", "clientId",
                            ])
                        else:
                            self.assertNotIn("test-client&+", result.stderr)
                            self.assertNotIn("test-realm", result.stderr)
                            self.assertNotIn("kcadm.sh", result.stderr)
                        self.assertEqual(requests[-1], {
                            "grant_type": ["client_credentials"],
                            "client_id": ["test-client&+"],
                            "client_secret": [SECRET],
                        })
                        args = json.loads((tmp_path / "args.json").read_text())
                        self.assertNotIn(SECRET, " ".join(args))
                        self.assertIn("test-namespace", args)
                        self.assertEqual(args[-1], f"http://keycloak.test-namespace:8082/realms/{realm}/protocol/openid-connect/token")
            finally:
                server.shutdown()
                thread.join()


if __name__ == "__main__":
    unittest.main()

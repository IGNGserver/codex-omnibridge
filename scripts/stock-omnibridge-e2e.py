#!/usr/bin/env python3
"""Run a local stock Codex app-server and optional CLI fixture E2E.

This probe uses only the Python standard library.  It copies registry/catalog
metadata into a fresh temporary CODEX_HOME, keeps auth.json as a symlink to the
operator-supplied file, and emits only hashes and metadata for credentials.
"""

import argparse
import base64
import hashlib
import http.server
import json
import os
import pathlib
import shutil
import socket
import struct
import subprocess
import tempfile
import threading
import time
import urllib.request


def fingerprint(path):
    stat = path.stat()
    return {
        "mode": oct(stat.st_mode & 0o777),
        "uid": stat.st_uid,
        "gid": stat.st_gid,
        "size": stat.st_size,
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }


def has_compaction(value):
    if isinstance(value, dict):
        return value.get("type") == "compaction_trigger" or any(
            has_compaction(child) for child in value.values()
        )
    return isinstance(value, list) and any(has_compaction(child) for child in value)


class FixtureServer(http.server.ThreadingHTTPServer):
    allow_reuse_address = True

    def __init__(self, address):
        self.records = []
        super().__init__(address, FixtureHandler)


class FixtureHandler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_GET(self):
        self.send_error(404)

    def do_POST(self):
        try:
            size = int(self.headers.get("Content-Length", "0"))
            request = json.loads(self.rfile.read(size))
        except Exception as error:
            self.send_error(400, str(error))
            return
        index = len(self.server.records) + 1
        is_compaction = has_compaction(request.get("input"))
        authorization = self.headers.get("Authorization", "")
        self.server.records.append(
            {
                "index": index,
                "path": self.path,
                "model": request.get("model"),
                "kind": "compaction" if is_compaction else "turn",
                "auth_sha256": hashlib.sha256(authorization.encode()).hexdigest()
                if authorization
                else None,
                "auth_present": bool(authorization),
                "account_present": bool(self.headers.get("ChatGPT-Account-Id")),
                "capability_forwarded": bool(
                    self.headers.get("x-codex-omnibridge-token")
                ),
                "x_codex_headers": sorted(
                    name.lower()
                    for name in self.headers
                    if name.lower().startswith("x-codex-")
                ),
                "reasoning_present": "reasoning" in request,
                "input_item_types": [
                    item.get("type")
                    for item in request.get("input", [])
                    if isinstance(item, dict)
                ],
            }
        )
        response_id = f"fixture-{index}"
        events = [{"type": "response.created", "response": {"id": response_id}}]
        if is_compaction:
            events.append(
                {
                    "type": "response.output_item.done",
                    "item": {
                        "type": "compaction",
                        "encrypted_content": "fixture-summary",
                    },
                }
            )
        else:
            events.append(
                {
                    "type": "response.output_item.done",
                    "item": {
                        "type": "message",
                        "role": "assistant",
                        "id": f"message-{index}",
                        "content": [{"type": "output_text", "text": "fixture ok"}],
                    },
                }
            )
        events.append(
            {
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "usage": {
                        "input_tokens": 0,
                        "input_tokens_details": None,
                        "output_tokens": 0,
                        "output_tokens_details": None,
                        "total_tokens": 0,
                    },
                },
            }
        )
        body = "".join(
            f"event: {event['type']}\n"
            + (
                f"data: {json.dumps(event, separators=(',', ':'))}\n\n"
                if len(event) != 1
                else "\n"
            )
            for event in events
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()


class UnixWebSocket:
    def __init__(self, path):
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.settimeout(2)
        self.socket.connect(str(path))
        self.buffer = b""
        self.fragment = b""
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        ).encode()
        self.socket.sendall(request)
        while b"\r\n\r\n" not in self.buffer:
            self.buffer += self.socket.recv(65536)
        header, self.buffer = self.buffer.split(b"\r\n\r\n", 1)
        if b" 101 " not in header:
            raise RuntimeError(f"websocket handshake failed: {header!r}")

    def read(self, length):
        while len(self.buffer) < length:
            chunk = self.socket.recv(65536)
            if not chunk:
                raise EOFError("websocket closed")
            self.buffer += chunk
        value, self.buffer = self.buffer[:length], self.buffer[length:]
        return value

    def send_json(self, value):
        data = json.dumps(value, separators=(",", ":")).encode()
        mask = os.urandom(4)
        length = len(data)
        if length < 126:
            header = bytes((0x81, 0x80 | length))
        elif length <= 65535:
            header = bytes((0x81, 0xFE)) + struct.pack("!H", length)
        else:
            header = bytes((0x81, 0xFF)) + struct.pack("!Q", length)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(data))
        self.socket.sendall(header + mask + masked)

    def receive_text(self):
        while True:
            first, second = self.read(2)
            opcode = first & 0x0F
            final = bool(first & 0x80)
            masked = bool(second & 0x80)
            length = second & 0x7F
            if length == 126:
                length = struct.unpack("!H", self.read(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self.read(8))[0]
            mask = self.read(4) if masked else b""
            data = self.read(length)
            if masked:
                data = bytes(byte ^ mask[index % 4] for index, byte in enumerate(data))
            if opcode == 8:
                raise EOFError("websocket close frame")
            if opcode == 9:
                self.socket.sendall(bytes((0x8A, len(data))) + data)
                continue
            if opcode == 10:
                continue
            if opcode == 0:
                self.fragment += data
                if final:
                    data, self.fragment = self.fragment, b""
                    return data.decode()
                continue
            if opcode == 1:
                if final:
                    return data.decode()
                self.fragment = data

    def close(self):
        self.socket.close()


def wait_for(path, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if path.exists():
            return
        time.sleep(0.1)
    raise TimeoutError(f"timed out waiting for {path}")


def wait_for_health(port, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1) as response:
                if response.status == 200:
                    return
        except Exception:
            time.sleep(0.1)
    raise TimeoutError("Router did not become healthy")


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=3)


def main(args):
    repo = pathlib.Path(args.repo).resolve()
    registry_source = pathlib.Path(args.registry).resolve()
    catalog_source = pathlib.Path(args.catalog).resolve()
    capability_source = pathlib.Path(args.capability).resolve()
    auth_source = pathlib.Path(args.auth).resolve()
    auth_before = fingerprint(auth_source)
    run_dir = pathlib.Path(tempfile.mkdtemp(prefix="codex-mp-stock-e2e-", dir=args.temp_dir))
    codex_home = run_dir / "codexhome"
    xdg = run_dir / "xdg"
    codex_home.mkdir()
    xdg.mkdir()
    for source in (registry_source, catalog_source, capability_source):
        shutil.copy2(source, codex_home / source.name)
    (codex_home / "auth.json").symlink_to(auth_source)
    providers_path = codex_home / registry_source.name
    providers = json.loads(providers_path.read_text())
    if len(providers.get("providers", [])) != 1:
        raise RuntimeError("fixture requires one custom provider")
    providers["providers"][0]["base_url"] = f"http://127.0.0.1:43489/v1"
    providers_path.write_text(json.dumps(providers, indent=2) + "\n")
    capability = capability_source.read_text().strip()
    if not capability:
        raise RuntimeError("Router capability is empty")
    credential_dir = xdg / "codexmultiprovider"
    credential_dir.mkdir()
    credential_file = credential_dir / ".credentials"
    credential_file.write_text(json.dumps({"provider:newapi": args.provider_secret}))
    os.chmod(credential_file, 0o600)
    (codex_home / "config.toml").write_text(
        f'''model_catalog_json = "{codex_home / catalog_source.name}"\n'''
        'model_provider = "omnibridge"\n\n'
        "[model_providers.omnibridge]\n"
        "base_url = \"http://127.0.0.1:8787/v1\"\n"
        'name = "OpenAI"\nrequires_openai_auth = true\n'
        'supports_websockets = false\nwire_api = "responses"\n\n'
        "[model_providers.omnibridge.http_headers]\n"
        f'x-codex-omnibridge-token = "{capability}"\n'
    )
    os.chmod(codex_home / "config.toml", 0o600)

    fixture = FixtureServer(("127.0.0.1", 43489))
    threading.Thread(target=fixture.serve_forever, daemon=True).start()
    router_log_path = run_dir / "router.log"
    app_log_path = run_dir / "app-server.log"
    router_log = router_log_path.open("w")
    app_log = app_log_path.open("w")
    router = None
    app_server = None
    websocket = None
    try:
        env = os.environ.copy()
        env.update(
            {
                "XDG_CONFIG_HOME": str(xdg),
                "CODEX_MP_SECRET_BACKEND": "file",
                "CODEX_MP_OFFICIAL_BASE_URL": "http://127.0.0.1:43489/v1",
                "RUST_LOG": "error",
            }
        )
        endpoint = codex_home / "router-endpoint.json"
        router = subprocess.Popen(
            [
                args.router_bin,
                "--registry",
                str(providers_path),
                "--secret-backend",
                "file",
                "router",
                "--port",
                "8787",
                "--endpoint-file",
                str(endpoint),
            ],
            cwd=repo,
            env=env,
            stdout=router_log,
            stderr=router_log,
            start_new_session=True,
        )
        wait_for_health(8787)
        socket_path = run_dir / "app-server.sock"
        app_env = env.copy()
        app_env.update(
            {
                "CODEX_HOME": str(codex_home),
                "CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED": "1",
            }
        )
        app_server = subprocess.Popen(
            [args.app_server_bin, "--listen", f"unix://{socket_path}", "--session-source", "vscode"],
            cwd=repo,
            env=app_env,
            stdout=app_log,
            stderr=app_log,
            start_new_session=True,
        )
        wait_for(socket_path)
        websocket = UnixWebSocket(socket_path)
        rpc_id = 0

        def rpc(method, params):
            nonlocal rpc_id
            rpc_id += 1
            request_id = rpc_id
            websocket.send_json(
                {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
            )
            while True:
                message = json.loads(websocket.receive_text())
                if message.get("id") == request_id:
                    return message

        initialized = rpc(
            "initialize",
            {
                "clientInfo": {
                    "name": "codex-mp-stock-e2e",
                    "title": "Codex MultiProvider stock E2E",
                    "version": "1",
                }
            },
        )
        if "error" in initialized:
            raise RuntimeError(initialized["error"])
        websocket.send_json({"jsonrpc": "2.0", "method": "initialized", "params": {}})
        model_list = rpc("model/list", {"includeHidden": False})
        models = [
            item.get("model") or item.get("id")
            for item in (model_list.get("result") or {}).get("data", [])
        ]
        required = ["gpt-5.5", "newapi/qwen3.8"]
        if not all(model in models for model in required):
            raise RuntimeError(f"model/list missing required models: {models}")
        thread_start = rpc(
            "thread/start",
            {
                "model": "gpt-5.5",
                "modelProvider": "omnibridge",
                "cwd": str(repo),
                "ephemeral": True,
            },
        )
        if "error" in thread_start:
            raise RuntimeError(thread_start["error"])
        thread = ((thread_start.get("result") or {}).get("thread") or {})
        thread_id = thread.get("id")
        if not thread_id:
            raise RuntimeError("thread/start returned no thread id")
        turns = []
        for number, model in enumerate(("gpt-5.5", "newapi/qwen3.8", "gpt-5.5"), 1):
            turn_start = rpc(
                "turn/start",
                {
                    "threadId": thread_id,
                    "input": [{"type": "text", "text": f"fixture turn {number}"}],
                    "model": model,
                    "effort": "low",
                },
            )
            if "error" in turn_start:
                raise RuntimeError(turn_start["error"])
            deadline = time.monotonic() + args.turn_timeout
            completed = None
            while time.monotonic() < deadline:
                try:
                    message = json.loads(websocket.receive_text())
                except socket.timeout:
                    continue
                if message.get("method") != "turn/completed":
                    continue
                params = message.get("params") or {}
                completed = params.get("turn") or params
                break
            if completed is None:
                raise TimeoutError(f"turn {number} did not complete")
            turns.append(
                {
                    "number": number,
                    "requested_model": model,
                    "status": completed.get("status"),
                    "error": completed.get("error"),
                }
            )
            if completed.get("status") != "completed":
                break
        cli_result = None
        if args.cli_bin:
            # Release the app-server's CODEX_HOME SQLite handles before using
            # the same isolated home from the terminal CLI process.
            websocket.close()
            websocket = None
            stop(app_server)
            app_server = None
            cli_env = env.copy()
            cli_env["CODEX_HOME"] = str(codex_home)
            cli_env["CODEX_MP_KEY_PROVIDER_NEWAPI"] = args.provider_secret
            try:
                cli = subprocess.run(
                    [
                        args.cli_bin,
                        "exec",
                        "--ephemeral",
                        "--skip-git-repo-check",
                        "--model",
                        "newapi/qwen3.8",
                        "--json",
                        "fixture cli turn",
                    ],
                    cwd=repo,
                    env=cli_env,
                    capture_output=True,
                    text=True,
                    timeout=args.cli_timeout,
                    check=False,
                )
                cli_result = {
                    "status": cli.returncode,
                    "completed_text": "fixture ok" in cli.stdout,
                    "stderr_tail": cli.stderr[-800:],
                }
            except subprocess.TimeoutExpired as error:
                cli_result = {
                    "status": None,
                    "completed_text": False,
                    "stderr_tail": str(error)[-800:],
                }
        result = {
            "run_dir": str(run_dir),
            "model_list": [model for model in models if model in required],
            "thread": {
                "model": thread.get("model"),
                "model_provider": thread.get("modelProvider"),
            },
            "turns": turns,
            "cli": cli_result,
            "upstream": fixture.records,
            "auth_before": auth_before,
            "auth_after": fingerprint(auth_source),
            "auth_unchanged": auth_before == fingerprint(auth_source),
            "credentials_file": str(credential_file),
            "credentials_file_mode": oct(credential_file.stat().st_mode & 0o777),
            "logs": {"router": str(router_log_path), "app_server": str(app_log_path)},
        }
        print(json.dumps(result, separators=(",", ":")))
        expected_turn_models = ["gpt-5.5", "qwen3.8", "gpt-5.5"]
        if args.cli_bin:
            expected_turn_models.append("qwen3.8")
        actual_turn_models = [
            record["model"] for record in fixture.records if record["kind"] == "turn"
        ]
        if any(turn["status"] != "completed" for turn in turns):
            return 1
        if actual_turn_models != expected_turn_models:
            return 1
        if not any(record["kind"] == "compaction" for record in fixture.records):
            return 1
        if result["auth_before"] != result["auth_after"]:
            return 1
        if args.cli_bin:
            cli_records = [
                record
                for record in fixture.records
                if record["kind"] == "turn" and record["model"] == "qwen3.8"
            ]
            expected_auth = hashlib.sha256(
                f"Bearer {args.provider_secret}".encode()
            ).hexdigest()
            if (
                not cli_result
                or cli_result["status"] != 0
                or not cli_result["completed_text"]
                or len(cli_records) != 2
                or cli_records[-1]["auth_sha256"] != expected_auth
                or cli_records[-1]["capability_forwarded"]
            ):
                return 1
        return 0
    finally:
        if websocket is not None:
            websocket.close()
        stop(app_server)
        stop(router)
        fixture.shutdown()
        fixture.server_close()
        router_log.close()
        app_log.close()


def parse_args():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", required=True)
    parser.add_argument("--registry", required=True)
    parser.add_argument("--catalog", required=True)
    parser.add_argument("--capability", required=True)
    parser.add_argument("--auth", required=True)
    parser.add_argument("--router-bin", required=True)
    parser.add_argument("--app-server-bin", required=True)
    parser.add_argument(
        "--cli-bin",
        help="also run stock `codex exec` against the same isolated Router fixture",
    )
    parser.add_argument("--provider-secret", default="fixture-provider-secret")
    parser.add_argument("--turn-timeout", type=float, default=55)
    parser.add_argument("--cli-timeout", type=float, default=90)
    parser.add_argument("--temp-dir", default="/tmp")
    return parser.parse_args()


if __name__ == "__main__":
    raise SystemExit(main(parse_args()))

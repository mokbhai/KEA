#!/usr/bin/env python3
"""Tests for the kea command-line shim (src-tauri/resources/kea-cli).

The tests run the real shim with `sh` against a stdlib mock of KEA's local API.
The mock serves the model routes over a TCP port and over a Unix socket, logs
every request it receives, and answers with fixed data. A test can then assert
both what the shim sent (method, path, bearer token, multipart fields, JSON
bodies) and what the shim printed.

Run from the repository root:

    python3 scripts/test_kea_cli.py -v

Needs python3 and sh. No third-party packages, and no network beyond 127.0.0.1.
"""

import hashlib
import io
import json
import os
import pty
import re
import select
import shutil
import socket
import socketserver
import subprocess
import tempfile
import threading
import unittest
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
SHIM = os.path.join(HERE, "..", "src-tauri", "resources", "kea-cli")

TOKEN = "test-token-123"
TRANSCRIPT = 'hello {there} second "quote"'
SEGMENTS = [
    {"id": 0, "start": 0.0, "end": 1.5, "text": "hello {there}"},
    {"id": 1, "start": 1.5, "end": 3.0, "text": 'second "quote"'},
]
MODELS = {
    "object": "list",
    "data": [
        {"id": "whisper-large", "object": "model", "kea_capability": "stt", "kea_default": True},
        {"id": "kokoro-en", "object": "model", "kea_capability": "tts", "kea_default": False},
        {"id": "qwen-chat", "object": "model", "kea_capability": "llm", "kea_default": True},
    ],
}
# The line in the shim that starts argument handling. Everything above it is
# definitions only, so the JSON-escaping tests can load that part alone.
SHIM_PRELUDE_END = "# --api-url may come before the command as well as after it."


def wav_bytes():
    """A tiny mono WAV whose samples include zero bytes, so any text-mode
    handling of the body would corrupt it."""
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(16000)
        w.writeframes(b"\x00\x00\x01\x00\xff\x7f" * 200)
    return buf.getvalue()


def parse_multipart(content_type, body):
    """Returns {name: str} for text parts and {name: {"filename", "bytes"}}
    for file parts."""
    boundary = None
    for piece in content_type.split(";"):
        piece = piece.strip()
        if piece.startswith("boundary="):
            boundary = piece[len("boundary="):].strip('"').encode()
    fields = {}
    for part in body.split(b"--" + boundary)[1:]:
        if part.startswith(b"--"):
            break
        if part.startswith(b"\r\n"):
            part = part[2:]
        head, _, content = part.partition(b"\r\n\r\n")
        if content.endswith(b"\r\n"):
            content = content[:-2]
        name = re.search(rb'name="([^"]*)"', head).group(1).decode()
        filename = re.search(rb'filename="([^"]*)"', head)
        if filename:
            fields[name] = {"filename": filename.group(1).decode(), "bytes": content}
        else:
            fields[name] = content.decode("utf-8")
    return fields


def jsonable_fields(fields):
    out = {}
    for name, value in fields.items():
        if isinstance(value, dict):
            out[name] = {
                "filename": value["filename"],
                "length": len(value["bytes"]),
                "sha256": hashlib.sha256(value["bytes"]).hexdigest(),
            }
        else:
            out[name] = value
    return out


def error_body(message, error_type="invalid_request_error", code=None):
    return {"error": {"message": message, "type": error_type, "code": code}}


def route(method, path, fields, body_json):
    """Returns (status, content_type, payload_bytes)."""
    def as_json(status, obj):
        return status, "application/json", json.dumps(obj).encode()

    if method == "GET" and path == "/v1/models":
        return as_json(200, MODELS)
    if method == "GET" and path == "/v1/status":
        return as_json(200, {"version": "test"})

    if method == "POST" and path == "/v1/audio/transcriptions":
        audio = fields["file"]["bytes"]
        if audio.startswith(b"BAD"):
            return as_json(400, error_body("could not decode the uploaded audio", code="decode_failed"))
        if fields.get("model") == "nope":
            return as_json(404, error_body("The model 'nope' does not exist", code="model_not_found"))
        fmt = fields.get("response_format", "json")
        speakers = fields.get("speakers") == "true"
        if fmt == "verbose_json":
            segments = []
            for seg in SEGMENTS:
                seg = dict(seg)
                if speakers:
                    seg["speaker"] = "Person %d" % (seg["id"] + 1)
                segments.append(seg)
            return as_json(200, {"task": "transcribe", "language": "en", "duration": 3.0,
                                 "text": TRANSCRIPT, "segments": segments})
        if fmt == "json":
            return as_json(200, {"text": TRANSCRIPT})
        if fmt == "text":
            return 200, "text/plain", TRANSCRIPT.encode()
        if fmt == "srt":
            srt = ("1\n00:00:00,000 --> 00:00:01,500\nhello {there}\n\n"
                   "2\n00:00:01,500 --> 00:00:03,000\nsecond \"quote\"\n\n")
            return 200, "application/x-subrip", srt.encode()
        if fmt == "vtt":
            vtt = ("WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nhello {there}\n\n"
                   "00:00:01.500 --> 00:00:03.000\nsecond \"quote\"\n\n")
            return 200, "text/vtt", vtt.encode()
        return as_json(400, error_body("unknown response_format " + fmt))

    if method == "POST" and path == "/v1/audio/speech":
        if body_json.get("input") == "__409__":
            return as_json(409, error_body(
                "No text-to-speech model is bound for API use. Choose one on the Models page.",
                code="model_not_bound"))
        if body_json.get("model") == "nope":
            return as_json(404, error_body("The model 'nope' does not exist", code="model_not_found"))
        speed = body_json.get("speed")
        if speed is not None and not (0.25 <= speed <= 4.0):
            return as_json(400, error_body("speed must be between 0.25 and 4.0", code="invalid_speed"))
        if body_json.get("response_format", "wav") != "wav":
            return as_json(400, error_body("only wav and pcm are supported"))
        return 200, "audio/wav", wav_bytes()

    if method == "POST" and path == "/v1/chat/completions":
        messages = body_json.get("messages", [])
        if any(isinstance(m.get("content"), list) for m in messages):
            return as_json(400, error_body("only text content is supported"))
        if body_json.get("model") == "nope":
            return as_json(404, error_body("The model 'nope' does not exist", code="model_not_found"))
        user = [m["content"] for m in messages if m.get("role") == "user"][-1]
        # ensure_ascii is on (the default), so the reply arrives with \u escapes,
        # including a surrogate pair for the emoji.
        return as_json(200, {
            "id": "chatcmpl-test", "object": "chat.completion", "created": 1,
            "model": "qwen-chat",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": user},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        })

    return as_json(404, error_body("no such route", code="not_found"))


class Handler(BaseHTTPRequestHandler):
    # Set per server by make_handler: the transport name and the log file.
    transport = "tcp"
    log_path = None
    log_lock = threading.Lock()

    def log_message(self, fmt, *args):
        pass

    def do_GET(self):
        self.handle_api()

    def do_POST(self):
        self.handle_api()

    def handle_api(self):
        length = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(length)
        content_type = self.headers.get("content-type", "")
        auth = self.headers.get("authorization")

        record = {
            "transport": self.transport,
            "method": self.command,
            "path": self.path,
            "authorization": auth,
            "content_type": content_type,
        }
        fields = {}
        body_json = {}
        if content_type.startswith("multipart/form-data"):
            fields = parse_multipart(content_type, body)
            record["fields"] = jsonable_fields(fields)
        elif body:
            record["raw_body"] = body.decode("utf-8", "replace")
            try:
                body_json = json.loads(body)
                record["json"] = body_json
            except ValueError:
                pass
        with Handler.log_lock:
            with open(self.log_path, "a", encoding="utf-8") as f:
                f.write(json.dumps(record) + "\n")

        if auth != "Bearer " + TOKEN:
            status, ctype, payload = 401, "application/json", json.dumps(
                error_body("Invalid or missing token", "authentication_error", "invalid_token")).encode()
        else:
            status, ctype, payload = route(self.command, self.path, fields, body_json)

        self.send_response(status)
        self.send_header("content-type", ctype)
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def make_handler(transport, log_path):
    return type("Handler_" + transport, (Handler,), {"transport": transport, "log_path": log_path})


class UnixServer(socketserver.ThreadingMixIn, socketserver.UnixStreamServer):
    daemon_threads = True


class TcpServer(ThreadingHTTPServer):
    daemon_threads = True


class MockApi:
    """Both transports, started on background threads for the test class."""

    def __init__(self, root):
        self.log_path = os.path.join(root, "requests.log")
        open(self.log_path, "w").close()
        self.socket_path = os.path.join(root, "kea.sock")
        self.tcp = TcpServer(("127.0.0.1", 0), make_handler("tcp", self.log_path))
        self.port = self.tcp.server_address[1]
        self.unix = UnixServer(self.socket_path, make_handler("unix", self.log_path))
        for server in (self.tcp, self.unix):
            threading.Thread(target=server.serve_forever, daemon=True).start()

    @property
    def base_url(self):
        return "http://127.0.0.1:%d" % self.port

    def requests(self):
        with open(self.log_path, encoding="utf-8") as f:
            return [json.loads(line) for line in f if line.strip()]

    def reset(self):
        open(self.log_path, "w").close()

    def stop(self):
        self.tcp.shutdown()
        self.tcp.server_close()
        self.unix.shutdown()
        self.unix.server_close()


def closed_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class ShimTestCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = tempfile.mkdtemp(prefix="kea-cli-test-")
        cls.api = MockApi(cls.root)
        cls.token_file = os.path.join(cls.root, "api-token")
        with open(cls.token_file, "w") as f:
            f.write(TOKEN + "\n")
        cls.home = os.path.join(cls.root, "home")
        os.makedirs(cls.home)

    @classmethod
    def tearDownClass(cls):
        cls.api.stop()
        shutil.rmtree(cls.root, ignore_errors=True)

    def setUp(self):
        self.api.reset()
        self.work = tempfile.mkdtemp(prefix="work-", dir=self.root)

    def shim(self, *args, stdin=None, env=None):
        """Runs the shim. Defaults to the TCP transport with the right token.
        An env value of None removes that variable for the run."""
        full = dict(os.environ)
        full.update({
            "HOME": self.home,
            "KEA_API_TOKEN_FILE": self.token_file,
            "KEA_API_SOCKET": self.api.socket_path,
            "KEA_API_URL": self.api.base_url,
        })
        for key, value in (env or {}).items():
            if value is None:
                full.pop(key, None)
            else:
                full[key] = value
        io_args = {"input": stdin} if stdin is not None else {"stdin": subprocess.DEVNULL}
        return subprocess.run(["sh", SHIM, *args], capture_output=True, env=full, timeout=60, **io_args)

    def last(self):
        reqs = self.api.requests()
        self.assertTrue(reqs, "the mock received no request")
        return reqs[-1]


class SttTests(ShimTestCase):
    def write_audio(self, name="call.m4a", data=b"AUDIO-BYTES"):
        path = os.path.join(self.work, name)
        with open(path, "wb") as f:
            f.write(data)
        return path, hashlib.sha256(data).hexdigest()

    def test_default_text_format_prints_the_transcript(self):
        path, sha = self.write_audio()
        r = self.shim("stt", path)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.decode(), TRANSCRIPT + "\n")
        req = self.last()
        self.assertEqual(req["method"], "POST")
        self.assertEqual(req["path"], "/v1/audio/transcriptions")
        self.assertEqual(req["authorization"], "Bearer " + TOKEN)
        self.assertEqual(req["fields"]["response_format"], "text")
        self.assertEqual(req["fields"]["file"]["sha256"], sha)
        self.assertEqual(req["fields"]["file"]["filename"], "call.m4a")
        self.assertNotIn("speakers", req["fields"])
        self.assertNotIn("language", req["fields"])

    def test_json_format_prints_the_json_body(self):
        path, _ = self.write_audio()
        r = self.shim("stt", path, "--format", "json")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout.decode()), {"text": TRANSCRIPT})
        self.assertEqual(self.last()["fields"]["response_format"], "json")

    def test_srt_passes_through_without_speakers(self):
        path, _ = self.write_audio()
        r = self.shim("stt", path, "--format", "srt")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("00:00:00,000 --> 00:00:01,500\nhello {there}\n", r.stdout.decode())
        self.assertEqual(self.last()["fields"]["response_format"], "srt")

    def test_stdin_input_is_uploaded(self):
        data = b"PIPED-AUDIO"
        r = self.shim("stt", "-", stdin=data)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.last()["fields"]["file"]["sha256"], hashlib.sha256(data).hexdigest())

    def test_srt_with_speakers_prefixes_each_cue(self):
        path, _ = self.write_audio()
        r = self.shim("stt", path, "--format", "srt", "--speakers")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            r.stdout.decode(),
            "1\n00:00:00,000 --> 00:00:01,500\nPerson 1: hello {there}\n\n"
            "2\n00:00:01,500 --> 00:00:03,000\nPerson 2: second \"quote\"\n\n")
        req = self.last()
        self.assertEqual(req["fields"]["response_format"], "verbose_json")
        self.assertEqual(req["fields"]["speakers"], "true")

    def test_vtt_with_speakers(self):
        path, _ = self.write_audio()
        r = self.shim("stt", path, "--format", "vtt", "--speakers")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(
            r.stdout.decode(),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nPerson 1: hello {there}\n\n"
            "00:00:01.500 --> 00:00:03.000\nPerson 2: second \"quote\"\n\n")

    def test_text_with_speakers_prints_one_line_per_segment(self):
        path, _ = self.write_audio()
        r = self.shim("stt", path, "--speakers")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.decode(), 'Person 1: hello {there}\nPerson 2: second "quote"\n')

    def test_verbose_json_with_speakers_is_the_raw_body(self):
        path, _ = self.write_audio()
        r = self.shim("stt", path, "--format", "verbose_json", "--speakers")
        self.assertEqual(r.returncode, 0, r.stderr)
        body = json.loads(r.stdout.decode())
        self.assertEqual([s["speaker"] for s in body["segments"]], ["Person 1", "Person 2"])

    def test_language_and_model_are_sent_as_form_fields(self):
        path, _ = self.write_audio()
        r = self.shim("stt", path, "--language", "de", "--model", "whisper-large")
        self.assertEqual(r.returncode, 0, r.stderr)
        fields = self.last()["fields"]
        self.assertEqual(fields["language"], "de")
        self.assertEqual(fields["model"], "whisper-large")

    def test_undecodable_upload_prints_the_api_message_and_exits_1(self):
        path, _ = self.write_audio(data=b"BAD-AUDIO")
        r = self.shim("stt", path)
        self.assertEqual(r.returncode, 1)
        self.assertEqual(r.stdout, b"")
        self.assertIn("kea: could not decode the uploaded audio", r.stderr.decode())

    def test_missing_file_argument_is_a_usage_error(self):
        self.assertEqual(self.shim("stt").returncode, 64)

    def test_unknown_format_is_a_usage_error(self):
        path, _ = self.write_audio()
        r = self.shim("stt", path, "--format", "mp3")
        self.assertEqual(r.returncode, 64)
        self.assertEqual(self.api.requests(), [])


class TtsTests(ShimTestCase):
    def test_out_writes_a_valid_wav_and_sends_every_field(self):
        out = os.path.join(self.work, "done.wav")
        r = self.shim("tts", "Build finished", "--out", out,
                      "--voice", "af_bella", "--speed", "1.25", "--model", "kokoro-en")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, b"")
        with open(out, "rb") as f:
            self.assertEqual(f.read(), wav_bytes())
        with wave.open(out, "rb") as w:
            self.assertEqual((w.getnchannels(), w.getsampwidth(), w.getframerate()), (1, 2, 16000))
        req = self.last()
        self.assertEqual(req["path"], "/v1/audio/speech")
        self.assertEqual(req["authorization"], "Bearer " + TOKEN)
        self.assertEqual(req["json"], {"input": "Build finished", "response_format": "wav",
                                       "voice": "af_bella", "speed": 1.25, "model": "kokoro-en"})

    def test_piped_stdout_receives_the_wav_bytes(self):
        r = self.shim("tts", "Hello")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout, wav_bytes())
        self.assertEqual(self.last()["json"], {"input": "Hello", "response_format": "wav"})

    def test_stdin_text(self):
        out = os.path.join(self.work, "stdin.wav")
        r = self.shim("tts", "-", "--out", out, stdin=b"from stdin")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.last()["json"]["input"], "from stdin")

    def test_terminal_guard_refuses_without_a_request(self):
        # stdout is a real pseudo-terminal, which is what the guard checks.
        master, slave = pty.openpty()
        full = dict(os.environ, HOME=self.home, KEA_API_TOKEN_FILE=self.token_file,
                    KEA_API_URL=self.api.base_url)
        proc = subprocess.Popen(["sh", SHIM, "tts", "Hello"], stdin=subprocess.DEVNULL,
                                stdout=slave, stderr=subprocess.PIPE, env=full)
        os.close(slave)
        try:
            _, err = proc.communicate(timeout=30)
            written = b""
            while select.select([master], [], [], 0.2)[0]:
                try:
                    chunk = os.read(master, 4096)
                except OSError:
                    break
                if not chunk:
                    break
                written += chunk
        finally:
            os.close(master)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("--out", err.decode())
        self.assertNotIn(b"RIFF", written)
        self.assertEqual(self.api.requests(), [], "no request should be made to a terminal")

    def test_api_error_prints_the_message_and_writes_no_file(self):
        out = os.path.join(self.work, "never.wav")
        r = self.shim("tts", "__409__", "--out", out)
        self.assertEqual(r.returncode, 1)
        self.assertIn("kea: No text-to-speech model is bound for API use.", r.stderr.decode())
        self.assertFalse(os.path.exists(out))

    def test_unknown_model_is_an_api_error(self):
        r = self.shim("tts", "hi", "--model", "nope")
        self.assertEqual(r.returncode, 1)
        self.assertEqual(r.stdout, b"")
        self.assertIn("kea: The model 'nope' does not exist", r.stderr.decode())

    def test_speed_outside_the_range_is_refused_by_the_api(self):
        r = self.shim("tts", "hi", "--speed", "9")
        self.assertEqual(r.returncode, 1)
        self.assertIn("speed must be between 0.25 and 4.0", r.stderr.decode())

    def test_speed_that_is_not_a_number_is_a_usage_error(self):
        r = self.shim("tts", "hi", "--speed", "fast")
        self.assertEqual(r.returncode, 64)
        self.assertEqual(self.api.requests(), [])


class LlmTests(ShimTestCase):
    # Quotes, backslashes, tab, newline, a control character, a non-BMP
    # character and an accented one: every case the escaper has to get right.
    PROMPT = 'Quote " back \\ tab\t newline\nctrl\x01 emoji \U0001F600 é 中'

    def test_prompt_survives_escaping_and_decoding_unchanged(self):
        r = self.shim("llm", self.PROMPT)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.decode("utf-8"), self.PROMPT + "\n")
        self.assertEqual(self.last()["json"]["messages"],
                         [{"role": "user", "content": self.PROMPT}])

    def test_piped_stdin_is_appended_after_a_blank_line(self):
        r = self.shim("llm", "Write a commit message for this", stdin=b"diff --git a b\n")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.last()["json"]["messages"][-1]["content"],
                         "Write a commit message for this\n\ndiff --git a b")

    def test_dash_reads_the_prompt_from_stdin(self):
        r = self.shim("llm", "-", stdin=b"from stdin\n")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.decode(), "from stdin\n")

    def test_system_and_model_are_sent(self):
        r = self.shim("llm", "hi", "--system", "Be terse.", "--model", "qwen-chat")
        self.assertEqual(r.returncode, 0, r.stderr)
        body = self.last()["json"]
        self.assertEqual(body["messages"], [{"role": "system", "content": "Be terse."},
                                            {"role": "user", "content": "hi"}])
        self.assertEqual(body["model"], "qwen-chat")

    def test_provider_style_error_is_an_api_error(self):
        r = self.shim("llm", "hi", "--model", "nope")
        self.assertEqual(r.returncode, 1)
        self.assertIn("kea: The model 'nope' does not exist", r.stderr.decode())


class ModelsTests(ShimTestCase):
    def test_lists_capabilities_and_marks_defaults(self):
        r = self.shim("models")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.decode(),
                         "whisper-large  stt  [default]\n"
                         "kokoro-en  tts\n"
                         "qwen-chat  llm  [default]\n")
        self.assertEqual(self.last()["method"], "GET")
        self.assertEqual(self.last()["authorization"], "Bearer " + TOKEN)

    def test_json_prints_the_raw_list(self):
        r = self.shim("models", "--json")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout.decode()), MODELS)


class TransportTests(ShimTestCase):
    def test_unix_socket_is_used_when_no_url_is_set(self):
        r = self.shim("models", env={"KEA_API_URL": None})
        self.assertEqual(r.returncode, 0, r.stderr)
        req = self.last()
        self.assertEqual(req["transport"], "unix")
        self.assertEqual(req["authorization"], "Bearer " + TOKEN)

    def test_api_url_flag_before_the_command_selects_tcp(self):
        r = self.shim("--api-url", self.api.base_url, "models", env={"KEA_API_URL": None,
                                                                     "KEA_API_SOCKET": "/nonexistent/kea.sock"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.last()["transport"], "tcp")

    def test_api_url_flag_after_the_command_selects_tcp(self):
        r = self.shim("models", "--api-url", self.api.base_url, env={"KEA_API_URL": None,
                                                                     "KEA_API_SOCKET": "/nonexistent/kea.sock"})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(self.last()["transport"], "tcp")

    def test_api_off_exits_69_with_the_general_settings_hint(self):
        r = self.shim("models", env={"KEA_API_URL": None, "KEA_API_SOCKET": "/nonexistent/kea.sock"})
        self.assertEqual(r.returncode, 69)
        self.assertIn("General", r.stderr.decode())

    def test_unreadable_token_exits_77(self):
        r = self.shim("models", env={"KEA_API_TOKEN_FILE": "/nonexistent/api-token"})
        self.assertEqual(r.returncode, 77)
        self.assertIn("cannot read", r.stderr.decode())

    def test_refused_tcp_port_exits_69(self):
        r = self.shim("models", env={"KEA_API_URL": "http://127.0.0.1:%d" % closed_port()})
        self.assertEqual(r.returncode, 69)
        self.assertIn("nothing is listening", r.stderr.decode())

    def test_wrong_token_is_a_401_api_error(self):
        wrong = os.path.join(self.work, "wrong-token")
        with open(wrong, "w") as f:
            f.write("not-the-token\n")
        r = self.shim("models", env={"KEA_API_TOKEN_FILE": wrong})
        self.assertEqual(r.returncode, 1)
        self.assertIn("kea: Invalid or missing token", r.stderr.decode())
        self.assertEqual(self.last()["authorization"], "Bearer not-the-token")

    def test_status_still_works_over_the_socket(self):
        r = self.shim("status", env={"KEA_API_URL": None})
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(json.loads(r.stdout.decode()), {"version": "test"})


class JsonEscapeTests(unittest.TestCase):
    """json_string is tested directly: the shim's definitions are loaded on
    their own, and the output is parsed by Python's json module."""

    CASES = [
        "",
        "plain",
        'quote " here',
        "back\\slash",
        "tab\tnewline\ncr\rend",
        "\x01\x1f\x08\x0c",
        "trailing newline\n",
        "\n\n",
        "é 中 \U0001F600",
        '\\"',
        "ends with a backslash \\",
        "{braces} and [brackets]",
    ]

    @classmethod
    def setUpClass(cls):
        with open(SHIM, encoding="utf-8") as f:
            text = f.read()
        if SHIM_PRELUDE_END not in text:
            raise AssertionError("the shim no longer has the prelude marker used by the tests")
        cls.root = tempfile.mkdtemp(prefix="kea-cli-escape-")
        cls.prelude = os.path.join(cls.root, "prelude.sh")
        with open(cls.prelude, "w", encoding="utf-8") as f:
            f.write(text.split(SHIM_PRELUDE_END)[0])

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)

    def test_json_string_round_trips_through_a_json_parser(self):
        for value in self.CASES:
            with self.subTest(value=value):
                r = subprocess.run(["sh", "-c", '. "$1"; json_string "$2"', "sh", self.prelude, value],
                                   capture_output=True, timeout=30)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual(json.loads(r.stdout.decode("utf-8")), value)


if __name__ == "__main__":
    unittest.main()

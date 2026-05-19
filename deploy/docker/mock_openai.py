"""Mock OpenAI-compatible server for pg_aidb regression tests.

Endpoints:
  GET  /health                 -> 200 {}
  POST /v1/embeddings          -> 200  1536-dim unit vector (all inputs → same vector)
  POST /v1/chat/completions    -> 200  fixed mock answer

Returns 1536-dim embeddings so they are compatible with ai.chunks.embedding vector(1536).
All embeddings are identical (unit vector), giving cosine similarity = 1.0 for all queries —
this is intentional for deterministic retrieval in tests.

do_GET is required: BaseHTTPRequestHandler returns 501 without it. (HANDOFF.md §5)
"""
import json
import math
from http.server import BaseHTTPRequestHandler, HTTPServer

# 1536-dim unit vector: magnitude = 1, cosine similarity between any two = 1.0
_DIM = 1536
_UNIT = 1.0 / math.sqrt(_DIM)
_EMBEDDING = [_UNIT] * _DIM

_MOCK_ANSWER = (
    "PostgreSQL is a powerful open-source relational database system "
    "with advanced features including ACID compliance, complex queries, "
    "and extensibility. [mock answer]"
)


class Handler(BaseHTTPRequestHandler):
    def _send(self, code: int, body: dict) -> None:
        payload = json.dumps(body).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def _read_body(self) -> dict:
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length)
        try:
            return json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            return {}

    def do_GET(self) -> None:
        if self.path == "/health":
            self._send(200, {})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self) -> None:
        body = self._read_body()

        if self.path == "/v1/embeddings":
            inputs = body.get("input", [""])
            if isinstance(inputs, str):
                inputs = [inputs]
            self._send(200, {
                "object": "list",
                "model": body.get("model", "text-embedding-3-small"),
                "data": [
                    {"object": "embedding", "index": i, "embedding": _EMBEDDING}
                    for i in range(len(inputs))
                ],
                "usage": {"prompt_tokens": len(inputs), "total_tokens": len(inputs)},
            })

        elif self.path == "/v1/chat/completions":
            self._send(200, {
                "id": "mock-chat-0001",
                "object": "chat.completion",
                "model": body.get("model", "gpt-4o"),
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": _MOCK_ANSWER},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 10, "completion_tokens": 20, "total_tokens": 30},
            })

        else:
            self._send(404, {"error": f"not found: {self.path}"})

    def log_message(self, format: str, *args) -> None:  # noqa: A002  # silence access log
        pass


if __name__ == "__main__":
    server = HTTPServer(("0.0.0.0", 8080), Handler)
    print("mock_openai listening on :8080", flush=True)
    server.serve_forever()

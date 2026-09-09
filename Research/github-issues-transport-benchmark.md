# Shelbi GitHub transport experiment, 2026-09-09

Companion to [[github-issues-robustness-and-performance]].

Copy the script below into a new temporary directory as `benchmark.py`, then run `python3 benchmark.py` there.
Requires Python 3 and `gh` on PATH; starts one loopback HTTP server on an ephemeral port.
The script and its `results.json` are the reproducibility artifacts. All files are in this
scratch directory; no repository files or GitHub resources were changed.

## Method

- macOS 26.6.2 arm64, Python 3.14.6, gh 2.96.0 (2026-07-02).
- Fixed HTTP/1.1 JSON response: 100 issue-shaped records, 47,577 bytes per page.
- Compare a fresh `gh api` subprocess for each operation with one persistent Python
  `http.client.HTTPConnection` across operations. Both fully consume and parse JSON.
- Single-page operations: 3 warmups, 60 measured samples. Ten-page operations:
  3 warmups, 16 measured samples. REST pages linked with `Link: rel="next"`.
- All workloads sequential. TCP_NODELAY enabled on server sockets. No TLS, DNS,
  WAN, GitHub rate limits, concurrency, credential resolution, or Rust client involved.
- Both GH_TOKEN and GH_ENTERPRISE_TOKEN contain a dummy value, GH_CONFIG_DIR points
  inside this scratch directory, and all mock requests were verified to carry only
  that dummy credential. Enterprise token matters: gh resolves auth per request
  host, so a loopback host can otherwise trigger keychain fallback and contaminate
  timings. No real credentials were used or printed.
- Two server conditions: zero injected delay and 25 ms sleep per request. The OS
  can overshoot that sleep; do not subtract precisely 25 ms from measured times.
- p95 is nearest rank (`ceil(.95 * n)`). With 16 samples, p95 is the maximum;
  regard these as a small exploratory benchmark, not a production performance SLO.
- 1,898 total loopback GETs including warmups; zero live GitHub API requests.

## Selected results

Timings are milliseconds per full operation. `gh --jq .[]` matches Shelbi's
current REST issue/comment output format; ten-page gh operations use --paginate.

| Delay/request | Pages/op | Client | Median | p95 | Measured requests | New TCP connections |
|---|---:|---|---:|---:|---:|---:|
| 0 ms | 1 | gh raw | 21.336 | 24.425 | 60 | 60 |
| 0 ms | 1 | gh --jq .[] | 22.979 | 27.690 | 60 | 60 |
| 0 ms | 1 | Pooled Python HTTP | 0.218 | 0.242 | 60 | 0 |
| 0 ms | 10 | gh raw --paginate | 26.261 | 26.854 | 160 | 16 |
| 0 ms | 10 | gh --paginate --jq .[] | 34.637 | 35.472 | 160 | 16 |
| 0 ms | 10 | Pooled Python HTTP | 2.300 | 2.426 | 160 | 0 |
| 25 ms | 1 | gh --jq .[] | 51.664 | 53.455 | 60 | 60 |
| 25 ms | 1 | Pooled Python HTTP | 30.032 | 30.504 | 60 | 0 |
| 25 ms | 10 | gh --paginate --jq .[] | 337.050 | 345.651 | 160 | 16 |
| 25 ms | 10 | Pooled Python HTTP | 295.370 | 300.379 | 160 | 0 |

Pooled connections were established during warmup, which is why their measured
new-connection count is zero. Each ten-page gh invocation reused one TCP connection;
it is incorrect to claim that gh necessarily spawns or reconnects once per page.
All sample values and additional --slurp/raw controls are in results.json.

## Interpretation

There is a measurable local cost to per-operation process startup, gh request
handling and output parsing. On this host it was about 23 ms for a single page in
Shelbi's format, versus about 0.2 ms for the pooled control. Batching pages in one
gh invocation amortizes process/connection setup. The benchmark does not establish
that Shelbi will become 100x faster or predict the latency of a future Rust client.
Network latency, request count, cache freshness, queued work, and redundant readers
can dominate what users experience.

A daemon-owned direct client is a reasonable architectural direction because it can
retain connection pools across calls and expose response status, headers, structured
errors, deadlines and cancellation directly. This experiment supplies supporting
evidence for removing process overhead, not proof that transport migration alone
fixes the integration. Validate the actual Rust implementation against gh using the
same request shape, realistic RTT/TLS, and low-volume repository reads before claiming
an end-to-end improvement.

Observed gh 2.96.0 detail: raw REST --paginate output flattened ten arrays into one
valid JSON array in this non-TTY, no-headers case. The current Shelbi comment saying
pagination necessarily produces invalid concatenated JSON is not true for that
version/mode. Do not depend on this observation across CLI versions without testing.

Official source corroborates these observations:

- gh API builds one HTTP client before its pagination loop:
  [GitHub CLI pagination source](https://github.com/cli/cli/blob/v2.96.0/pkg/cmd/api/api.go#L366-L419)
- Raw REST pagination has a paginatedArrayReader path; --jq is evaluated per page:
  [GitHub CLI response formatting source](https://github.com/cli/cli/blob/v2.96.0/pkg/cmd/api/api.go#L454-L479)
- Auth is selected per request hostname:
  [GitHub CLI authentication source](https://github.com/cli/cli/blob/v2.96.0/api/http_client.go#L140-L158)

Earlier exploratory runs without a dummy enterprise token had keychain fallback
overhead and are excluded. results.json contains only the corrected successful run.

## Reproducible script

Script SHA-256: `5fd56293cf029bf0a8c5373e7453a0033bbcaa35aef4faac572d1ad32797dc6a`.

```python
#!/usr/bin/env python3
"""Loopback-only comparison of gh startup/HTTP vs a pooled HTTP client.

Run: python3 benchmark.py
No real credentials or GitHub requests are used. Requires gh on PATH.
Outputs results.json beside this file. Timings include JSON parsing by Python.
This isolates local overhead; it is NOT an estimate of GitHub production latency.
"""
import http.client
import json
import math
import os
from pathlib import Path
import platform
import socket
import statistics
import subprocess
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlparse

ROOT = Path(__file__).resolve().parent
PAYLOAD = [dict(number=n, title=f"Issue {n}: example work item", body="x" * 256,
                state="open", updated_at="2026-09-09T12:00:00Z", labels=[{"name": "bug"}],
                assignees=[{"login": "example"}], html_url=f"https://example.invalid/issues/{n}")
           for n in range(1, 101)]
BODY = json.dumps(PAYLOAD, separators=(",", ":")).encode()


class Server(ThreadingHTTPServer):
    daemon_threads = True
    block_on_close = False
    requests_seen = 0
    connections_seen = 0
    requests_without_dummy_auth = 0
    delay_s = 0

    def get_request(self):
        connection, address = super().get_request()
        connection.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.connections_seen += 1
        return connection, address


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_GET(self):
        self.server.requests_seen += 1
        if self.headers.get("Authorization") != "token dummy-loopback-token-only":
            self.server.requests_without_dummy_auth += 1
        query = parse_qs(urlparse(self.path).query)
        page = int(query.get("page", [1])[0])
        pages = int(query.get("pages", [1])[0])
        time.sleep(self.server.delay_s)
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(BODY)))
        if page < pages:
            self.send_header("Link", f'<http://127.0.0.1:{self.server.server_port}/issues?pages={pages}&page={page + 1}>; rel="next"')
        self.end_headers()
        self.wfile.write(BODY)
        self.wfile.flush()


def p95(values):
    return sorted(values)[math.ceil(len(values) * .95) - 1]


def main():
    server = Server(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_port}"
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("GH_", "GITHUB_")) and key.upper() not in {"HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"}}
    environment.update(GH_TOKEN="dummy-loopback-token-only", GH_ENTERPRISE_TOKEN="dummy-loopback-token-only", GH_CONFIG_DIR=str(ROOT / "gh-config"),
                       GH_PROMPT_DISABLED="1", NO_PROXY="127.0.0.1,localhost", LC_ALL="C")

    def run_gh(pages, mode):
        args = ["gh", "api", f"{url}/issues?pages={pages}"]
        if pages > 1:
            args.append("--paginate")
        if mode == "gh_slurp":
            args.append("--slurp")
        elif mode == "gh_jq_issues":
            args.extend(["--jq", ".[]"])
        result = subprocess.run(args, env=environment, cwd=ROOT, capture_output=True, check=True)
        # gh --paginate can return merged or concatenated JSON arrays depending
        # on version; --jq .[] returns concatenated objects. Consume all objects,
        # just like the
        # pooled client's response arrays. Do not time partial JSON parsing.
        source = result.stdout.decode()
        decoder = json.JSONDecoder()
        parsed = []
        position = 0
        while position < len(source):
            while position < len(source) and source[position].isspace():
                position += 1
            if position == len(source):
                break
            value, position = decoder.raw_decode(source, position)
            parsed.append(value)
        if mode == "gh_slurp":
            parsed = parsed[0]
        if mode == "gh_jq_issues":
            assert len(parsed) == pages * 100
        else:
            assert sum(len(page) for page in parsed) == pages * 100

    results = dict(timestamp_utc=time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                   platform=platform.platform(), python=platform.python_version(),
                   gh_version=subprocess.check_output(["gh", "--version"], text=True).splitlines()[0],
                   endpoint="loopback HTTP/1.1, no TLS", payload_bytes_per_page=len(BODY),
                   issues_per_page=100, percentiles="nearest rank, 1-based ceil(.95*n)",
                   cases=[])
    for delay_ms in [0, 25]:
        server.delay_s = delay_ms / 1000
        for pages, iterations in [(1, 60), (10, 16)]:
            modes = ["gh_raw", "gh_jq_issues"]
            if pages > 1:
                modes.append("gh_slurp")
            modes.append("python_http_client_pooled")
            for mode in modes:
                connection = http.client.HTTPConnection("127.0.0.1", server.server_port)

                def run_pooled():
                    parsed = []
                    for page in range(1, pages + 1):
                        connection.request("GET", f"/issues?pages={pages}&page={page}", headers={"Authorization": "token dummy-loopback-token-only"})
                        response = connection.getresponse()
                        assert response.status == 200
                        data = json.loads(response.read())
                        assert len(data) == 100
                        parsed.append(data)
                    assert len(parsed) == pages

                operation = (lambda: run_gh(pages, mode)) if mode.startswith("gh_") else run_pooled
                # Warm caches and sockets outside measured samples.
                for _ in range(3):
                    operation()
                before_requests = server.requests_seen
                before_connections = server.connections_seen
                samples = []
                for _ in range(iterations):
                    start = time.perf_counter_ns()
                    operation()
                    samples.append((time.perf_counter_ns() - start) / 1e6)
                case = dict(mode=mode, injected_server_delay_ms_per_request=delay_ms,
                            pages_per_operation=pages, iterations=iterations,
                            median_ms=round(statistics.median(samples), 3),
                            p95_ms=round(p95(samples), 3),
                            min_ms=round(min(samples), 3), max_ms=round(max(samples), 3),
                            requests=server.requests_seen - before_requests,
                            accepted_tcp_connections=server.connections_seen - before_connections,
                            samples_ms=[round(v, 3) for v in samples])
                results["cases"].append(case)
                assert server.requests_without_dummy_auth == 0, "A mock request missed dummy auth; do not publish timings with keychain fallback"
                print(json.dumps({k: v for k, v in case.items() if k != "samples_ms"}), flush=True)
                connection.close()
    results["total_server_requests_including_warmups"] = server.requests_seen
    results["requests_without_dummy_auth"] = server.requests_without_dummy_auth
    (ROOT / "results.json").write_text(json.dumps(results, indent=2) + "\n")
    server.shutdown()


if __name__ == "__main__":
    main()
```

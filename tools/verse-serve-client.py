#!/usr/bin/env python3
"""Ask a running `verse serve` to transcribe a file.

This exists to make a claim checkable rather than asserted: that the service is
usable from **any** language. It uses nothing but the standard library — no SDK,
no generated stubs, no HTTP client package — because the wire is plain HTTP/1.1
and JSON. If this file needs a dependency to work, the claim is false.

    python -I tools/verse-serve-client.py models/sensevoice/zh.wav

`-I` keeps the interpreter from importing anything from the current directory,
which matters when the directory is somebody else's.

The port and the token come from the discovery file the service writes. Pass
`--serve-json` if it is somewhere else, or `--port` and `--token` directly.
"""

import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request


def default_discovery() -> str:
    """Where the service writes what a client needs to reach it."""
    override = os.environ.get("VERSE_CACHE")
    if override:
        return os.path.join(override, "serve.json")
    if os.name == "nt":
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
        return os.path.join(base, "Verse", "serve.json")
    base = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
    return os.path.join(base, "verse", "serve.json")


def call(port: int, token: str, method: str, path: str, body=None):
    """One request. Returns (status, parsed JSON)."""
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=data,
        method=method,
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        },
    )

    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as failure:
        # The service answers refusals with a JSON body in the same shape as
        # everything else, so a client does not need a second code path.
        raw = failure.read()
        try:
            return failure.code, json.loads(raw)
        except ValueError:
            return failure.code, {"error": {"kind": "http", "message": raw.decode(errors="replace")}}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("input", help="an absolute path to the audio to transcribe")
    parser.add_argument("--serve-json", default=default_discovery())
    parser.add_argument("--port", type=int)
    parser.add_argument("--token")
    parser.add_argument("--engine", default=None)
    parser.add_argument("--format", default="srt", choices=["srt", "txt"])
    parser.add_argument("--timeout", type=float, default=600.0)
    args = parser.parse_args()

    port, token = args.port, args.token
    if port is None or token is None:
        try:
            with open(args.serve_json, encoding="utf-8") as handle:
                discovery = json.load(handle)
        except OSError as error:
            print(f"no discovery file at {args.serve_json}: {error}", file=sys.stderr)
            print("is `verse serve` running? point --serve-json at the file it wrote.", file=sys.stderr)
            return 2
        port = port or discovery["port"]
        token = token or discovery["token"]

    spec = {"input": os.path.abspath(args.input), "format": args.format}
    if args.engine:
        spec["engine"] = args.engine

    status, job = call(port, token, "POST", "/jobs", spec)
    if status != 202:
        print(json.dumps(job, indent=2, ensure_ascii=False), file=sys.stderr)
        return 1

    job_id = job["id"]
    print(f"job {job_id}: {job['state']}", file=sys.stderr)

    deadline = time.time() + args.timeout
    while time.time() < deadline:
        status, job = call(port, token, "GET", f"/jobs/{job_id}")
        if status != 200:
            print(json.dumps(job, indent=2, ensure_ascii=False), file=sys.stderr)
            return 1
        if job["state"] in ("done", "failed", "cancelled"):
            break
        progress = job.get("progress") or {}
        print(
            f"  {job['state']}: {progress.get('segments', 0)} segments, "
            f"{progress.get('positionMs', 0) / 1000:.1f}s in",
            file=sys.stderr,
        )
        time.sleep(0.5)
    else:
        print(f"job {job_id} did not finish within {args.timeout}s", file=sys.stderr)
        return 1

    print(json.dumps(job, indent=2, ensure_ascii=False))
    return 0 if job["state"] == "done" else 1


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Measure what a pool of workers is worth, against a running `verse serve`.

    python -I tools/measure-pool.py --workers 4 --jobs 16

Two numbers are printed, and neither alone is honest:

- **total wall time** for the measured batch — the end-to-end cost a client
  feels;
- **median per-job recognition time** — what the pool does to the work itself.

A bigger pool pays more model loads, and the warm-up is therefore done and
timed *separately*, before the batch that is measured. Nothing is dropped from
the measurement afterwards: dropping the slowest jobs would be a way of
manufacturing a result.

The instrument is checked before anything is read:

- `sum(loads)` from `/health` must equal the worker count, or some worker loaded
  twice and the workload is mis-sized;
- the model must be resident afterwards, or the warm-up did not work.

Run it through `--workers 1` twice before believing any other row.
"""

import argparse
import json
import os
import statistics
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor


def default_discovery() -> str:
    override = os.environ.get("VERSE_CACHE")
    if override:
        return os.path.join(override, "serve.json")
    if os.name == "nt":
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
        return os.path.join(base, "Verse", "serve.json")
    base = os.environ.get("XDG_DATA_HOME") or os.path.expanduser("~/.local/share")
    return os.path.join(base, "verse", "serve.json")


class Client:
    def __init__(self, port: int, token: str):
        self.port = port
        self.headers = {
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json",
        }

    def _call(self, method: str, path: str, body=None):
        data = json.dumps(body).encode() if body is not None else None
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.port}{path}",
            data=data,
            method=method,
            headers=self.headers,
        )
        try:
            with urllib.request.urlopen(request, timeout=120) as response:
                return response.status, json.loads(response.read() or b"null")
        except urllib.error.HTTPError as failure:
            return failure.code, json.loads(failure.read() or b"null")

    def submit(self, spec) -> int:
        status, job = self._call("POST", "/jobs", spec)
        if status != 202:
            raise SystemExit(f"submit refused: {status} {job}")
        return job["id"]

    def job(self, job_id: int) -> dict:
        _, job = self._call("GET", f"/jobs/{job_id}")
        return job

    def health(self) -> dict:
        _, health = self._call("GET", "/health")
        return health

    def wait_all(self, ids) -> None:
        """Poll until every one of them has settled."""
        remaining = set(ids)
        while remaining:
            for job_id in list(remaining):
                if self.job(job_id)["state"] in ("done", "failed", "cancelled"):
                    remaining.discard(job_id)
            if remaining:
                time.sleep(0.05)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("input", help="an absolute path to the audio to transcribe")
    parser.add_argument("--workers", type=int, required=True, help="the pool size to expect")
    parser.add_argument("--jobs", type=int, default=16, help="how many to measure")
    parser.add_argument("--serve-json", default=default_discovery())
    args = parser.parse_args()

    with open(args.serve_json, encoding="utf-8") as handle:
        discovery = json.load(handle)

    client = Client(discovery["port"], discovery["token"])
    # The cache off, always: a cache hit would be measuring the cache.
    spec = {"input": os.path.abspath(args.input), "noCache": True}

    # Warm every worker, and time it separately. After this no job in the
    # measured batch pays a model load, so the batch needs no corrections.
    warm_started = time.time()
    client.wait_all([client.submit(spec) for _ in range(args.workers)])
    warm_seconds = time.time() - warm_started

    health = client.health()
    loads = health["model"]["loads"]
    if loads != args.workers:
        print(
            f"instrument: BROKEN — {loads} loads for {args.workers} workers. "
            "Some worker loaded twice, or none did; the timings below mean nothing.",
            file=sys.stderr,
        )
    if health["model"]["residentKeepers"] < args.workers:
        print(
            f"instrument: only {health['model']['residentKeepers']} of "
            f"{args.workers} workers hold a model. Nothing below measures a pool.",
            file=sys.stderr,
        )

    # The measured batch, all at once.
    started = time.time()
    ids = [client.submit(spec) for _ in range(args.jobs)]
    client.wait_all(ids)
    wall_seconds = time.time() - started

    per_job = sorted(client.job(job_id)["elapsedMs"] for job_id in ids)
    median = statistics.median(per_job)

    print(
        json.dumps(
            {
                "workers": args.workers,
                "jobs": args.jobs,
                "loads": loads,
                "residentKeepers": health["model"]["residentKeepers"],
                "warmSeconds": round(warm_seconds, 2),
                "totalWallSeconds": round(wall_seconds, 2),
                "perJobMedianMs": median,
                "perJobMinMs": per_job[0],
                "perJobMaxMs": per_job[-1],
                "impliedPeakBytes": health["pool"]["impliedPeakBytes"],
            }
        )
    )
    return 0 if loads == args.workers else 1


if __name__ == "__main__":
    sys.exit(main())

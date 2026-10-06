# `verse serve`: a loopback service other programs can drive

The maintainer asked whether the backend was a service other applications could
use. It was not: a library plus two in-process consumers. The previous round
built the prerequisite — `ModelKeeper`, which owns a model across jobs. This
round builds the transport.

Taken with the maintainer: **HTTP on 127.0.0.1, hand-rolled**; **job-based**
(submit, then poll) because a one-hour file takes minutes and a synchronous call
could show no progress and could not be cancelled; **queued, one job at a time**,
because one keeper is one model is one job.

## Why a subcommand of `verse` and not a new crate

`crates/verse-cli` has **no `[lib]` target** — only `[[bin]] name = "verse"`. So
`report.rs`, which holds the pinned result wire and the contract tests that pin
it, is **structurally unreachable from another crate**. A separate `verse-serve`
crate would duplicate those DTOs — a third vocabulary, which is what this round
exists to avoid — or force `report.rs` into a library, which is churn on a frozen
format.

One vocabulary follows: a job's result **is** a `FileResult`, built by the
existing `FileResult::transcribed` / `failed`, so a transcript is described in
the same JSON `verse transcribe --json` emits.

## The constraint this collides with, and the amendment

`design.md` §4.5 says all network access lives in `verse-model`, and
`verse-model/src/lib.rs:3-4` says it is the only crate permitted to perform
network I/O. The distinction that keeps the guarantee intact:

> **An outbound connection is a request to a server; an inbound one is a request
> from a local peer.** The first is what "offline" forbids, and only
> `verse-model` may make it. A listener accepts connections and initiates none.

The socket and every document that restates the rule move in one commit,
because the constraint is explicit.

---

## [x] 1. Amend the boundary, and stand up the listener

`serve/{mod,http,router}.rs`; health DTOs in `report.rs`; `httparse` in the
manifest with its reason; the document edits; `serve` in `main.rs`.

**Verify:** parser unit tests — Content-Length body; chunked → 501; both headers
→ 400; oversized headers → 431; `PUT` → 405 with `Allow`; a garbage request line
→ 400; `Expect: 100-continue` gets an interim; `Connection: close` closes. An
integration test spawning `CARGO_BIN_EXE_verse` that reads the real discovery
file: no token → 401, token → 200, and two requests on one socket. A `grep` for
every restatement of the offline rule, showing only the amended text.

**Done.** 14 parser tests, 7 router tests, 6 integration tests. The integration
tests spawn the real binary, read the real discovery file and speak to a real
socket with a hand-written `TcpStream` — so what is verified is the server and
not a client library's idea of it.

Over the wire, on a running process:

```
no token 401 · wrong token 401 · right token 200
Origin: http://evil.test   403
unknown route 404 · PUT 405   chunked 501 · 1.1 MB body 413
garbage request line 400      three requests, one connection
```

**`Expect: 100-continue` got a 404, and that is the point.** `/jobs` does not
exist yet, so 404 is the right answer — but the request was *routed*, which
means the interim `100` was sent and the body was read. Before that fix the
server read for a body curl was still waiting to send, and the client hung.

**Two things httparse settled that reading could not.** It refuses `HTTP/2.0`
at parse time, so the `version > 1` guard is belt-and-braces rather than the
mechanism. And it gives the *same* error for `HTTP/2.0` as for a request line
whose version field is nonsense — so the first attempt's 505 was a
precise-sounding status the parser cannot justify, and it is 400 now.

**One bug of my own, found by running it.** The subcommand's argument loop was
missing the `i += 1` that every other parser in this file ends with, because
`take_value` stops *on* the value rather than past it. `serve --port 0` reported
"unknown option 0" — which is at least a loud way to be wrong.

The `grep` for every restatement of the offline rule returns only amended text.

## [ ] 2. Jobs: submit, poll, list, cancel

`serve/jobs.rs` with the state, the worker and a `Runner` seam so the lifecycle
is testable without weights — the same shape as `keep::Loader`.

**Verify:** with a fake runner — `202` + `Location`; `queued → running → done`;
`result` carries all fourteen `FileResult` keys; cancel-while-queued settles at
once; cancel-while-running settles only when the runner returns; two submits
never overlap; the 33rd queued submit gets `429`. With the real keeper and an
empty models directory, a job lands `failed` with `kind: "model"`. With the real
model, a job on the sample clip reaches `done`.

## [ ] 3. Progress, retention, connection cap

One long-lived drain thread; FIFO eviction at `MAX_RETAINED`; the connection cap.

**Verify:** `progress` rises during a job and carries **no** percentage;
`MAX_RETAINED + 5` jobs leaves the count pinned, the oldest id `404` naming the
bound; the 65th socket gets `503`.

## [ ] 4. Models, and both no-model paths made visible

`GET /models`; a submit-time refusal that names `verse model fetch`.

**Verify:** an empty models directory refuses at submit; a model present at the
declared size but unusable is *accepted* and lands `failed` with
`kind: "model"` — the size-only check not failing silently; `GET /models` matches
`verse model list --json` for the same directory.

## [ ] 5. The "any language" proof, and the documents

`tools/verse-serve-client.py`; the task log; `design.md` §4.11; `README.md`;
`llms.txt`; the changelog.

**Verify:** the same job driven from **Python, PowerShell and Rust**, with the
returned JSON pasted for each — three languages, one wire.

---

## Not in this round

- **A pool of keepers.** Deferred with the cost named: each is a resident model.
- **An `output` field**, so the service writes a transcript to disk. The JSON is
  adequate, and doing it properly means the collision rules and the ownership
  record.
- **Preload / release routes.** The keeper has them; exposing them is a decision
  about who may spend a gigabyte, not a transport question.
- **MCP.** A thin wrapper over this, and wrapping an unfinished surface means
  rewriting the wrapper.

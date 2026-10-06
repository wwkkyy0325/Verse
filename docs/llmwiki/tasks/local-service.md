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

## [x] 2. Jobs: submit, poll, list, cancel

`serve/jobs.rs` with the state, the worker and a `Runner` seam so the lifecycle
is testable without weights — the same shape as `keep::Loader`.

**Verify:** with a fake runner — `202` + `Location`; `queued → running → done`;
`result` carries all fourteen `FileResult` keys; cancel-while-queued settles at
once; cancel-while-running settles only when the runner returns; two submits
never overlap; the 33rd queued submit gets `429`. With the real keeper and an
empty models directory, a job lands `failed` with `kind: "model"`. With the real
model, a job on the sample clip reaches `done`.

**Done**, together with the progress drain and `GET /models` — the plan put the
drain in step 3 and `/models` in step 4, but leaving a `progress` field that
always reads zero and a `models` endpoint a client would obviously want is worse
than moving them. Retention stays in step 3.

Driven from Python against the real model, on a real socket:

```
job 1: queued → running → done
  transcript        "开放时间早上9点至下午5点。"
  segmentCount      1
  submittedAtMs 2125 · startedAtMs 2125 · finishedAtMs 3683 · elapsedMs 1558

12 jobs submitted → 11 queued, 1 running
  DELETE the queued one   200 cancelled
  DELETE it again         409   DELETE a stranger  404
  at the end              0 queued, not running, 12 retained

no model installed → 409 kind "model", message naming `verse model fetch`
```

**Three bugs, all found by running it.**

One: `submittedAtMs` was measured from the job's own start and `finishedAtMs`
from the worker's, so a job reported `submittedAtMs: 1604` and
`finishedAtMs: 5`. Three timestamps from three origins are not timestamps. They
now share the service's start, and `elapsedMs == finished − started` exactly.

Two, and this one was my own documentation overclaiming: I wrote that
`elapsedMs` is "recognition time only, comparable with the CLI's report". It is
— for every job except the one that pays the model load. Measured: the first job
at 1558 ms, then 272 ms cold and 242 ms warm for the same clip against the CLI's
307 ms. The doc comment now says which job is the slow one instead of implying
none is.

Three: `GET /models` was missing, so the no-model test could not check the
vocabulary. It reuses `report::ModelList` and `ModelEntry` — the same structs
`verse model list --json` builds — so the two cannot drift.

**And two tests of mine were racy.** Both asserted on the queue length without
first pinning the worker, so whether the first job had been picked up was
scheduling rather than fact. Both now wait for it to be running. The suite was
run three times to check.

The transcript arriving as mojibake in one run was the console's cp936
rendering, not the wire: over HTTP the text is exactly the expected Chinese.

## [x] 3. Progress, retention, connection cap

One long-lived drain thread; FIFO eviction at `MAX_RETAINED`; the connection cap.

**Verify:** `progress` rises during a job and carries **no** percentage;
`MAX_RETAINED + 5` jobs leaves the count pinned, the oldest id `404` naming the
bound; the 65th socket gets `503`.

**Done.** Verified against a running service, 45 jobs in one session:

```
after 15 jobs: retained=32/32  uptime=15s
after 30 jobs: retained=32/32  uptime=16s
after 45 jobs: retained=32/32  uptime=16s
the first job is now: 404, "Finished jobs are kept for the most recent 32;
                      an evicted one reads the same as one that never existed."
```

The count is pinned while uptime grows, which is the shape of memory bounded by
constants rather than by time. A unit test asserts the same without a socket.

The connection cap, measured by sending a request on each of 71 sockets: 65
served, six refused with `503`, and closing one made the next succeed
immediately. Five served rather than exactly 64 because connections idle out
during the loop — the cap counts *live* connections, and the guard that returns
a slot is the same one that refuses.

**A correction to my own first attempt at that test.** It opened 71 sockets and
expected refusals, and got none: TCP accepts a connection long before the server
sees it, so the cap can only be observed by *sending* something. The refusal is a
response, not a refused handshake — worth knowing for anyone probing this.

## [x] 4. Models, and both no-model paths made visible

`GET /models`; a submit-time refusal that names `verse model fetch`.

**Verify:** an empty models directory refuses at submit; a model present at the
declared size but unusable is *accepted* and lands `failed` with
`kind: "model"` — the size-only check not failing silently; `GET /models` matches
`verse model list --json` for the same directory.

**Done, and the second half of that verification is wrong — in a way worth
recording, because the plan assumed something the library does not do.**

`GET /models` reuses `report::ModelList` and `ModelEntry`, the same structs
`verse model list --json` builds, so the two cannot drift. With no model
installed, `/health` reports `present: false`, a submit is refused `409` with
`kind: "model"`, and the message names `verse model fetch sensevoice` — checked
end to end.

**But a model that is present and unusable does not land as a failed job. It
takes the process down.** Planted a decoy: a catalogue declaring sizes that two
files of garbage actually have, so the size-only check passes. Measured:

```
health says present: True      <- size only, and it matches
submit accepted   : 202        <- the check cannot tell
sherpa-onnx: symbol-table.cc:ReadTokens:132 Error: not tokens
the server is gone
```

The library **terminates the process** rather than returning an error — the CLI
exits 127 on the same input, which is not one of its documented codes. So the
`Err` that `ModelKeeper` is built to announce never happens, and the design of
step 2, "a load failure announces its job before announcing itself", does not
apply to this case: there is no failure to announce.

This is **not new** — the command line has always had it, and a corrupt model
has always ended that process. What is new is that the process is now a service
other programs depend on, so the blast radius is a running server rather than a
finished command.

It cannot be fixed under this project's rules. Catching a C library calling
`exit` needs signal handling or `unsafe`, both of which are excluded; running
each recognition in a subprocess would kill the keeper, which exists precisely
to hold the model in-process. What can be done — and is not done here — is
making `is_present` mean something stronger than a size, so a corrupt download is
refused before anything loads it. That needs a way to validate a model without
loading it.

Recorded rather than papered over. The common case — no model downloaded at all —
is handled cleanly at submit, and that is the case a user meets.

## [x] 5. The "any language" proof, and the documents

`tools/verse-serve-client.py`; the task log; `design.md` §4.11; `README.md`;
`llms.txt`; the changelog.

**Verify:** the same job driven from **Python, PowerShell and Rust**, with the
returned JSON pasted for each — three languages, one wire.

**Done.** Three languages, the same transcript, and none of them using an SDK:

| | |
|---|---|
| Python | stdlib `urllib` only — `tools/verse-serve-client.py` |
| PowerShell | `Invoke-RestMethod`, **comparing by codepoint** so the console's encoding is not what is under test. Result: `text matches Python's: True` |
| Rust | an integration test that spawns the real binary and drives a real socket |

All three: `开放时间早上9点至下午5点。`, one segment.

The PowerShell comparison is by codepoint on purpose. Printing the Chinese
through a pipe renders as mojibake on this machine's cp936 console — which
happened twice during this round and looks exactly like a bug in the service.
It is not: over HTTP the bytes are the expected UTF-8, checked separately. Worth
recording because the next person to see that output will believe it.

Documents moved with the code: `design.md` §4.11, a note in `ui-design.md` §6.1
that 17322 is the service port and not the dev server's 17321, a README section,
and the routes in `llms.txt`.

## What is not verified

The window, which this round did not touch.

**A corrupt model**, which is recorded rather than fixed: see step 4. The
service can be ended by a malformed model file, and nothing under this project's
rules can catch it.

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

//! `verse serve` — the pipeline behind a loopback socket.
//!
//! Other programs on this machine can ask this process to transcribe a file and
//! poll for the result. Nothing leaves the machine: the listener binds
//! `127.0.0.1` and only that, it initiates no connection, and it is bound only
//! while a person is running this command. `design.md` §4.5 carries the
//! argument for why a listener does not weaken the offline guarantee.
//!
//! **Why this is a subcommand and not its own crate.** `crates/verse-cli` has no
//! `[lib]` target, so `report.rs` — the pinned result wire and the contract
//! tests that pin it — is structurally unreachable from another crate. A
//! separate binary would have to describe a transcript a third time. This way a
//! job's result *is* the [`crate::report::FileResult`] that `verse transcribe
//! --json` already emits.

mod http;
mod jobs;
mod router;

use std::io::Write;
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use verse_core::EventBus;
use verse_pipeline::ModelKeeper;

use http::{HeadError, Response};

/// What the memory budget is, in words a client can act on.
///
/// It is a written assumption rather than a measurement, because reading the
/// machine's RAM needs FFI and `unsafe` on every platform this project targets
/// and the project has neither.
pub const BUDGET_ASSUMPTION: &str =
    "assumed, not probed: a quarter of the 8 GB machine floor design.md states";

/// The port `serve` uses unless told otherwise.
///
/// Chosen by the rule in `ui-design.md` §6.1: above 1024 so it needs no
/// privileges, below 49152 so it is outside the range Windows hands out, and
/// not a default of anything likely to be running. 17321 is the window's dev
/// server, so this is the one next to it rather than on top of it.
pub const DEFAULT_PORT: u16 = 17322;

/// How many connections may be open at once.
///
/// Bounded because this process is meant to run for days: an unbounded accept
/// loop is an unbounded number of threads. Sixty-four is far above what a
/// polling client needs and far below what would exhaust a desktop.
const MAX_CONNECTIONS: usize = 64;

/// What a client needs to reach this service.
///
/// Written to the per-user data directory, beside the cache. It is a **hint and
/// never a guarantee**: there is no signal handling available under this
/// project's no-`unsafe` rule, so a killed process leaves it behind, and a
/// client that finds a file and cannot connect has learned that the service is
/// not running.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    pub version: u32,
    pub host: String,
    pub port: u16,
    pub token: String,
    pub pid: u32,
    /// Identifies this run. A client that reconnects to a port it read from a
    /// stale file compares this against `/health` to tell whose answer it got.
    pub instance: String,
    pub started_at_ms: u64,
    pub models_dir: String,
}

/// What `serve` was asked to do.
pub struct Config {
    /// `0` asks the operating system for a free port.
    pub port: u16,
    pub models_dir: std::path::PathBuf,
    pub engine: String,
    /// How long the model is kept after the last job. `None` holds it.
    pub idle: Option<Duration>,
    /// How many workers. `None` means one until the adaptive rule is wired —
    /// the default is deliberately left where it was while the pool is built.
    pub workers: Option<usize>,
}

impl Config {
    /// The idle timeout from the environment, by the same rule the window uses.
    pub fn idle_from_env() -> Option<Duration> {
        let Some(raw) = std::env::var_os("VERSE_MODEL_IDLE_SECS") else {
            return Some(verse_pipeline::DEFAULT_IDLE);
        };
        if raw.is_empty() {
            return Some(verse_pipeline::DEFAULT_IDLE);
        }

        let text = raw.to_string_lossy().into_owned();
        match text.trim().parse::<u64>() {
            Ok(0) => None,
            Ok(seconds) => Some(Duration::from_secs(seconds)),
            Err(_) => {
                eprintln!(
                    "warning: VERSE_MODEL_IDLE_SECS={text:?} is not a number of seconds; \
                     using {}",
                    verse_pipeline::DEFAULT_IDLE.as_secs()
                );
                Some(verse_pipeline::DEFAULT_IDLE)
            }
        }
    }
}

/// What every request handler is given.
pub struct Server {
    /// One per worker. Aggregated for `/health` rather than reported one by
    /// one: a client cares what the pool costs, not which worker is warm.
    pub keepers: Vec<Arc<ModelKeeper>>,
    pub jobs: jobs::Jobs,
    pub workers: usize,
    /// Why the pool is that size, so a client can tell a decision from a
    /// default.
    pub worker_source: String,
    /// What each worker asks the machine for. Set once, so every worker binds
    /// the same model identity.
    pub per_worker_threads: usize,
    /// The weights one worker loads, from the catalogue. Zero when the engine
    /// is not in the catalogue at all — which makes the pool one, rather than
    /// guessing what it costs.
    pub model_bytes: u64,
    pub models_dir: std::path::PathBuf,
    pub engine: String,
    pub token: String,
    pub instance: String,
    pub started: Instant,
    pub started_at_ms: u64,
}

impl Server {
    /// Refuse a request that does not carry this run's token.
    ///
    /// A bearer token, and worth being precise about what it is worth: it stops
    /// a **browser** — which cannot read the discovery file, and whose
    /// cross-origin requests are preflighted and never answered — and it stops
    /// another **user** on the machine. It does **not** stop a process already
    /// running as this user, which can read the same file. The boundary there is
    /// the operating system's user boundary, not this string.
    fn authorised(&self, head: &http::Head) -> bool {
        match head.header("authorization") {
            Some(value) => value
                .strip_prefix("Bearer ")
                .is_some_and(|given| constant_eq(given, &self.token)),
            None => false,
        }
    }
}

/// Compare without an early return, so the time taken does not say how much of
/// the token was right.
fn constant_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Run until killed.
pub fn run(config: Config) -> Result<(), String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port)).map_err(|error| {
        // Loudly, rather than sliding to another port. A service that quietly
        // moved would leave its client looking at the one it was told about —
        // the failure the dev server already taught this project.
        format!(
            "could not listen on 127.0.0.1:{}: {error}\n\
             Something else is using that port. Choose another with --port.",
            config.port
        )
    })?;

    let port = listener
        .local_addr()
        .map_err(|error| format!("could not read the bound address: {error}"))?
        .port();

    // A previous run that was killed rather than closed can leave a half-written
    // cache entry behind. Swept here rather than in the cache itself, because
    // the cache is used by commands that finish and the litter only matters to
    // the one thing that does not.
    let data_dir = verse_store::data_dir(&verse_store::Roots::from_env());
    let swept = verse_store::Cache::under(&data_dir).sweep_temporaries();
    if swept > 0 {
        eprintln!("cleared {swept} half-written cache files from a previous run");
    }

    let bus = EventBus::new();

    // **One thread budget for the whole pool**, decided here rather than per
    // worker, and it is an invariant rather than a tidiness. The settings
    // digest includes the resolved thread count, so workers with different
    // budgets would hold *different model identities* — and a job's request
    // would then be runnable on some workers and not others. One budget keeps
    // dispatch FIFO and id-keyed, which is what the queue assumes.
    let hardware = verse_core::HardwareProfile::probe();
    let workers = config.workers.unwrap_or(1);
    let per_worker_threads =
        verse_core::per_worker_threads(hardware.engine_threads(), workers);

    let keepers: Vec<Arc<ModelKeeper>> = (0..workers)
        .map(|_| Arc::new(ModelKeeper::with_timeout(bus.clone(), config.idle)))
        .collect();

    let server = Arc::new(Server {
        jobs: jobs::Jobs::new(keepers.clone(), bus.clone(), None),
        keepers,
        workers,
        worker_source: if config.workers.is_some() {
            "flag".to_string()
        } else {
            "default".to_string()
        },
        per_worker_threads,
        model_bytes: model_bytes(&config.models_dir, &config.engine),
        models_dir: config.models_dir.clone(),
        engine: config.engine.clone(),
        token: new_token(),
        instance: new_token()[..16].to_string(),
        started: Instant::now(),
        started_at_ms: now_ms(),
    });

    let discovery = Discovery {
        version: crate::report::VERSION,
        host: "127.0.0.1".to_string(),
        port,
        token: server.token.clone(),
        pid: std::process::id(),
        instance: server.instance.clone(),
        started_at_ms: server.started_at_ms,
        models_dir: config.models_dir.display().to_string(),
    };
    let path = discovery_path();
    write_discovery(&path, &discovery)?;

    eprintln!("verse serve listening on http://127.0.0.1:{port}");
    eprintln!("  token and port: {}", path.display());
    eprintln!("  models: {}", config.models_dir.display());
    eprintln!("  press Ctrl-C to stop");

    accept_loop(&listener, server)
}

fn accept_loop(listener: &TcpListener, server: Arc<Server>) -> Result<(), String> {
    let live = Arc::new(AtomicUsize::new(0));

    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            // One failed accept is not a reason to stop serving.
            Err(_) => continue,
        };

        // Claimed before the thread is spawned, so the cap counts connections
        // rather than threads that have not started yet. The guard releases it
        // when the thread ends, whatever way it ends.
        if live.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
            live.fetch_sub(1, Ordering::Relaxed);
            let _ = refuse_busy(stream);
            continue;
        }

        let server = Arc::clone(&server);
        let live = Arc::clone(&live);
        std::thread::spawn(move || {
            let _guard = ConnectionGuard(live);
            let _ = serve_connection(stream, &server);
        });
    }

    Ok(())
}

/// Releases a connection slot however the thread ends.
struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Answer a connection that arrived over the cap.
fn refuse_busy(mut stream: TcpStream) -> std::io::Result<()> {
    let response = Response::json(
        503,
        r#"{"error":{"kind":"tooManyRequests","message":"too many open connections"}}"#
            .to_string(),
    )
    .with("Retry-After", "1");
    http::write_response(&mut stream, &response, true)
}

/// One connection, for as many requests as the client keeps sending.
fn serve_connection(mut stream: TcpStream, server: &Server) -> std::io::Result<()> {
    stream.set_read_timeout(Some(http::READ_TIMEOUT)).ok();
    stream.set_write_timeout(Some(http::READ_TIMEOUT)).ok();

    loop {
        // The idle timeout applies only between requests. A client that has
        // sent nothing for ten seconds is gone; one that is midway through
        // sending gets the longer read timeout above.
        stream
            .set_read_timeout(Some(http::IDLE_TIMEOUT))
            .ok();

        let (head, body) = match http::read_request(&mut stream) {
            Ok(Some(request)) => request,
            // The client closed between requests, which is ordinary.
            Ok(None) => return Ok(()),
            Err(HeadError::Refuse(status, message)) => {
                let response = Response::json(status, error_body(status, message));
                return http::write_response(&mut stream, &response, true);
            }
            Err(HeadError::Incomplete) => return Ok(()),
        };

        // A client waiting for `100 Continue` before sending its body cannot be
        // answered until it has sent it, and refusing the header instead is the
        // kind of thing that works until someone uses curl with a large body.
        // `read_request` has already read the body by this point, which is
        // correct for the sizes this API accepts.
        let close = head.wants_close();
        let response = router::route(server, &head, &body);

        http::write_response(&mut stream, &response, close)?;
        if close {
            return Ok(());
        }
    }
}

/// The body of a refusal, in the same shape as every other error.
///
/// Written by hand rather than with `serde_json` because it is produced for
/// requests that may have been refused before anything else understood them —
/// including a malformed head — and this must not be able to fail.
fn error_body(status: u16, message: &str) -> String {
    let kind = match status {
        401 => "unauthorized",
        403 => "forbidden",
        404 => "notFound",
        405 => "unsupported",
        408 => "badRequest",
        413 => "payloadTooLarge",
        429 => "tooManyRequests",
        431 => "badRequest",
        501 => "unsupported",
        503 => "tooManyRequests",
        505 => "unsupported",
        _ => "badRequest",
    };
    format!(
        r#"{{"error":{{"kind":"{kind}","message":{}}}}}"#,
        serde_json::Value::String(message.to_string())
    )
}

/// What one worker's weights cost, from the catalogue.
///
/// Zero when the engine is not in the catalogue — which the sizing rule reads
/// as "unknown", and answers with one worker rather than a guess.
fn model_bytes(models_dir: &std::path::Path, engine: &str) -> u64 {
    use verse_model::Catalog;

    Catalog::load_or_embedded(&models_dir.join("catalog.json"))
        .ok()
        .and_then(|catalog| {
            catalog
                .find(engine)
                .map(|spec| spec.files.iter().filter_map(|file| file.size).sum())
        })
        .unwrap_or(0)
}

/// Where the discovery file lives.
pub fn discovery_path() -> std::path::PathBuf {
    verse_store::data_dir(&verse_store::Roots::from_env()).join("serve.json")
}

/// Write it where a client will look, atomically.
///
/// Through a temporary name and a rename, the same way the output record is
/// written: a client that reads a half-written file would see a port or a token
/// that is not the real one, and the failure would look like a wrong token.
fn write_discovery(path: &std::path::Path, discovery: &Discovery) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }

    let text = serde_json::to_string_pretty(discovery)
        .map_err(|error| format!("could not serialise the discovery file: {error}"))?;

    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;

    // Owner-only where the filesystem has the concept. On Windows `std` cannot
    // set an ACL without `unsafe`, so the per-user directory's own permissions
    // are the boundary — said plainly rather than implied to be tighter.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
    }

    std::fs::rename(&temporary, path)
        .map_err(|error| format!("could not place {}: {error}", path.display()))?;

    if let Ok(mut out) = std::fs::OpenOptions::new().append(true).open(path) {
        let _ = out.flush();
    }
    Ok(())
}

/// A bearer token, from the only randomness the standard library offers.
///
/// `RandomState` is seeded by the operating system and is not a documented
/// cryptographic contract — stated rather than implied. It is enough for a
/// loopback listener whose threat model is a browser and another user, and if
/// it ever needs to be stronger, the honest place to get that is a dependency
/// chosen on purpose.
fn new_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut out = String::with_capacity(64);
    for round in 0..4u64 {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u64(round);
        hasher.write_u64(now_ms());
        hasher.write_u32(std::process::id());
        out.push_str(&format!("{:016x}", hasher.finish()));
    }
    out
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis() as u64)
        .unwrap_or(0)
}

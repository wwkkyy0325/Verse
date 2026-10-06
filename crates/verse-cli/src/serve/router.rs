//! Which request goes where.
//!
//! Small enough to read in one sitting, which is the point: the whole surface
//! is half a dozen routes on a loopback socket, and a routing framework would
//! be more machinery than the thing it routes.

use verse_model::{Catalog, Downloader};

use super::http::{Head, Response};
use super::Server;
use crate::report;

/// Answer one request.
///
/// Authentication first, so nothing below has to think about it, and so an
/// unauthorised request cannot learn anything from the difference between one
/// route and another.
pub fn route(server: &Server, head: &Head, _body: &[u8]) -> Response {
    if let Some(origin) = head.header("origin") {
        // Our clients are scripts and send no Origin. A request that carries
        // one came from a web page, and no page has any business driving a
        // service that reads the user's files.
        return refusal(403, &format!("this service does not answer browser requests (Origin: {origin})"));
    }

    if !server.authorised(head) {
        return Response::json(401, error_body(401, "missing or wrong token"))
            .with("WWW-Authenticate", "Bearer");
    }

    match (head.method.as_str(), head.path.as_str()) {
        ("GET", "/health") => health(server),
        ("GET", _) | ("POST", _) | ("DELETE", _) => {
            refusal(404, &format!("no route for {} {}", head.method, head.path))
        }
        (method, _) => Response::json(
            405,
            error_body(405, &format!("{method} is not supported")),
        )
        .with("Allow", "GET, POST, DELETE"),
    }
}

/// What the service is and what it is doing.
fn health(server: &Server) -> Response {
    let models = Catalog::load_or_embedded(&server.models_dir.join("catalog.json"));
    let present = models
        .ok()
        .and_then(|catalog| {
            catalog
                .find(&server.engine)
                .map(|spec| Downloader::is_present(spec, &server.models_dir))
        })
        .unwrap_or(false);

    let view = report::Health {
        version: report::VERSION,
        ok: true,
        service: report::ServiceInfo {
            instance: server.instance.clone(),
            pid: std::process::id(),
            started_at_ms: server.started_at_ms,
            uptime_ms: server.started.elapsed().as_millis() as u64,
        },
        model: report::ModelInfo {
            engine: server.engine.clone(),
            // Size only, and named so: a model present at the expected size but
            // unusable passes this, which is why the job is what reports it.
            present,
            status: status_name(server.keeper.status()).to_string(),
            loads: server.keeper.loads(),
        },
        models_dir: server.models_dir.display().to_string(),
    };

    json(200, &view)
}

fn status_name(status: verse_pipeline::ModelStatus) -> &'static str {
    match status {
        verse_pipeline::ModelStatus::Unloaded => "unloaded",
        verse_pipeline::ModelStatus::Loading => "loading",
        verse_pipeline::ModelStatus::Standby => "standby",
        verse_pipeline::ModelStatus::Busy => "busy",
    }
}

/// Serialise a report DTO, falling back to a hand-built error if that fails.
///
/// A serialisation failure here would be a bug, and answering nothing at all is
/// the one response a client cannot act on.
pub(super) fn json<T: serde::Serialize>(status: u16, value: &T) -> Response {
    match serde_json::to_string(value) {
        Ok(text) => Response::json(status, text),
        Err(error) => refusal(500, &format!("could not serialise the answer: {error}")),
    }
}

pub(super) fn refusal(status: u16, message: &str) -> Response {
    Response::json(status, error_body(status, message))
}

/// An error body in the same shape everything else uses.
fn error_body(status: u16, message: &str) -> String {
    let kind = match status {
        401 => "unauthorized",
        403 => "forbidden",
        404 => "notFound",
        405 => "unsupported",
        409 => "conflict",
        413 => "payloadTooLarge",
        429 => "tooManyRequests",
        501 => "unsupported",
        503 => "tooManyRequests",
        _ => "badRequest",
    };

    let value = serde_json::json!({ "error": { "kind": kind, "message": message } });
    value.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::serve::constant_eq;
    use std::time::Instant;
    use verse_core::EventBus;
    use verse_pipeline::ModelKeeper;

    fn a_server(dir: &std::path::Path) -> Server {
        Server {
            keeper: ModelKeeper::with_timeout(EventBus::new(), None),
            models_dir: dir.to_path_buf(),
            engine: "sensevoice".to_string(),
            token: "a".repeat(64),
            instance: "0123456789abcdef".to_string(),
            started: Instant::now(),
            started_at_ms: 1_791_000_000_000,
        }
    }

    fn get(path: &str, token: Option<&str>) -> Head {
        let mut text = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n");
        if let Some(token) = token {
            text.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        text.push_str("\r\n");
        crate::serve::http::parse_head(text.as_bytes()).expect("parses")
    }

    fn body(response: &Response) -> serde_json::Value {
        serde_json::from_slice(&response.body).expect("json")
    }

    #[test]
    fn health_names_its_version_and_every_field_a_client_needs() {
        // The same discipline the CLI's report holds itself to: a client should
        // not have to branch on a key existing.
        let dir = std::env::temp_dir().join("verse-serve-test-health");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");

        let server = a_server(&dir);
        let response = route(&server, &get("/health", Some(&server.token)), &[]);

        assert_eq!(response.status, 200);
        let value = body(&response);

        assert_eq!(value["version"], serde_json::json!(crate::report::VERSION));
        for key in ["version", "ok", "service", "model", "modelsDir"] {
            assert!(value.get(key).is_some(), "missing key: {key}");
        }
        for key in ["instance", "pid", "startedAtMs", "uptimeMs"] {
            assert!(value["service"].get(key).is_some(), "missing service.{key}");
        }
        for key in ["engine", "present", "status", "loads"] {
            assert!(value["model"].get(key).is_some(), "missing model.{key}");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_absent_model_is_reported_rather_than_guessed_at() {
        let dir = std::env::temp_dir().join("verse-serve-test-absent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");

        let server = a_server(&dir);
        let value = body(&route(&server, &get("/health", Some(&server.token)), &[]));

        assert_eq!(value["model"]["present"], serde_json::json!(false));
        assert_eq!(value["model"]["loads"], serde_json::json!(0));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_request_without_a_token_is_refused() {
        let server = a_server(&std::env::temp_dir());
        let response = route(&server, &get("/health", None), &[]);

        assert_eq!(response.status, 401);
        assert!(response
            .headers
            .iter()
            .any(|(name, _)| *name == "WWW-Authenticate"));
    }

    #[test]
    fn a_request_with_the_wrong_token_is_refused() {
        let server = a_server(&std::env::temp_dir());
        assert_eq!(route(&server, &get("/health", Some("wrong")), &[]).status, 401);
    }

    #[test]
    fn a_browser_request_is_refused_even_with_the_token() {
        // The defence that actually matters. A page cannot read the discovery
        // file, so it cannot have the token — but if it somehow did, this is
        // the second line, and it does not depend on the token at all.
        let server = a_server(&std::env::temp_dir());
        let text = format!(
            "GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nOrigin: http://evil.test\r\n\r\n",
            server.token
        );
        let head = crate::serve::http::parse_head(text.as_bytes()).expect("parses");

        let response = route(&server, &head, &[]);
        assert_eq!(response.status, 403);
        assert!(body(&response)["error"]["message"]
            .as_str()
            .expect("a message")
            .contains("browser"));
    }

    #[test]
    fn an_unknown_route_is_a_not_found_and_a_bad_method_is_a_method_error() {
        let server = a_server(&std::env::temp_dir());

        let missing = route(&server, &get("/nope", Some(&server.token)), &[]);
        assert_eq!(missing.status, 404);

        let text = format!(
            "PUT /health HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\n\r\n",
            server.token
        );
        let head = crate::serve::http::parse_head(text.as_bytes()).expect("parses");
        let response = route(&server, &head, &[]);

        assert_eq!(response.status, 405);
        assert!(response.headers.iter().any(|(name, _)| *name == "Allow"));
    }

    #[test]
    fn the_token_comparison_is_not_an_early_return() {
        // Lengths differ, bytes differ, and a prefix is not enough.
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
        assert!(!constant_eq("abc", "ab"));
        assert!(!constant_eq("", "a"));
    }
}

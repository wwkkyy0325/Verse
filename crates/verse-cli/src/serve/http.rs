//! The HTTP this service speaks, and the HTTP it deliberately does not.
//!
//! Hand-rolled, because the surface is four routes on loopback and a server
//! framework would be a dependency for a fraction of what this already needs.
//! `httparse` does the one genuinely fiddly part — the incremental parse of a
//! request line and headers across reads — and it is already in this
//! workspace's build graph through Tauri's hyper stack, so it costs a manifest
//! line rather than a crate.
//!
//! **What is not supported, and why that is safe here.** No chunked transfer
//! encoding: a second framing is a second thing to get wrong, and a loopback
//! client always knows its body length. No HTTP/2, no upgrades, no ranges, no
//! compression, no CORS — nothing multiplexes, and there is no browser to
//! serve. Every one of those is refused explicitly rather than mis-parsed.
//!
//! Every buffer is capped by a constant. This process is meant to run for days
//! (`design.md` §3, C2), so nothing here may grow with what a client sends.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// A request line longer than this is refused rather than buffered.
pub const MAX_REQUEST_LINE: usize = 8 * 1024;
/// Headers beyond this are refused.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// And beyond this many of them.
pub const MAX_HEADERS: usize = 64;
/// Bodies beyond this are refused. A job specification is a few hundred bytes;
/// a megabyte is three orders of magnitude of headroom.
pub const MAX_BODY: usize = 1024 * 1024;
/// How long a connection may sit between requests before it is closed.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a client may take to finish sending a request it has begun.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// A parsed request head — everything before the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub method: String,
    pub path: String,
    /// `0` for HTTP/1.0, `1` for HTTP/1.1 — `httparse`'s own encoding.
    pub version: u8,
    /// Lower-cased names, because HTTP header names are case-insensitive and a
    /// client is entitled to send `content-length`.
    pub headers: Vec<(String, String)>,
    /// How many bytes of the input the head occupied.
    pub consumed: usize,
}

impl Head {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Whether the client asked for the connection to end after this response.
    ///
    /// HTTP/1.0 closes unless it asked to keep alive; HTTP/1.1 keeps alive
    /// unless it asked to close. Saying so is the whole of the rule.
    pub fn wants_close(&self) -> bool {
        match self.header("connection").map(str::to_ascii_lowercase) {
            Some(value) if value.contains("close") => true,
            Some(value) if value.contains("keep-alive") => false,
            _ => self.version == 0,
        }
    }
}

/// Why a head could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadError {
    /// Not all of it has arrived yet. Read more.
    Incomplete,
    /// It is wrong, and this is the status to answer with.
    Refuse(u16, &'static str),
}

/// Parse a request head, or say why not.
///
/// Pure, so the refusal rules are unit-testable without a socket — which
/// matters, because "chunked is refused" is a claim about behaviour that no
/// amount of reading the code would establish.
pub fn parse_head(buffer: &[u8]) -> Result<Head, HeadError> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);

    let consumed = match request.parse(buffer) {
        Ok(httparse::Status::Complete(consumed)) => consumed,
        Ok(httparse::Status::Partial) => return Err(HeadError::Incomplete),
        // Distinguished from a malformed head because the two call for
        // different answers: one is a client that should send fewer headers,
        // the other is a client that sent nonsense.
        Err(httparse::Error::TooManyHeaders) => {
            return Err(HeadError::Refuse(431, "too many headers"))
        }
        // A version it does not speak, and *only* that, as far as it can tell:
        // `HTTP/2.0` and a line whose version field is nonsense produce the same
        // error, so answering 505 would be a precise-sounding status this
        // parser cannot actually justify. A real HTTP/2 client does not send a
        // text request line at all, so there is no client to be misled.
        Err(httparse::Error::Version) => {
            return Err(HeadError::Refuse(400, "the HTTP version is not supported"))
        }
        Err(_) => return Err(HeadError::Refuse(400, "malformed request")),
    };

    let Some(method) = request.method else {
        return Err(HeadError::Refuse(400, "no method"));
    };
    let Some(path) = request.path else {
        return Err(HeadError::Refuse(400, "no path"));
    };
    let Some(version) = request.version else {
        return Err(HeadError::Refuse(400, "no HTTP version"));
    };

    if version > 1 {
        return Err(HeadError::Refuse(505, "only HTTP/1.0 and 1.1"));
    }
    // An origin-form target begins with `/`. Anything else is a proxy request
    // or an absolute URI, and neither is something a loopback client sends.
    if !path.starts_with('/') {
        return Err(HeadError::Refuse(400, "the target must begin with /"));
    }

    let headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|header| !header.name.is_empty())
        .map(|header| {
            (
                header.name.to_ascii_lowercase(),
                String::from_utf8_lossy(header.value).into_owned(),
            )
        })
        .collect();

    let head = Head {
        method: method.to_string(),
        path: path.to_string(),
        version,
        headers,
        consumed,
    };

    check_framing(&head)?;
    Ok(head)
}

/// Refuse the framings this service does not implement.
///
/// Checked here rather than at the point of reading the body, so a request that
/// cannot be framed is refused before anything is read for it.
fn check_framing(head: &Head) -> Result<(), HeadError> {
    if head.header("transfer-encoding").is_some() {
        // Including `identity`, which is legal but means nothing here.
        return Err(HeadError::Refuse(
            501,
            "transfer-encoding is not implemented; send a Content-Length",
        ));
    }

    if let Some(raw) = head.header("content-length") {
        let parsed = raw.trim().parse::<usize>().map_err(|_| {
            HeadError::Refuse(400, "Content-Length is not a number of bytes")
        })?;
        if parsed > MAX_BODY {
            return Err(HeadError::Refuse(413, "the body is too large"));
        }
    }

    Ok(())
}

/// The length of the body, from the only header that may declare one.
pub fn body_length(head: &Head) -> Result<usize, HeadError> {
    match head.header("content-length") {
        Some(raw) => raw
            .trim()
            .parse::<usize>()
            .map_err(|_| HeadError::Refuse(400, "Content-Length is not a number of bytes")),
        None => Ok(0),
    }
}

/// Read one request from a connection.
///
/// `Ok(None)` means the peer closed cleanly between requests, which is ordinary
/// and not an error.
pub fn read_request(
    stream: &mut TcpStream,
) -> Result<Option<(Head, Vec<u8>)>, HeadError> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];

    let head = loop {
        if buffer.len() > MAX_REQUEST_LINE + MAX_HEADER_BYTES {
            return Err(HeadError::Refuse(431, "the request head is too large"));
        }

        match parse_head(&buffer) {
            Ok(head) => break head,
            Err(HeadError::Incomplete) => {
                // The first read of a connection returning nothing is a client
                // that connected and went away, which is not a refusal.
                let read = match stream.read(&mut chunk) {
                    Ok(0) if buffer.is_empty() => return Ok(None),
                    Ok(0) => return Err(HeadError::Refuse(400, "the request was cut short")),
                    Ok(read) => read,
                    Err(error) if is_timeout(&error) && buffer.is_empty() => return Ok(None),
                    Err(error) if is_timeout(&error) => {
                        return Err(HeadError::Refuse(408, "the request took too long"))
                    }
                    Err(_) => return Ok(None),
                };
                buffer.extend_from_slice(&chunk[..read]);
            }
            Err(refusal) => return Err(refusal),
        }
    };

    let length = body_length(&head)?;

    // A client that sent `Expect: 100-continue` is *waiting* for this before it
    // sends the body. Reading first would block until the timeout and then
    // refuse a request the client was ready to complete — which is what curl
    // does for any body over a kilobyte.
    if length > 0
        && head
            .header("expect")
            .is_some_and(|value| value.eq_ignore_ascii_case("100-continue"))
    {
        write_continue(stream).map_err(|_| HeadError::Refuse(400, "the connection failed"))?;
    }

    let mut body = buffer[head.consumed..].to_vec();
    while body.len() < length {
        let wanted = (length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..wanted]) {
            Ok(0) => return Err(HeadError::Refuse(400, "the body was cut short")),
            Ok(read) => body.extend_from_slice(&chunk[..read]),
            Err(error) if is_timeout(&error) => {
                return Err(HeadError::Refuse(408, "the body took too long"))
            }
            // Any other read failure means the connection is gone. There is
            // nowhere left to send a refusal, but the caller still has to know
            // the request is not forthcoming.
            Err(_) => return Ok(None),
        }
    }
    body.truncate(length);

    Ok(Some((head, body)))
}

fn is_timeout(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// What the router decided.
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// Extra headers, e.g. `Location` and `Retry-After`.
    pub headers: Vec<(&'static str, String)>,
}

impl Response {
    pub fn json(status: u16, body: String) -> Self {
        Self {
            status,
            body: body.into_bytes(),
            headers: Vec::new(),
        }
    }

    pub fn with(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
}

/// Write a response.
///
/// `Content-Length` always, `Connection` to match what the client asked for.
pub fn write_response(
    stream: &mut TcpStream,
    response: &Response,
    close: bool,
) -> std::io::Result<()> {
    let mut out = String::with_capacity(256);
    out.push_str("HTTP/1.1 ");
    out.push_str(&response.status.to_string());
    out.push(' ');
    out.push_str(reason(response.status));
    out.push_str("\r\nContent-Type: application/json; charset=utf-8\r\n");
    out.push_str("Cache-Control: no-store\r\n");
    for (name, value) in &response.headers {
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    out.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    out.push_str(if close {
        "Connection: close\r\n"
    } else {
        "Connection: keep-alive\r\n"
    });
    out.push_str("\r\n");

    stream.write_all(out.as_bytes())?;
    if !response.body.is_empty() {
        stream.write_all(&response.body)?;
    }
    stream.flush()
}

/// Send the interim response curl waits for before it sends a large body.
pub fn write_continue(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        503 => "Service Unavailable",
        505 => "HTTP Version Not Supported",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(text: &str) -> Result<Head, HeadError> {
        parse_head(text.as_bytes())
    }

    #[test]
    fn an_ordinary_get_parses() {
        let parsed = head("GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("parses");
        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.path, "/health");
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.header("host"), Some("127.0.0.1"));
    }

    #[test]
    fn header_names_are_case_insensitive() {
        // A client is entitled to send `Content-Length`. Matching it exactly
        // would read the body length as zero and hang waiting for a body that
        // was already sent.
        let parsed = head("POST /jobs HTTP/1.1\r\nCONTENT-LENGTH: 2\r\n\r\n").expect("parses");
        assert_eq!(parsed.header("content-length"), Some("2"));
        assert_eq!(body_length(&parsed).expect("a length"), 2);
    }

    #[test]
    fn a_partial_head_is_incomplete_rather_than_wrong() {
        // The distinction matters: incomplete means read more, wrong means
        // answer and close. Confusing them turns a slow client into a 400.
        assert_eq!(head("GET /health HTTP/1.1\r\n"), Err(HeadError::Incomplete));
        assert_eq!(head(""), Err(HeadError::Incomplete));
    }

    #[test]
    fn chunked_is_refused_rather_than_mis_parsed() {
        // The important one. Accepting it without implementing it would mean
        // reading a chunk-size line as the body.
        assert_eq!(
            head("POST /jobs HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            Err(HeadError::Refuse(501, "transfer-encoding is not implemented; send a Content-Length"))
        );
    }

    #[test]
    fn a_body_larger_than_the_cap_is_refused_before_it_is_read() {
        let parsed = head(&format!(
            "POST /jobs HTTP/1.1\r\nContent-Length: {}\r\n\r\n",
            MAX_BODY + 1
        ));
        assert_eq!(parsed, Err(HeadError::Refuse(413, "the body is too large")));
    }

    #[test]
    fn a_content_length_that_is_not_a_number_is_refused() {
        assert!(matches!(
            head("POST /jobs HTTP/1.1\r\nContent-Length: many\r\n\r\n"),
            Err(HeadError::Refuse(400, _))
        ));
    }

    #[test]
    fn too_many_headers_are_refused() {
        let mut text = String::from("GET /health HTTP/1.1\r\n");
        for index in 0..(MAX_HEADERS + 5) {
            text.push_str(&format!("X-Pad-{index}: value\r\n"));
        }
        text.push_str("\r\n");

        // Either httparse declines the array or the byte cap catches it; both
        // are refusals rather than a truncated parse.
        assert!(matches!(head(&text), Err(HeadError::Refuse(431, _))));
    }

    #[test]
    fn a_garbage_request_line_is_refused() {
        assert!(matches!(head("not a request\r\n\r\n"), Err(HeadError::Refuse(400, _))));
    }

    #[test]
    fn a_target_that_is_not_origin_form_is_refused() {
        // An absolute URI means a client believes this is a proxy.
        assert!(matches!(
            head("GET http://example.com/ HTTP/1.1\r\n\r\n"),
            Err(HeadError::Refuse(400, _))
        ));
    }

    #[test]
    fn a_version_this_service_cannot_speak_is_refused() {
        // 400 and not 505, deliberately: `httparse` gives the same error for
        // `HTTP/2.0` and for a line whose version field is nonsense, so the
        // more specific status would be a claim the parser cannot support.
        assert!(matches!(
            head("GET / HTTP/2.0\r\n\r\n"),
            Err(HeadError::Refuse(400, _))
        ));
    }

    #[test]
    fn http_1_0_closes_unless_it_asks_otherwise() {
        let plain = head("GET /health HTTP/1.0\r\n\r\n").expect("parses");
        assert!(plain.wants_close(), "1.0 closes by default");

        let kept = head("GET /health HTTP/1.0\r\nConnection: keep-alive\r\n\r\n").expect("parses");
        assert!(!kept.wants_close());
    }

    #[test]
    fn http_1_1_keeps_alive_unless_it_asks_otherwise() {
        let plain = head("GET /health HTTP/1.1\r\n\r\n").expect("parses");
        assert!(!plain.wants_close(), "1.1 keeps alive by default");

        let closed = head("GET /health HTTP/1.1\r\nConnection: close\r\n\r\n").expect("parses");
        assert!(closed.wants_close());
    }

    #[test]
    fn the_body_starts_where_the_head_ended() {
        // The parse reports how much it consumed, and getting that wrong reads
        // the last header as the first bytes of the body.
        let text = "POST /jobs HTTP/1.1\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let parsed = head(text).expect("parses");
        assert_eq!(&text.as_bytes()[parsed.consumed..], b"{\"a\":1}");
    }

    #[test]
    fn every_status_this_service_uses_has_a_reason() {
        for status in [200, 202, 400, 401, 403, 404, 405, 408, 409, 413, 429, 431, 500, 501, 503, 505] {
            assert_ne!(reason(status), "Unknown", "status {status} has no reason");
        }
    }
}

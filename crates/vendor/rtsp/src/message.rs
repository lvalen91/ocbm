//! RTSP/1.0 + HTTP/1.1 message codec for the receiver control plane. AirPlay/CarPlay uses RTSP-style
//! requests (OPTIONS/SETUP/RECORD/…) and HTTP-style ones (GET/POST `/info`, `/pair-*`) over the same
//! TCP connection; the protocol token on the request line selects the response version. Messages are
//! request-line + CRLF headers + blank line + `Content-Length` body (clean-room; zero-dependency).

/// Protocol version on the request/status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Version {
    Rtsp10,
    Http11,
}

impl Version {
    pub fn token(self) -> &'static str {
        match self {
            Version::Rtsp10 => "RTSP/1.0",
            Version::Http11 => "HTTP/1.1",
        }
    }
    fn parse(tok: &str) -> Option<Version> {
        match tok {
            "RTSP/1.0" => Some(Version::Rtsp10),
            "HTTP/1.1" | "HTTP/1.0" => Some(Version::Http11),
            _ => None,
        }
    }
}

/// Upper bound on a request body — RTSP control messages (incl. SETUP plists) are small; anything
/// larger is rejected rather than trusted. Matches the C `AirPlayReceiverServer`'s 16 MB HTTP
/// message cap (`kHTTPMessage*MaxLen`), not the old 1 MiB placeholder.
/// Largest RTSP body accepted before parsing.
///
/// Was 16 MiB, which is ~1000x what this protocol ever legitimately carries and was reachable
/// PRE-PAIR-VERIFY: `ControlServer::handle` routes `/command`, `SETUP` and `TEARDOWN` on the
/// plaintext path, so anything that could reach `[::]:5000` — the box's own AP subnet in wireless
/// mode — could send `Content-Length: 16777216` and cost 33 MiB of RSS (27% of a 123 MB box) per
/// request, unauthenticated. It also afforded ~1.8M levels of nested bplist, ~14x what is needed to
/// overflow the stack in `plist::Value`'s recursive drop glue.
///
/// Real bodies on this protocol: the largest observed are a 1482 B SUBSCRIBE config and a 791 B
/// SETUP. 256 KiB is three orders of magnitude of headroom over anything real.
pub const MAX_BODY: usize = 256 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    BadRequestLine,
    BadHeader,
    BadContentLength,
}

/// A parsed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub method: String,
    pub uri: String,
    pub version: Version,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// Case-insensitive header lookup.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// The path portion of the URI (strips an `rtsp://host` prefix and any `?query`).
    pub fn path(&self) -> &str {
        let u = &self.uri;
        let after_authority = if let Some(idx) = u.find("://") {
            u[idx + 3..].find('/').map(|s| &u[idx + 3 + s..]).unwrap_or("/")
        } else {
            u.as_str()
        };
        after_authority.split('?').next().unwrap_or(after_authority)
    }

    /// Parse one request from `buf`. Returns `Ok(None)` if more bytes are needed, or
    /// `Ok(Some((req, consumed)))` once a full message (incl. body) is present.
    pub fn parse(buf: &[u8]) -> Result<Option<(Request, usize)>, ParseError> {
        let Some(head_end) = find_crlf_crlf(buf) else {
            return Ok(None);
        };
        let head = core::str::from_utf8(&buf[..head_end]).map_err(|_| ParseError::BadHeader)?;
        let mut lines = head.split("\r\n");

        let req_line = lines.next().ok_or(ParseError::BadRequestLine)?;
        let mut parts = req_line.splitn(3, ' ');
        let method = parts.next().ok_or(ParseError::BadRequestLine)?.to_string();
        let uri = parts.next().ok_or(ParseError::BadRequestLine)?.to_string();
        let version =
            Version::parse(parts.next().ok_or(ParseError::BadRequestLine)?).ok_or(ParseError::BadRequestLine)?;

        let mut headers = Vec::new();
        for line in lines {
            if line.is_empty() {
                continue;
            }
            let (k, v) = line.split_once(':').ok_or(ParseError::BadHeader)?;
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }

        let body_start = head_end + 4;
        let content_len = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Content-Length"))
            .map(|(_, v)| v.parse::<usize>().map_err(|_| ParseError::BadContentLength))
            .transpose()?
            .unwrap_or(0);
        if content_len > MAX_BODY {
            return Err(ParseError::BadContentLength);
        }

        let total = body_start.checked_add(content_len).ok_or(ParseError::BadContentLength)?;
        if buf.len() < total {
            return Ok(None);
        }
        let body = buf[body_start..total].to_vec();
        Ok(Some((Request { method, uri, version, headers, body }, total)))
    }
}

/// A response to serialize back to the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub version: Version,
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    /// A 200 OK echoing the request's version + CSeq, with the given content-type and body.
    pub fn ok(req: &Request, content_type: Option<&str>, body: Vec<u8>) -> Response {
        let mut headers = Vec::new();
        if let Some(cseq) = req.header("CSeq") {
            headers.push(("CSeq".to_string(), cseq.to_string()));
        }
        if let Some(ct) = content_type {
            headers.push(("Content-Type".to_string(), ct.to_string()));
        }
        Response { version: req.version, status: 200, reason: "OK".into(), headers, body }
    }

    /// A bodyless error response echoing version + CSeq.
    pub fn status(req: &Request, status: u16, reason: &str) -> Response {
        let mut headers = Vec::new();
        if let Some(cseq) = req.header("CSeq") {
            headers.push(("CSeq".to_string(), cseq.to_string()));
        }
        Response { version: req.version, status, reason: reason.into(), headers, body: Vec::new() }
    }

    /// Serialize, injecting `Content-Length` (always, mirroring the server).
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = format!("{} {} {}\r\n", self.version.token(), self.status, self.reason);
        for (k, v) in &self.headers {
            if !k.eq_ignore_ascii_case("Content-Length") {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
        }
        // The C server stamps a Server header on every response; mirror it if the caller didn't.
        if !self.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("Server")) {
            // SDK-inferred and UNVERIFIED in capture: RTSP rides inside the encrypted NCM channel, so
            // no `Server:` literal appears in any log. `AirTunes/509.11` is the SDK-inferred value.
            out.push_str("Server: AirTunes/509.11\r\n");
        }
        out.push_str(&format!("Content-Length: {}\r\n\r\n", self.body.len()));
        let mut bytes = out.into_bytes();
        bytes.extend_from_slice(&self.body);
        bytes
    }
}

fn find_crlf_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rtsp_setup_with_body() {
        let raw = b"SETUP rtsp://10.0.0.1/abc RTSP/1.0\r\nCSeq: 3\r\nContent-Length: 5\r\n\r\nhello";
        let (req, used) = Request::parse(raw).unwrap().unwrap();
        assert_eq!(used, raw.len());
        assert_eq!(req.method, "SETUP");
        assert_eq!(req.version, Version::Rtsp10);
        assert_eq!(req.path(), "/abc");
        assert_eq!(req.header("cseq"), Some("3"));
        assert_eq!(req.body, b"hello");
    }

    #[test]
    fn parse_http_post_info_query_path() {
        let raw = b"POST /info?txtAirPlay HTTP/1.1\r\nContent-Length: 0\r\n\r\n";
        let (req, _) = Request::parse(raw).unwrap().unwrap();
        assert_eq!(req.version, Version::Http11);
        assert_eq!(req.path(), "/info");
    }

    #[test]
    fn parse_incomplete_returns_none() {
        assert_eq!(Request::parse(b"GET /info RTSP/1.0\r\nCSeq: 1\r\n").unwrap(), None); // no blank line
        // headers complete but body short
        assert_eq!(
            Request::parse(b"POST /x RTSP/1.0\r\nContent-Length: 4\r\n\r\nhi").unwrap(),
            None
        );
    }

    #[test]
    fn rejects_oversized_or_overflowing_content_length() {
        // overflowing integer → error, not panic
        let raw = b"POST /x RTSP/1.0\r\nContent-Length: 99999999999999999999999\r\n\r\n";
        assert_eq!(Request::parse(raw), Err(ParseError::BadContentLength));
        // valid number but over the cap → error
        let over = format!("POST /x RTSP/1.0\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1);
        assert_eq!(Request::parse(over.as_bytes()), Err(ParseError::BadContentLength));
    }

    #[test]
    fn response_echoes_cseq_and_sets_length() {
        let (req, _) = Request::parse(b"GET /info RTSP/1.0\r\nCSeq: 7\r\n\r\n").unwrap().unwrap();
        let resp = Response::ok(&req, Some("application/x-apple-binary-plist"), b"\x01\x02".to_vec());
        let s = resp.serialize();
        let text = String::from_utf8_lossy(&s);
        assert!(text.starts_with("RTSP/1.0 200 OK\r\n"));
        assert!(text.contains("CSeq: 7\r\n"));
        assert!(text.contains("Content-Type: application/x-apple-binary-plist\r\n"));
        assert!(text.contains("Content-Length: 2\r\n\r\n"));
        assert!(s.ends_with(&[0x01, 0x02]));
    }
}

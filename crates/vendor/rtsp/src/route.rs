//! Request routing — maps (method, path) to a receiver action, mirroring the dispatch table in
//! `AirPlayReceiverServer.c` (`_requestProcess`). The handler bodies live in the receiver/server
//! wiring; this is the pure classification so it can be unit-tested and shared.
//!
//! Faithful to the C: methods are matched case-insensitively (`strnicmpx`) and paths by
//! case-insensitive suffix (`strnicmp_suffix`); the pairing/auth/command/feedback/diag-info
//! endpoints are **POST-only**, `/info` is GET or POST, `/log`+`/logs` are diagnostic, an
//! unrecognized path under a known method is 404, and an unknown method is 501.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Options,
    Flush,
    Record,
    Setup,
    Teardown,
    Info,
    AuthSetup,
    Command,
    Feedback,
    PairSetup,
    PairVerify,
    GetParameter,
    SetParameter,
    DiagInfo,
    Log,
    /// Known method but unrecognized path → 404 Not Found.
    NotFound,
    /// Unrecognized method → 501 Not Implemented.
    NotImplemented,
}

impl Route {
    /// Classify a request by its method and path (path already stripped of authority/query).
    pub fn classify(method: &str, path: &str) -> Route {
        // Case-insensitive suffix match, mirroring the C `strnicmp_suffix`.
        let suffix = |s: &str| {
            let p = path.as_bytes();
            let s = s.as_bytes();
            p.len() >= s.len() && p[p.len() - s.len()..].eq_ignore_ascii_case(s)
        };
        // `/logs` is served regardless of method (handled before the method switch in the C).
        if suffix("/logs") {
            return Route::Log;
        }
        match method.to_ascii_uppercase().as_str() {
            "OPTIONS" => Route::Options,
            "SET_PARAMETER" => Route::SetParameter,
            "FLUSH" => Route::Flush,
            "GET_PARAMETER" => Route::GetParameter,
            "RECORD" => Route::Record,
            "SETUP" => Route::Setup,
            "TEARDOWN" => Route::Teardown,
            "GET" => {
                if suffix("/log") {
                    Route::Log
                } else if suffix("/info") {
                    Route::Info
                } else {
                    Route::NotFound
                }
            }
            "POST" => {
                if suffix("/auth-setup") {
                    Route::AuthSetup
                } else if suffix("/command") {
                    Route::Command
                } else if suffix("/diag-info") {
                    Route::DiagInfo
                } else if suffix("/feedback") {
                    Route::Feedback
                } else if suffix("/info") {
                    Route::Info
                } else if suffix("/pair-setup") {
                    Route::PairSetup
                } else if suffix("/pair-verify") {
                    Route::PairVerify
                } else {
                    Route::NotFound
                }
            }
            _ => Route::NotImplemented,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_rtsp_and_http() {
        assert_eq!(Route::classify("SETUP", "/abc"), Route::Setup);
        assert_eq!(Route::classify("RECORD", "*"), Route::Record);
        assert_eq!(Route::classify("GET", "/info"), Route::Info);
        assert_eq!(Route::classify("POST", "/pair-verify"), Route::PairVerify);
        assert_eq!(Route::classify("POST", "/pair-setup"), Route::PairSetup);
        assert_eq!(Route::classify("POST", "/auth-setup"), Route::AuthSetup);
        assert_eq!(Route::classify("POST", "/feedback"), Route::Feedback);
        assert_eq!(Route::classify("POST", "/diag-info"), Route::DiagInfo);
    }

    #[test]
    fn pairing_endpoints_are_post_only() {
        // The C dispatches pair-setup/pair-verify/auth-setup under POST only; a GET is a 404.
        assert_eq!(Route::classify("GET", "/pair-setup"), Route::NotFound);
        assert_eq!(Route::classify("GET", "/pair-verify"), Route::NotFound);
        assert_eq!(Route::classify("GET", "/auth-setup"), Route::NotFound);
    }

    #[test]
    fn unknown_method_vs_unknown_path() {
        assert_eq!(Route::classify("POST", "/nope"), Route::NotFound);
        assert_eq!(Route::classify("GET", "/nope"), Route::NotFound);
        assert_eq!(Route::classify("WAT", "/info"), Route::NotImplemented);
        assert_eq!(Route::classify("PUT", "/x"), Route::NotImplemented);
    }

    #[test]
    fn case_insensitive_method_and_suffix_path() {
        assert_eq!(Route::classify("post", "/pair-setup"), Route::PairSetup);
        assert_eq!(Route::classify("GET", "rtsp://host/info"), Route::Info);
        assert_eq!(Route::classify("GET", "/log"), Route::Log);
        assert_eq!(Route::classify("POST", "/logs"), Route::Log);
    }
}

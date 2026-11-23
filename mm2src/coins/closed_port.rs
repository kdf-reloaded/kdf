//! A loopback address on which nothing listens, for the tests that need a node or a service that is not there.

/// A loopback port on which nothing listens and which no stub server of a test can be given.
///
/// The operating system never hands out a port below 1024 for a bind to port 0, so no stub server of a parallel
/// test can end up on it, and nothing listens on port 1 of a build host. A connection to it is refused at once.
/// A port that was bound and released again would not do: the next bind to port 0 anywhere in the test process
/// may get it, and that server would then receive the requests that were meant to fail.
///
/// Limits:
/// - On Windows a refused loopback connection is reported after about a second, not at once: a test that measures
///   time against this port needs slack there.
/// - A browser refuses port 1 as an unsafe port, so this helper is meant for native tests; in a browser a request
///   to it fails before any connection is made.
/// - On a host whose packet filter drops the connection attempt instead of refusing it, the refusal becomes a
///   timeout.
///
/// If one of these ever matters, the stricter form is a socket that is bound and never listens, held for the
/// lifetime of the test.
pub(crate) const CLOSED_PORT: u16 = 1;

/// The base URL of a server that is not there: every connection to it is refused. See [`CLOSED_PORT`].
pub(crate) fn closed_url() -> String { format!("http://127.0.0.1:{CLOSED_PORT}") }

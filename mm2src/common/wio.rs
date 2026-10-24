//! `wio` stands for "web I/O", it contains the parts which aren't directly available with WASM.

use futures::compat::Future01CompatExt;
use futures::executor::ThreadPool;
use futures01::sync::oneshot::{self, Receiver};
use futures01::{Async, Future, Poll};
use gstuff::{duration_to_float, now_float};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioTimer;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::runtime::Runtime;

fn start_core_thread() -> Mm2Runtime { Mm2Runtime(Runtime::new().unwrap()) }

pub struct Mm2Runtime(pub Runtime);

lazy_static! {
    /// Shared asynchronous reactor.
    pub static ref CORE: Mm2Runtime = start_core_thread();
    /// Shared CPU pool to run intensive/sleeping requests on s separate thread.
    pub static ref POOL: Mutex<ThreadPool> = Mutex::new(ThreadPool::builder()
        .pool_size(8)
        .name_prefix("POOL")
        .create().expect("!ThreadPool"));
}

impl<Fut: std::future::Future<Output = ()> + Send + 'static> hyper::rt::Executor<Fut> for &Mm2Runtime {
    fn execute(&self, fut: Fut) { self.0.spawn(fut); }
}

/// With a shared reactor drives the future `f` to completion.
///
/// NB: This function is only useful if you need to get the results of the execution.
/// If the results are not necessary then a future can be scheduled directly on the reactor:
///
///     CORE.spawn (|_| f);
pub fn drive<F, R, E>(f: F) -> Receiver<Result<R, E>>
where
    F: Future<Item = R, Error = E> + Send + 'static,
    R: Send + 'static,
    E: Send + 'static,
{
    let (sx, rx) = oneshot::channel();
    CORE.0.spawn(
        f.then(move |fr: Result<R, E>| -> Result<(), ()> {
            let _ = sx.send(fr);
            Ok(())
        })
        .compat(),
    );
    rx
}

pub fn drive03<F, O>(f: F) -> futures::channel::oneshot::Receiver<O>
where
    F: std::future::Future<Output = O> + Send + 'static,
    O: Send + 'static,
{
    let (sx, rx) = futures::channel::oneshot::channel();
    CORE.0.spawn(async move {
        let res = f.await;
        if sx.send(res).is_err() {
            log!("drive03 receiver is dropped");
        };
    });
    rx
}

/// With a shared reactor drives the future `f` to completion.
///
/// Similar to `fn drive`, but returns a stringified error,
/// allowing us to collapse the `Receiver` and return the `R` directly.
pub fn drive_s<F, R, E>(f: F) -> impl Future<Item = R, Error = String>
where
    F: Future<Item = R, Error = E> + Send + 'static,
    R: Send + 'static,
    E: fmt::Display + Send + 'static,
{
    drive(f).then(move |r| -> Result<R, String> {
        let r = try_s!(r); // Peel the `Receiver`.
        let r = try_s!(r); // `E` to `String`.
        Ok(r)
    })
}

/// Finishes with the "timeout" error if the underlying future isn't ready withing the given timeframe.
///
/// NB: Tokio timers (in `tokio::timer`) only seem to work under the Tokio runtime,
/// which is unfortunate as we want the different futures executed on the different reactors
/// depending on how much they're I/O-bound, CPU-bound or blocking.
/// Unlike the Tokio timers this `Timeout` implementation works with any reactor.
/// Another option to consider is https://github.com/alexcrichton/futures-timer.
/// P.S. The older `0.1` version of the `tokio::timer` might work NP, it works in other parts of our code.
///      The new version, on the other hand, requires the Tokio runtime (https://tokio.rs/blog/2018-03-timers/).
/// P.S. We could try using the `futures-timer` crate instead, but note that it is currently under-maintained,
///      https://github.com/rustasync/futures-timer/issues/9#issuecomment-400802515.
pub struct Timeout<R> {
    fut: Box<dyn Future<Item = R, Error = String>>,
    started: f64,
    timeout: f64,
    monitor: Option<JoinHandle<()>>,
}
impl<R> Future for Timeout<R> {
    type Item = R;
    type Error = String;
    fn poll(&mut self) -> Poll<R, String> {
        match self.fut.poll() {
            Err(err) => Err(err),
            Ok(Async::Ready(r)) => Ok(Async::Ready(r)),
            Ok(Async::NotReady) => {
                let now = now_float();
                if now >= self.started + self.timeout {
                    Err(format!("timeout ({:.1} > {:.1})", now - self.started, self.timeout))
                } else {
                    // Start waking up this future until it has a chance to timeout.
                    // For now it's just a basic separate thread. Will probably optimize later.
                    if self.monitor.is_none() {
                        let task = futures01::task::current();
                        let deadline = self.started + self.timeout;
                        self.monitor = Some(
                            std::thread::Builder::new()
                                .name("timeout monitor".into())
                                .spawn(move || loop {
                                    std::thread::sleep(Duration::from_secs(1));
                                    task.notify();
                                    if now_float() > deadline + 2. {
                                        break;
                                    }
                                })
                                .unwrap(),
                        );
                    }
                    Ok(Async::NotReady)
                }
            },
        }
    }
}
impl<R> Timeout<R> {
    pub fn new(fut: Box<dyn Future<Item = R, Error = String>>, timeout: Duration) -> Timeout<R> {
        Timeout {
            fut,
            started: now_float(),
            timeout: duration_to_float(timeout),
            monitor: None,
        }
    }
}

unsafe impl<R> Send for Timeout<R> where Box<dyn Future<Item = R, Error = String>>: Send {}

/// Initialize the crate.
pub fn init() {
    // Pre-allocate the stack trace buffer in order to avoid allocating it from a signal handler.
    super::black_box(&*super::trace_buf());
    super::black_box(&*super::trace_name_buf());
}

/// Pending-connection queue length of the server listeners.
///
/// `std::net::TcpListener::bind` uses 128; the `hyper` 0.14 servers bound through tokio, which uses 1024.
pub const LISTEN_BACKLOG: u32 = 1024;

/// Binds a listening socket for the HTTP servers, synchronously, so a bind failure reaches the caller.
///
/// Built with `TcpSocket` to get a backlog of [`LISTEN_BACKLOG`]. `TcpSocket` does not do what
/// `std::net::TcpListener::bind` does on Unix, which is to set `SO_REUSEADDR`; without it a restart
/// can fail with "address already in use" while the old socket sits in `TIME_WAIT`, so it is set
/// explicitly (and, like `std`, not at all on Windows, where it would allow port hijacking).
/// `IPV6_V6ONLY` is left at the system default, as `std` leaves it.
///
/// The socket is registered with the shared reactor, so this enters the `CORE` runtime itself.
pub fn bind_listener(addr: std::net::SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    let _guard = CORE.0.enter();
    let socket = if addr.is_ipv4() {
        tokio::net::TcpSocket::new_v4()?
    } else {
        tokio::net::TcpSocket::new_v6()?
    };
    #[cfg(unix)]
    socket.set_reuseaddr(true)?;
    socket.bind(addr)?;
    socket.listen(LISTEN_BACKLOG)
}

/// Builds the HTTPS connector shared by the native HTTP clients (`HYPER` and the Tendermint RPC client).
///
/// The `ring` crypto provider is passed explicitly instead of relying on the process-global default
/// provider: that default is resolved at runtime on the first handshake and panics when it is ambiguous,
/// and nothing here should depend on what else in the process installed. Trust anchors are the bundled
/// `webpki-roots` set, TLS 1.2 and 1.3 are both enabled (`tls12` feature), and HTTP/1.1 and HTTP/2 are
/// negotiated through ALPN.
pub fn https_connector() -> HttpsConnector<HttpConnector> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    // Please note there was a problem on iOS if native roots are used instead.
    HttpsConnectorBuilder::new()
        .with_provider_and_webpki_roots(provider)
        .expect("the ring provider supports the default protocol versions")
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build()
}

/// The shared HTTP client type: request bodies are fully buffered.
pub type HyperClient = Client<HttpsConnector<HttpConnector>, Full<Bytes>>;

lazy_static! {
    /// NB: With a shared client there is a possibility that keep-alive connections will be reused.
    pub static ref HYPER: HyperClient = {
        Client::builder(&*CORE)
            .pool_timer(TokioTimer::new())
            // Hyper had a lot of Keep-Alive bugs over the years and I suspect
            // that with the shared client we might be getting errno 10054
            // due to a closed Keep-Alive connection mismanagement.
            // (To solve this problem Hyper should proactively close the Keep-Alive
            // connections after a configurable amount of time has passed since
            // their creation, thus saving us from trying to use the connections
            // closed on the other side. I wonder if we can implement this strategy
            // ourselves with a custom connector or something).
            // Performance of Keep-Alive in the Hyper client is questionable as well,
            // should measure it on a case-by-case basis when we need it.
            .pool_max_idle_per_host(0)
            .build(https_connector())
    };
}

#[cfg(all(test, unix))]
mod listener_tests {
    use super::*;
    use std::io::Read;
    use std::net::TcpStream;

    /// The server side closes first, so its socket is left in `TIME_WAIT`; a restart must still bind.
    #[test]
    fn bind_listener_rebinds_over_time_wait() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).unwrap();
        let (server_side, _) = CORE.0.block_on(listener.accept()).unwrap();
        drop(server_side); // active close on the server: this socket goes to TIME_WAIT
        assert_eq!(client.read(&mut [0u8; 1]).unwrap(), 0);
        drop(listener);
        let again = bind_listener(addr).expect("rebinding the same port right after a restart");
        assert_eq!(again.local_addr().unwrap(), addr);
    }

    #[test]
    fn bind_listener_reports_a_busy_port_synchronously() {
        let listener = bind_listener("127.0.0.1:0".parse().unwrap()).unwrap();
        let err = bind_listener(listener.local_addr().unwrap()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[test]
    fn bind_listener_accepts_ipv6_when_the_host_has_it() {
        // IPv6 may be disabled on the host (CI containers); only a bind that is possible at all is checked.
        if std::net::TcpListener::bind("[::1]:0").is_err() {
            return;
        }
        let listener = bind_listener("[::1]:0".parse().unwrap()).unwrap();
        assert!(listener.local_addr().unwrap().is_ipv6());
    }
}

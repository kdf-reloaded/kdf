//! A loopback HTTP stub that answers every request with canned bytes, for the tests that check what an error text
//! carries of a node's answer (CRD ch.38 T38.6.9.3, ch.20 T-N3).

use std::io::{Read, Write};

/// Serves a loopback port and answers each connection, in arrival order, with the bytes `answer` returns for the
/// request it received (the request head and body as lossy text). Returns the port. The serving thread ends with
/// the test process.
pub(crate) fn serve(answer: impl Fn(&str) -> Vec<u8> + Send + 'static) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            // The head and the body may arrive in separate segments: read up to the declared length.
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let n = stream.read(&mut chunk).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&request).to_ascii_lowercase();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                    if body.len() >= length {
                        break;
                    }
                }
            }
            let reply = answer(&String::from_utf8_lossy(&request));
            // A client that gave up (it read what it needed) is no failure of the stub.
            let _ = stream.write_all(&reply);
        }
    });
    port
}

/// A complete HTTP/1.1 answer with `status` (for example `"500 Internal Server Error"`), the given content type and
/// body. The connection is closed after it, so a client never reuses it.
pub(crate) fn http_response(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    reply.extend_from_slice(body);
    reply
}

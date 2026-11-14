//! The queries and the answers of the Google and the fixed modes (CRD ch.45 R45.6B.3, R45.6B.4): messages built and read
//! with the types of `hickory-proto`, sent over tokio's UDP and TCP sockets.

use super::Options;
use hickory_proto::op::{Message, MessageType, Query, ResponseCode};
use hickory_proto::rr::{DNSClass, Name, RData, RecordType};
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// How many CNAME links of an answer are followed (R45.6B.3).
pub(super) const MAX_CNAME_LINKS: usize = 8;

/// The question of a query: an absolute name and a record type (`A` or `AAAA`).
#[derive(Clone, Debug)]
pub(super) struct Question {
    pub(super) name: Name,
    pub(super) record_type: RecordType,
}

/// `name` as an absolute name: no search domain is appended (R45.6B.3 item 4). Non-ASCII input goes through the IDNA
/// conversion. An invalid name is an error, never a panic.
pub(super) fn absolute_name(name: &str) -> Result<Name, String> {
    if name.trim_matches('.').is_empty() {
        return Err("the name is empty".to_owned());
    }
    let mut name = Name::from_utf8(name).map_err(|e| format!("the name is not a valid DNS name: {}", e))?;
    name.set_fqdn(true);
    Ok(name)
}

/// What one server's answer means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Interpreted {
    /// The addresses of the question and the time they may be kept (clamped, R45.6B.4 item 1).
    Positive { addresses: Vec<IpAddr>, ttl: Duration },
    /// The name does not exist (`nxdomain`) or has no address of the type: authoritative, the servers after this one are
    /// not asked.
    Negative { nxdomain: bool, ttl: Duration },
    /// The server did not answer, or reported its own failure: the next server is asked.
    ServerFailure(String),
}

/// Whether `response` is the answer to the query `id` asked for `question`: the same ID and the same question.
pub(super) fn is_answer_to(response: &Message, id: u16, question: &Question) -> bool {
    response.metadata.message_type == MessageType::Response
        && response.metadata.id == id
        && response.queries.len() == 1
        && response.queries[0].query_type == question.record_type
        && response.queries[0].query_class == DNSClass::IN
        && response.queries[0].name.eq_ignore_root(&question.name)
}

/// Reads an answer that `is_answer_to` accepted (R45.6B.3, R45.6B.4 item 1).
///
/// A CNAME chain that starts at the name asked is followed inside the answer section, up to [`MAX_CNAME_LINKS`] links.
/// The address records of the type whose owner is in that chain are the answer, in the server's order. Their lifetime is
/// the least of the records used. A chain that ends without addresses is a negative answer: no new query follows it.
/// A negative answer lives as long as the SOA record of the authority section says (the least of its own lifetime and its
/// minimum), or the floor when there is none.
pub(super) fn interpret(response: &Message, question: &Question, options: &Options) -> Interpreted {
    let code = response.metadata.response_code;
    match code {
        ResponseCode::NoError | ResponseCode::NXDomain => {},
        ResponseCode::ServFail => return Interpreted::ServerFailure("the server reported a failure (SERVFAIL)".into()),
        ResponseCode::Refused => return Interpreted::ServerFailure("the server refused the query (REFUSED)".into()),
        other => return Interpreted::ServerFailure(format!("the server answered with the code {}", other)),
    }

    let mut chain: Vec<&Name> = vec![&question.name];
    let mut lifetime: Option<u32> = None;
    let mut current = &question.name;
    for _ in 0..MAX_CNAME_LINKS {
        let link = response.answers.iter().find_map(|record| match &record.data {
            RData::CNAME(target) if record.name.eq_ignore_root(current) => Some((record.ttl, &target.0)),
            _ => None,
        });
        match link {
            Some((ttl, target)) if !chain.iter().any(|seen| seen.eq_ignore_root(target)) => {
                lifetime = Some(lifetime.map_or(ttl, |least| least.min(ttl)));
                chain.push(target);
                current = target;
            },
            _ => break,
        }
    }

    let mut addresses = Vec::new();
    for record in &response.answers {
        if record.record_type() != question.record_type || !chain.iter().any(|owner| owner.eq_ignore_root(&record.name))
        {
            continue;
        }
        if let Some(address) = record.data.ip_addr() {
            addresses.push(address);
            lifetime = Some(lifetime.map_or(record.ttl, |least| least.min(record.ttl)));
        }
    }

    if !addresses.is_empty() {
        let ttl = Duration::from_secs(u64::from(lifetime.unwrap_or(0)));
        return Interpreted::Positive {
            addresses,
            ttl: ttl.clamp(options.positive_min_ttl, options.positive_max_ttl),
        };
    }
    let negative = response.authorities.iter().find_map(|record| match &record.data {
        RData::SOA(soa) => Some(record.ttl.min(soa.minimum)),
        _ => None,
    });
    let ttl = negative.map_or(options.negative_min_ttl, |secs| Duration::from_secs(u64::from(secs)));
    Interpreted::Negative {
        nxdomain: code == ResponseCode::NXDomain,
        ttl: ttl.clamp(options.negative_min_ttl, options.negative_max_ttl),
    }
}

/// The query for `question`: a random ID, the recursion-desired bit set (R45.6B.3 item 4).
fn build_query(question: &Question) -> Result<(u16, Vec<u8>), String> {
    let mut message = Message::query();
    message.metadata.recursion_desired = true;
    message.add_query(Query::query(question.name.clone(), question.record_type));
    let id = message.metadata.id;
    let bytes = message
        .to_vec()
        .map_err(|e| format!("the query could not be built: {}", e))?;
    Ok((id, bytes))
}

/// A new UDP socket on a port the operating system picks (R45.6B.3 item 4): every query gets a fresh socket, and the
/// ephemeral port is drawn at random by the platforms the node runs on. An explicit draw by the node could take a port that
/// a local service is about to listen on.
async fn bind_fresh(server: &SocketAddr) -> io::Result<UdpSocket> {
    let any: IpAddr = if server.is_ipv4() {
        Ipv4Addr::UNSPECIFIED.into()
    } else {
        Ipv6Addr::UNSPECIFIED.into()
    };
    UdpSocket::bind((any, 0)).await
}

fn timed_out() -> io::Error { io::Error::new(io::ErrorKind::TimedOut, "no answer in time") }

/// One UDP exchange. Datagrams that are not the answer to this question (another ID or another question) are ignored
/// until the time is up. The socket is connected, so the operating system drops datagrams from any other address.
async fn ask_udp(
    server: SocketAddr,
    id: u16,
    query: &[u8],
    question: &Question,
    timeout: Duration,
) -> io::Result<Message> {
    let socket = bind_fresh(&server).await?;
    socket.connect(server).await?;
    socket.send(query).await?;
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buffer = vec![0u8; 4096];
    loop {
        let received = tokio::time::timeout_at(deadline, socket.recv(&mut buffer))
            .await
            .map_err(|_| timed_out())??;
        if let Ok(message) = Message::from_vec(&buffer[..received]) {
            if is_answer_to(&message, id, question) {
                return Ok(message);
            }
        }
    }
}

/// One TCP exchange: the message and the answer, each with a two-byte length in front.
async fn ask_tcp(
    server: SocketAddr,
    id: u16,
    query: &[u8],
    question: &Question,
    timeout: Duration,
) -> io::Result<Message> {
    let exchange = async {
        let length =
            u16::try_from(query.len()).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "query too long"))?;
        let mut stream = TcpStream::connect(server).await?;
        let mut framed = Vec::with_capacity(query.len() + 2);
        framed.extend_from_slice(&length.to_be_bytes());
        framed.extend_from_slice(query);
        stream.write_all(&framed).await?;
        let mut prefix = [0u8; 2];
        stream.read_exact(&mut prefix).await?;
        let mut body = vec![0u8; usize::from(u16::from_be_bytes(prefix))];
        stream.read_exact(&mut body).await?;
        let message =
            Message::from_vec(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        if is_answer_to(&message, id, question) {
            Ok(message)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the answer is not for the question asked",
            ))
        }
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(result) => result,
        Err(_) => Err(timed_out()),
    }
}

/// Asks one server (R45.6B.3): over UDP, waiting up to [`Options::query_timeout`], and again over TCP to the same server
/// when the answer has the TC bit set. Any problem of the exchange is a [`Interpreted::ServerFailure`].
pub(super) async fn ask(server: SocketAddr, question: &Question, options: &Options) -> Interpreted {
    let (id, query) = match build_query(question) {
        Ok(built) => built,
        Err(text) => return Interpreted::ServerFailure(text),
    };
    let message = match ask_udp(server, id, &query, question, options.query_timeout).await {
        Ok(message) if message.metadata.truncation => {
            match ask_tcp(server, id, &query, question, options.query_timeout).await {
                Ok(message) => message,
                Err(error) => return Interpreted::ServerFailure(format!("over TCP: {}", error)),
            }
        },
        Ok(message) => message,
        Err(error) => return Interpreted::ServerFailure(error.to_string()),
    };
    interpret(&message, question, options)
}

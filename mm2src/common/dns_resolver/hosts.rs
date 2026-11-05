//! The operating system's hosts file (CRD ch.45 R45.6B.3 item 2). It is read once, when the resolver is installed.

use super::DnsFamily;
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;

/// The names of the hosts file and their addresses, in the order of the file.
#[derive(Clone, Debug, Default)]
pub(super) struct Hosts {
    by_name: HashMap<String, Vec<IpAddr>>,
}

/// `/etc/hosts` on Unix (Android included), `%SystemRoot%\System32\drivers\etc\hosts` on Windows.
#[cfg(windows)]
fn hosts_path() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root)
        .join("System32")
        .join("drivers")
        .join("etc")
        .join("hosts")
}

#[cfg(not(windows))]
fn hosts_path() -> PathBuf { PathBuf::from("/etc/hosts") }

impl Hosts {
    /// Reads the system's hosts file. An unreadable file is an empty map, with one DEBUG line.
    pub(super) fn load() -> Hosts {
        let path = hosts_path();
        match std::fs::read(&path) {
            Ok(bytes) => Hosts::parse(&String::from_utf8_lossy(&bytes)),
            Err(error) => {
                log::debug!(
                    "DNS: the hosts file could not be read ({}); it is taken as empty",
                    error
                );
                Hosts::default()
            },
        }
    }

    /// Parses the text of a hosts file: `#` starts a comment, a line is an address followed by a name and any aliases,
    /// names are case-insensitive, IPv4 and IPv6 entries alike. A line that does not start with an address is skipped.
    pub(super) fn parse(text: &str) -> Hosts {
        let mut by_name: HashMap<String, Vec<IpAddr>> = HashMap::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("");
            let mut tokens = line.split_whitespace();
            let address = match tokens.next() {
                // A zone identifier (`fe80::1%lo`) is not part of the address.
                Some(token) => match token.split('%').next().unwrap_or("").parse::<IpAddr>() {
                    Ok(address) => address,
                    Err(_) => continue,
                },
                None => continue,
            };
            for name in tokens {
                let addresses = by_name
                    .entry(name.trim_end_matches('.').to_ascii_lowercase())
                    .or_default();
                if !addresses.contains(&address) {
                    addresses.push(address);
                }
            }
        }
        Hosts { by_name }
    }

    /// The addresses of `name` (lowercase, without a trailing dot) of the family asked for. `None` when the file lists
    /// none: the name then goes to the servers.
    pub(super) fn lookup(&self, name: &str, family: DnsFamily) -> Option<Vec<IpAddr>> {
        let found = family.select(self.by_name.get(name)?);
        if found.is_empty() {
            None
        } else {
            Some(found)
        }
    }
}

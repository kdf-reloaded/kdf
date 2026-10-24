// Copyright 2019 Parity Technologies (UK) Ltd.
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the "Software"),
// to deal in the Software without restriction, including without limitation
// the rights to use, copy, modify, merge, publish, distribute, sublicense,
// and/or sell copies of the Software, and to permit persons to whom the
// Software is furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
// FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

use futures_rustls::{rustls, TlsAcceptor, TlsConnector};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, TrustAnchor};
use std::convert::TryFrom;
use std::{fmt, io, sync::Arc};

/// TLS configuration.
#[derive(Clone)]
pub struct Config {
    pub(crate) client: TlsConnector,
    pub(crate) server: Option<TlsAcceptor>,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Config")
    }
}

/// Private key, DER-encoded ASN.1 in either PKCS#8 or PKCS#1 format.
pub struct PrivateKey(PrivateKeyDer<'static>);

impl Clone for PrivateKey {
    // `PrivateKeyDer` deliberately has no `Clone` impl; `clone_key` is its explicit duplicate.
    fn clone(&self) -> Self { PrivateKey(self.0.clone_key()) }
}

impl PrivateKey {
    /// Parse the given bytes as a DER-encoded ASN.1 private key in PKCS#8, PKCS#1 or SEC1 format.
    ///
    /// VENDOR PATCH: upstream panics (`expect("unknown or invalid key format")`) on a malformed
    /// key. A malformed key file is an operator input error and must not abort the process, so
    /// this returns an error instead.
    pub fn new(bytes: Vec<u8>) -> Result<Self, Error> {
        let key = PrivateKeyDer::try_from(bytes).map_err(|e| Error::Tls(e.into()))?;
        Ok(PrivateKey(key))
    }
}

/// Certificate, DER-encoded X.509 format.
#[derive(Debug, Clone)]
pub struct Certificate(CertificateDer<'static>);

impl Certificate {
    /// Assert the given bytes are in DER-encoded X.509 format.
    pub fn new(bytes: Vec<u8>) -> Self {
        Certificate(CertificateDer::from(bytes))
    }
}

impl Config {
    /// Create a new TLS configuration with the given server key and certificate chain.
    pub fn new<I>(key: PrivateKey, certs: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = Certificate>,
    {
        let mut builder = Config::builder();
        builder.server(key, certs)?;
        Ok(builder.finish())
    }

    /// Create a client-only configuration.
    pub fn client() -> Self {
        let client = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("the ring provider supports the default protocol versions")
            .with_root_certificates(client_root_store())
            .with_no_client_auth();
        Config {
            client: Arc::new(client).into(),
            server: None,
        }
    }

    /// Create a new TLS configuration builder.
    pub fn builder() -> Builder {
        Builder {
            client_root_store: client_root_store(),
            server: None,
        }
    }
}

/// Setup the rustls client configuration.
fn client_root_store() -> rustls::RootCertStore {
    let mut client_root_store = rustls::RootCertStore::empty();
    client_root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().map(|ta| TrustAnchor {
        subject: ta.subject.clone(),
        subject_public_key_info: ta.subject_public_key_info.clone(),
        name_constraints: ta.name_constraints.clone(),
    }));
    client_root_store
}

/// The crypto provider is always `ring`, passed explicitly. rustls 0.23 otherwise relies on a
/// process-wide default provider and panics at the first handshake when none or two are enabled.
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// TLS configuration builder.
pub struct Builder {
    client_root_store: rustls::RootCertStore,
    server: Option<rustls::ServerConfig>,
}

impl Builder {
    /// Set server key and certificate chain.
    pub fn server<I>(&mut self, key: PrivateKey, certs: I) -> Result<&mut Self, Error>
    where
        I: IntoIterator<Item = Certificate>,
    {
        let certs = certs.into_iter().map(|c| c.0).collect();
        let server = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .map_err(|e| Error::Tls(Box::new(e)))?
            .with_no_client_auth()
            .with_single_cert(certs, key.0)
            .map_err(|e| Error::Tls(Box::new(e)))?;
        self.server = Some(server);
        Ok(self)
    }

    /// Add an additional trust anchor.
    pub fn add_trust(&mut self, cert: &Certificate) -> Result<&mut Self, Error> {
        self.client_root_store
            .add(cert.0.clone())
            .map_err(|e| Error::Tls(Box::new(e)))?;
        Ok(self)
    }

    /// Finish configuration.
    pub fn finish(self) -> Config {
        let client = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .expect("the ring provider supports the default protocol versions")
            .with_root_certificates(self.client_root_store)
            .with_no_client_auth();

        Config {
            client: Arc::new(client).into(),
            server: self.server.map(|s| Arc::new(s).into()),
        }
    }
}

pub(crate) fn dns_name_ref(name: &str) -> Result<ServerName<'static>, Error> {
    ServerName::try_from(String::from(name)).map_err(|_| Error::InvalidDnsName(name.into()))
}

// Error //////////////////////////////////////////////////////////////////////////////////////////

/// TLS related errors.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// An underlying I/O error.
    Io(io::Error),
    /// Actual TLS error.
    Tls(Box<dyn std::error::Error + Send + Sync>),
    /// The DNS name was invalid.
    InvalidDnsName(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "i/o error: {e}"),
            Error::Tls(e) => write!(f, "tls error: {e}"),
            Error::InvalidDnsName(n) => write!(f, "invalid DNS name: {n}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            Error::Tls(e) => Some(&**e),
            Error::InvalidDnsName(_) => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

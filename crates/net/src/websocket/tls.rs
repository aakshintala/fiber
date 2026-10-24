//! TLS configuration shared with the HTTPS connector's trust-store decision.
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use std::io::{self, Cursor};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex};

/// Builds a TLS config using the same OS trust-store policy as HTTPS calls.
pub fn client_config() -> Arc<ClientConfig> {
    configured(
        crate::empty_store_with(&crate::STORE_IS_EMPTY, crate::platform_certificate_count),
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned()),
    )
}

fn configured(empty: bool, fallback: rustls::RootCertStore) -> Arc<ClientConfig> {
    let builder = ClientConfig::builder();
    let config = if empty {
        builder
            .with_root_certificates(fallback)
            .with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(Platform))
            .with_no_client_auth()
    };
    Arc::new(config)
}

// Initialising the OS verifier can fail. Delay it to certificate verification,
// where rustls can return that failure instead of an infallible config panicking.
#[derive(Debug)]
struct Platform;
impl ServerCertVerifier for Platform {
    fn verify_server_cert(
        &self,
        end: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        rustls_platform_verifier::Verifier::new(Arc::new(rustls::crypto::ring::default_provider()))?
            .verify_server_cert(end, intermediates, name, ocsp, now)
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// One socket handle per pump; no blocking TCP call holds the TLS state lock.
pub(super) struct Socket {
    pub(super) tcp: TcpStream,
    shared: Arc<Shared>,
    prefix: Cursor<Vec<u8>>,
}
struct Shared {
    send: Mutex<()>,
    state: Option<Mutex<rustls::ClientConnection>>,
    #[cfg(test)]
    pause: Option<Arc<super::Pause>>,
}
impl Socket {
    pub(super) fn new(
        mut tcp: TcpStream,
        tail: Vec<u8>,
        endpoint: &super::Endpoint,
        via: &super::Via,
    ) -> Result<Self, crate::Error> {
        let (state, prefix) = if endpoint.secure {
            let name =
                ServerName::try_from(endpoint.host.clone()).map_err(|e| crate::Error::Tls {
                    reason: e.to_string(),
                })?;
            let mut state =
                rustls::ClientConnection::new(Arc::clone(&via.tls), name).map_err(|e| {
                    crate::Error::Tls {
                        reason: e.to_string(),
                    }
                })?;
            state.set_buffer_limit(None);
            if !tail.is_empty() {
                state.read_tls(&mut Cursor::new(tail))?;
                state.process_new_packets().map_err(|e| crate::Error::Tls {
                    reason: e.to_string(),
                })?;
            }
            while state.is_handshaking() {
                state.complete_io(&mut tcp).map_err(|e| {
                    if e.kind() == io::ErrorKind::InvalidData {
                        crate::Error::Tls {
                            reason: e.to_string(),
                        }
                    } else {
                        super::tunnel::io_error(e, via.limits.connect())
                    }
                })?;
            }
            (Some(Mutex::new(state)), Vec::new())
        } else {
            (None, tail)
        };
        Ok(Self {
            tcp,
            shared: Arc::new(Shared {
                send: Mutex::new(()),
                state,
                #[cfg(test)]
                pause: via.pause.clone(),
            }),
            prefix: Cursor::new(prefix),
        })
    }
    pub(super) fn try_clone(&self) -> io::Result<Self> {
        Ok(Self {
            tcp: self.tcp.try_clone()?,
            shared: Arc::clone(&self.shared),
            prefix: Cursor::new(Vec::new()),
        })
    }
    fn outgoing(&self, plaintext: Option<&[u8]>) -> io::Result<Vec<u8>> {
        let Some(state) = &self.shared.state else {
            return Ok(plaintext.unwrap_or_default().to_vec());
        };
        let mut state = support::lock(state);
        if let Some(bytes) = plaintext {
            state.writer().write_all(bytes)?;
        }
        let mut records = Vec::new();
        while state.wants_write() {
            state.write_tls(&mut records)?;
        }
        Ok(records)
    }
    fn send(&self, plaintext: Option<&[u8]>) -> io::Result<()> {
        let _send = support::lock(&self.shared.send);
        let records = self.outgoing(plaintext)?;
        #[cfg(test)]
        if std::thread::current().name() == Some("net-ws-out")
            && let Some(pause) = &self.shared.pause
        {
            pause.wait();
        }
        (&self.tcp).write_all(&records)
    }
}
impl Read for Socket {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let Some(state) = &self.shared.state else {
            let n = self.prefix.read(bytes)?;
            return if n == 0 { self.tcp.read(bytes) } else { Ok(n) };
        };
        loop {
            let read = support::lock(state).reader().read(bytes);
            match read {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            // rustls may consume only part of a TCP read. Keep its remainder
            // until every record and its plaintext have been drained.
            if self.prefix.position()
                == u64::try_from(self.prefix.get_ref().len()).unwrap_or(u64::MAX)
            {
                let mut encrypted = [0; 16_384];
                let n = self.tcp.read(&mut encrypted)?;
                self.prefix = Cursor::new(encrypted.get(..n).unwrap_or_default().to_vec());
            }
            let pending = {
                let mut state = support::lock(state);
                // Feed TCP EOF too: rustls distinguishes close_notify from truncation.
                state.read_tls(&mut self.prefix)?;
                state.process_new_packets().map_err(io::Error::other)?;
                state.wants_write()
            };
            if pending {
                self.send(None)?;
            }
        }
    }
}
impl Write for Socket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.send(Some(bytes))?;
        Ok(bytes.len())
    }
    // Writes already drain TLS records and complete their TCP writes; flush has no output.
    #[cfg_attr(false, mutants::skip)]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "tls_tests.rs"]
pub(crate) mod tests;

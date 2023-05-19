use axum_server::tls_rustls::RustlsConfig;
use hyper::{http::uri::Scheme, Uri};
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    ParseError(#[from] config::ConfigError),
    #[error("invalid uri: {0}")]
    InvalidUri(#[from] hyper::http::uri::InvalidUri),
    #[error(transparent)]
    IOError(#[from] std::io::Error),
}

/// Configuration information describing a helper network.
///
/// The most important thing this contains is discovery information for each of the participating
/// helpers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Information about each helper participating in the network. The order that helpers are
    /// listed here determines their assigned helper identities in the network. Note that while the
    /// helper identities are stable, roles are assigned per query.
    pub peers: [PeerConfig; 3],
}

impl NetworkConfig {
    /// Reads config from string. Expects config to be toml format.
    /// To read file, use `fs::read_to_string`
    ///
    /// # Errors
    /// if `input` is in an invalid format
    pub fn from_toml_str(input: &str) -> Result<Self, Error> {
        use config::{Config, File, FileFormat};

        let conf: Self = Config::builder()
            .add_source(File::from_str(input, FileFormat::Toml))
            .build()?
            .try_deserialize()?;

        Ok(conf)
    }

    pub fn peers(&self) -> &[PeerConfig; 3] {
        &self.peers
    }

    /// # Panics
    /// If `PathAndQuery::from_str("")` fails
    #[must_use]
    pub fn override_scheme(self, scheme: &Scheme) -> NetworkConfig {
        NetworkConfig {
            peers: self.peers.map(|mut peer| {
                let mut parts = peer.url.into_parts();
                parts.scheme = Some(scheme.clone());
                // `http::uri::Uri::from_parts()` requires that a URI have a path if it has a
                // scheme. If the URI does not have a scheme, it is not required to have a path.
                if parts.path_and_query.is_none() {
                    parts.path_and_query = Some("".parse().unwrap());
                }
                peer.url = Uri::try_from(parts).unwrap();
                peer
            }),
        }
    }
}

#[cfg(test)]
impl Default for NetworkConfig {
    fn default() -> Self {
        NetworkConfig {
            peers: [
                PeerConfig::new("localhost:3000".parse().unwrap()),
                PeerConfig::new("localhost:3001".parse().unwrap()),
                PeerConfig::new("localhost:3002".parse().unwrap()),
            ],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerConfig {
    /// Peer URL
    #[serde(with = "crate::uri")]
    pub url: Uri,

    /// Peer's TLS certificate or CA in PEM format
    ///
    /// If the peer's TLS certificate can be verified using the system truststore, this may be omitted.
    ///
    /// If the peer's TLS certificate cannot be verified using the system truststore, or for a stronger
    /// check that the peer uses the expected PKI, either the peer certificate or the authority
    /// certificate that issues the peer's certificate may be specified here.
    ///
    /// If a certificate is specified here, only the specified certificate will be accepted. The system
    /// truststore will not be used.
    pub certificate: Option<String>,
    /// public key which should be used to encrypt match keys
    pub matchkey_encryption_key: Option<String>,
}

impl PeerConfig {
    pub fn new(url: Uri) -> Self {
        Self {
            url,
            certificate: None,
            matchkey_encryption_key: None,
        }
    }

    /// Returns `PeerConfig` for talking to the default self-signed server test cert.
    /// # Errors
    /// if cert is invalid
    /// # Panics
    /// never, but clippy doesn't understand that
    #[must_use]
    #[cfg(any(test, feature = "self-signed-certs"))]
    pub fn https_self_signed(port: u16, matchkey_encryption: bool) -> PeerConfig {
        PeerConfig {
            url: format!("https://localhost:{port}").parse().unwrap(),
            certificate: Some(TEST_CERT.to_owned()),
            matchkey_encryption_key: if matchkey_encryption {
                Some(TEST_MATCHKEY_ENCRYPTION_KEY.to_owned())
            } else {
                None
            },
        }
    }
}

/*
 * TODO(tls): delete this if not needed when TLS and config work is finished

fn pk_from_str<'de, D>(deserializer: D) -> Result<PublicKey, D::Error>
where
    D: Deserializer<'de>,
{
    let s: String = Deserialize::deserialize(deserializer)?;
    let mut buf = [0_u8; 32];
    hex::decode_to_slice(s, &mut buf).map_err(<D::Error as serde::de::Error>::custom)?;

    Ok(PublicKey::from(buf))
}
*/

#[derive(Clone, Debug)]
pub enum TlsConfig {
    File {
        /// Path to file containing certificate
        certificate_file: PathBuf,

        /// Path to file containing private key
        private_key_file: PathBuf,
    },
    Inline {
        /// Certificate in PEM format
        certificate: String,

        // Private key in PEM format
        private_key: String,
    },
}

#[derive(Clone, Debug)]
pub enum MatchKeyEncryptionConfig {
    File {
        /// Path to file containing public key which encrypts match keys
        public_key_file: PathBuf,

        /// Path to file containing private key which decrypts match keys
        private_key_file: PathBuf,
    },
    Inline {
        /// Public key in hex format
        public_key: String,

        // Private key in hex format
        private_key: String,
    },
}

/// Configuration information for launching an instance of the helper party web service.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Port to listen. If not specified, will ask Kernel to assign the port
    pub port: Option<u16>,

    /// If true, use insecure HTTP. Otherwise (default), use HTTPS.
    pub disable_https: bool,

    /// TLS configuration for helper-to-helper communication
    pub tls: Option<TlsConfig>,

    /// Configuration needed for encrypting and decrypting match keys
    pub matchkey_encryption_info: Option<MatchKeyEncryptionConfig>,
}

impl ServerConfig {
    #[must_use]
    #[cfg(any(test, feature = "test-fixture"))]
    pub fn insecure_http(matchkey_encryption: bool) -> ServerConfig {
        ServerConfig {
            port: None,
            disable_https: true,
            tls: None,
            matchkey_encryption_info: Self::get_dummy_matchkey_encryption_info(matchkey_encryption),
        }
    }

    #[cfg(any(test, feature = "test-fixture"))]
    fn get_dummy_matchkey_encryption_info(
        matchkey_encryption: bool,
    ) -> Option<MatchKeyEncryptionConfig> {
        if matchkey_encryption {
            Some(MatchKeyEncryptionConfig::Inline {
                public_key: TEST_MATCHKEY_ENCRYPTION_KEY.to_owned(),
                private_key: TEST_MATCHKEY_DECRYPTION_KEY.to_owned(),
            })
        } else {
            None
        }
    }

    #[must_use]
    #[cfg(any(test, feature = "test-fixture"))]
    pub fn insecure_http_port(port: u16, matchkey_encryption: bool) -> ServerConfig {
        ServerConfig {
            port: Some(port),
            disable_https: true,
            tls: None,
            matchkey_encryption_info: Self::get_dummy_matchkey_encryption_info(matchkey_encryption),
        }
    }

    /// Returns `ServerConfig` instance configured with self-signed cert and key. Not intended to
    /// use in production, therefore it is hidden behind a feature flag.
    /// # Errors
    /// if cert is invalid
    #[must_use]
    #[cfg(any(test, feature = "self-signed-certs"))]
    pub fn https_self_signed(matchkey_encryption: bool) -> ServerConfig {
        ServerConfig {
            port: None,
            disable_https: false,
            tls: Some(TlsConfig::Inline {
                certificate: TEST_CERT.to_owned(),
                private_key: TEST_KEY.to_owned(),
            }),
            matchkey_encryption_info: Self::get_dummy_matchkey_encryption_info(matchkey_encryption),
        }
    }

    /// Create a `RustlsConfig` for the `ServerConfig`.
    ///
    /// # Errors
    /// If there is a problem with the TLS configuration.
    pub async fn as_rustls_config(&self) -> io::Result<RustlsConfig> {
        match &self.tls {
            None => {
                // Using io::Error for this would not be my first choice, but it's
                // what the axum RustlsConfig::from_* routines do as well.
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "missing TLS configuration",
                ))
            }
            Some(TlsConfig::Inline {
                certificate,
                private_key,
            }) => {
                RustlsConfig::from_pem(
                    certificate.as_bytes().to_owned(),
                    private_key.as_bytes().to_owned(),
                )
                .await
            }
            Some(TlsConfig::File {
                certificate_file,
                private_key_file,
            }) => RustlsConfig::from_pem_file(&certificate_file, &private_key_file).await,
        }
    }
}

// This is here because it can be activated outside of tests with the
// `self-signed-certs` feature. It can probably be made test-only
// and moved to `crate::net::test`.
#[cfg(any(test, feature = "self-signed-certs"))]
const TEST_CERT: &str = "\
-----BEGIN CERTIFICATE-----
MIIBlDCCATugAwIBAgIICJ+d1TBXe0AwCgYIKoZIzj0EAwIwFDESMBAGA1UEAwwJ
bG9jYWxob3N0MB4XDTIzMDMyODAwMDIwOVoXDTIzMDYyNzAwMDIwOVowFDESMBAG
A1UEAwwJbG9jYWxob3N0MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEbuhfFs0U
Qae5KoQuCNBaJ81cpIWntGXSbaxJxkXNERkgcD9zf35HBAM7j8NYr3Kjh+W1lz80
qj6kHwAzq3fJSqN3MHUwFAYDVR0RBA0wC4IJbG9jYWxob3N0MA4GA1UdDwEB/wQE
AwICpDAdBgNVHSUEFjAUBggrBgEFBQcDAQYIKwYBBQUHAwIwHQYDVR0OBBYEFFvf
qKaSDivAf1+1H3wkItW8+GumMA8GA1UdEwEB/wQFMAMBAf8wCgYIKoZIzj0EAwID
RwAwRAIgBqQPA/TAIh0J4GqUuclWkyDIZbaoUXSYbM4tYM//clMCIAaEHKVK5krK
MEv5kZ1e2xkmEQ+b3v7cAy3d58SjhW+v
-----END CERTIFICATE-----
";

#[cfg(any(test, feature = "self-signed-certs"))]
const TEST_KEY: &str = "\
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg2ZJo2GQ7gbCrj2PC
zQVb6BVsrGhV6E3GrDIAerI/HbKhRANCAARu6F8WzRRBp7kqhC4I0FonzVykhae0
ZdJtrEnGRc0RGSBwP3N/fkcEAzuPw1ivcqOH5bWXPzSqPqQfADOrd8lK
-----END PRIVATE KEY-----
";

#[cfg(any(test, feature = "test-fixture"))]
const TEST_MATCHKEY_ENCRYPTION_KEY: &str = "\
0ef21c2f73e6fac215ea8ec24d39d4b77836d09b1cf9aeb2257ddd181d7e663d
";

#[cfg(any(test, feature = "test-fixture"))]
const TEST_MATCHKEY_DECRYPTION_KEY: &str = "\
a0778c3e9960576cbef4312a3b7ca34137880fd588c11047bd8b6a8b70b5a151
";

#[cfg(all(test, not(feature = "shuttle"), feature = "in-memory-infra"))]
mod tests {
    use crate::{helpers::HelperIdentity, test_fixture::config::TestConfigBuilder};
    use hyper::Uri;

    const URI_1: &str = "http://localhost:3000";
    const URI_2: &str = "http://localhost:3001";
    const URI_3: &str = "http://localhost:3002";

    #[allow(dead_code)] // TODO(tls) need to add back report public key configuration
    fn hex_str_to_public_key(hex_str: &str) -> x25519_dalek::PublicKey {
        let pk_bytes: [u8; 32] = hex::decode(hex_str)
            .expect("valid hex string")
            .try_into()
            .expect("hex should be exactly 32 bytes");
        pk_bytes.into()
    }

    #[test]
    fn parse_config() {
        let conf = TestConfigBuilder::with_default_test_ports().build();

        let uri1 = URI_1.parse::<Uri>().unwrap();
        let id1 = HelperIdentity::try_from(1usize).unwrap();
        let value1 = &conf.network.peers()[id1];
        assert_eq!(value1.url, uri1);

        let uri2 = URI_2.parse::<Uri>().unwrap();
        let id2 = HelperIdentity::try_from(2usize).unwrap();
        let value2 = &conf.network.peers()[id2];
        assert_eq!(value2.url, uri2);

        let uri3 = URI_3.parse::<Uri>().unwrap();
        let id3 = HelperIdentity::try_from(3usize).unwrap();
        let value3 = &conf.network.peers()[id3];
        assert_eq!(value3.url, uri3);
    }
}

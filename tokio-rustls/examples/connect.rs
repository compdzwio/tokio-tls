use std::sync::Arc;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use tokio_rustls_fork_shadow_tls::TlsConnector;
use rustls_fork_shadow_tls::{OwnedTrustAnchor, RootCertStore};

#[tokio::main]
async fn main() {
    let mut root_store = RootCertStore::empty();
    root_store.add_server_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.0.iter().map(|ta| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
            ta.subject,
            ta.spki,
            ta.name_constraints,
        )
    }));
    let config = rustls_fork_shadow_tls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let connector = TlsConnector::from(Arc::new(config));
    let stream = TcpStream::connect("rsproxy.cn:443").await.unwrap();
    println!("rsproxy.cn:443 connected");

    let domain = rustls_fork_shadow_tls::ServerName::try_from("rsproxy.cn").unwrap();
    let mut stream = connector.connect(domain, stream).await.unwrap();
    println!("handshake success");

    let content = b"GET / HTTP/1.0\r\nHost: rsproxy.cn\r\n\r\n";
    stream.write_all(content).await?;
    println!("http request sent");

    let buf = vec![0_u8; 64];
    let n = stream.read(buf).await?;
    let resp = String::from_utf8(buf).unwrap();
    println!("http response recv: \n\n{resp}");
}

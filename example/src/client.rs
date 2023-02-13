use std::io::Cursor;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use tokio_rustls_fork_shadow_tls::TlsConnector;
use rustls_fork_shadow_tls::{Certificate, OwnedTrustAnchor, RootCertStore};
use rustls_pemfile::certs;

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
    root_store
        .add(&read_ca_certs())
        .expect("unable to trust self-signed CA");
    let config = rustls_fork_shadow_tls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let connector = TlsConnector::from(config);
    let stream = TcpStream::connect("127.0.0.1:50443").await.unwrap();
    println!("127.0.0.1:50443 connected");

    let domain = rustls_fork_shadow_tls::ServerName::try_from("monoio.rs").unwrap();
    let mut stream = connector.connect(domain, stream).await.unwrap();
    println!("handshake success");

    let data = "hello world";
    stream.write_all(data.as_bytes()).await.expect("unable to send data");
    println!("send data: {data}");
    let mut buf = vec![0; data.len()];
    let buf = buf.as_mut_slice();
    let res = stream.read(buf).await;
    assert!(res.is_ok(), "unable to recv data");
    println!(
        "recv data: {}",
        String::from_utf8(buf.to_vec()).expect("invalid data")
    );
    let _ = stream.shutdown().await;
}

fn read_ca_certs() -> Certificate {
    let mut ca_cursor = Cursor::new(include_bytes!("../certs/rootCA.crt"));
    let ca_data = certs(&mut ca_cursor).unwrap().pop().unwrap();
    Certificate(ca_data)
}

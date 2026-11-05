//! HTTPS must negotiate the protocols accepted by the request server.

use std::sync::Arc;

#[tokio::test]
async fn certificate_listener_negotiates_http2_and_http1() -> anyhow::Result<()> {
    drop(rustls::crypto::ring::default_provider().install_default());
    let fixture = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])?;
    let directory = tempfile::tempdir()?;
    let cert = directory.path().join("certificate.pem");
    let key = directory.path().join("private-key.pem");
    std::fs::write(&cert, fixture.cert.pem())?;
    std::fs::write(&key, fixture.signing_key.serialize_pem())?;
    let (acceptor, _) = super::load_pem_as_acceptor(&cert, &key)?;
    let mut roots = rustls::RootCertStore::empty();
    roots.add(fixture.cert.der().clone())?;
    for protocol in [b"h2".as_slice(), b"http/1.1".as_slice()] {
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(roots.clone())
            .with_no_client_auth();
        config.alpn_protocols = vec![protocol.to_vec()];
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let (client_io, server_io) = tokio::io::duplex(4096);
        let (client, server) = tokio::try_join!(
            connector.connect("localhost".try_into()?, client_io),
            acceptor.accept(server_io)
        )?;
        anyhow::ensure!(client.get_ref().1.alpn_protocol() == Some(protocol));
        anyhow::ensure!(server.get_ref().1.alpn_protocol() == Some(protocol));
    }
    Ok(())
}

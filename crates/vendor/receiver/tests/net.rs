//! Connection-glue test: drive `serve_connection` over an in-memory duplex stream with a plaintext
//! OPTIONS request, asserting the read→feed→write loop returns the routed 200 response.

use std::io::{Cursor, Read, Write};

use mfi::auth_client::MfiSigner;
use pairing::setup::PeerSaver;
use pairing::verify::{Identity, PeerStore};
use receiver::info::{build_info, DeviceConfig};
use receiver::net::serve_connection;
use receiver::server::ControlServer;

struct NoStore;
impl PeerStore for NoStore {
    fn find_peer(&self, _id: &[u8]) -> Option<[u8; 32]> {
        None
    }
}
impl PeerSaver for NoStore {
    fn save_peer(&mut self, _id: &[u8], _ltpk: [u8; 32]) {}
}

struct NoSigner;
impl MfiSigner for NoSigner {
    fn copy_certificate(&mut self) -> std::io::Result<Vec<u8>> {
        Ok(Vec::new())
    }
    fn create_signature(&mut self, _d: &[u8]) -> std::io::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

/// A one-shot in-memory stream: serves `input` to reads, captures writes, EOFs when drained.
struct MockStream {
    input: Cursor<Vec<u8>>,
    output: Vec<u8>,
}
impl Read for MockStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.input.read(buf)
    }
}
impl Write for MockStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn serve_connection_answers_options() {
    let acc = Identity::new(b"acc".to_vec(), [9u8; 32]);
    let info = build_info(&DeviceConfig::default());
    let mut server = ControlServer::new(&acc, b"3939".to_vec(), NoStore, NoSigner, info);

    let req = b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n\r\n".to_vec();
    let mut stream = MockStream { input: Cursor::new(req), output: Vec::new() };
    serve_connection(&mut stream, &mut server).unwrap();

    let text = String::from_utf8_lossy(&stream.output);
    assert!(text.starts_with("RTSP/1.0 200 OK\r\n"), "got: {text:?}");
    assert!(text.contains("CSeq: 1\r\n"));
    assert!(text.contains("Public:"), "OPTIONS advertises methods");
}

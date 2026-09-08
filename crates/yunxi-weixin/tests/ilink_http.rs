use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};

use base64::Engine as _;
use yunxi_weixin::{
    IlinkHttpConfig, IlinkHttpTransport, IlinkMessage, IlinkTransport, MemorySecretStore,
    RequestContext, SecretMaterial, SecretRef, SecretStore,
};

fn start_server(responses: Vec<&'static str>) -> (String, JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
    let endpoint = format!(
        "http://{}/",
        listener.local_addr().expect("listener address")
    );
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        for body in responses {
            let (mut stream, _) = listener.accept().expect("accept request");
            let request = read_request(&mut stream);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
            requests.push(request);
        }
        requests
    });
    (endpoint, handle)
}

fn read_request(stream: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).expect("read request");
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_end = header_end + 4;
            let header = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = header
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")
                        .or_else(|| line.strip_prefix("content-length:"))
                })
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if bytes.len() >= header_end.saturating_add(content_length) {
                break;
            }
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn http_transport_matches_ilink_paths_headers_and_bounded_models() {
    let (endpoint, server) = start_server(vec![
        r#"{"qrcode":"qr-1","qrcode_img_content":"image"}"#,
        r#"{"status":"confirmed","ilink_bot_token":"issued-token"}"#,
        r#"{"ret":0,"get_updates_buf":"cursor-2","msgs":[{"message_id":123,"from_user_id":"peer-1","item_list":[{"type":1,"text_item":{"text":"hello"},"is_completed":true}]}]}"#,
        r#"{"ret":0}"#,
    ]);
    let store = MemorySecretStore::new();
    let token_ref = SecretRef::new("host:weixin/token").expect("token reference");
    store
        .put(
            token_ref.clone(),
            SecretMaterial::from_text("stored-token").expect("token"),
        )
        .expect("store token");
    let config = IlinkHttpConfig::for_loopback(endpoint, "test-account").expect("config");
    let mut transport =
        IlinkHttpTransport::new(config.with_token_ref(token_ref), &store).expect("transport");
    let context = RequestContext::new();

    let challenge = transport.fetch_qr(&context).expect("fetch QR");
    assert_eq!(challenge.qrcode, "qr-1");
    let qr_poll = transport
        .poll_qr_status(&challenge.qrcode, Some("123456"), &context)
        .expect("poll QR");
    assert!(qr_poll.auth_token.is_some());
    let batch = transport
        .get_updates("cursor-1", &context)
        .expect("updates");
    assert_eq!(batch.cursor, "cursor-2");
    assert_eq!(batch.messages.len(), 1);
    assert_eq!(batch.messages[0].message_id, "123");
    let message = IlinkMessage::text("out-1", "bot", "reply").expect("message");
    transport.send_message(&message, &context).expect("send");

    let requests = server.join().expect("server");
    assert!(requests[0].starts_with("GET /ilink/bot/get_bot_qrcode?bot_type=3 HTTP/1.1"));
    assert!(
        requests[1].starts_with(
            "GET /ilink/bot/get_qrcode_status?qrcode=qr-1&verify_code=123456 HTTP/1.1"
        )
    );
    assert!(requests[2].contains("POST /ilink/bot/getupdates HTTP/1.1"));
    assert!(requests[2].contains("authorization: Bearer stored-token"));
    let uin = requests[2]
        .lines()
        .find_map(|line| line.strip_prefix("x-wechat-uin: "))
        .expect("random X-WECHAT-UIN header");
    assert_ne!(uin, "0");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(uin)
        .expect("base64 UIN");
    let number = String::from_utf8(decoded).expect("decimal UIN");
    assert!(number.parse::<u32>().is_ok());
    assert!(requests[2].contains("\"get_updates_buf\":\"cursor-1\""));
    assert!(requests[3].contains("POST /ilink/bot/sendmessage HTTP/1.1"));
}

#[test]
fn http_transport_accepts_observed_cursor_and_message_aliases() {
    let (endpoint, server) = start_server(vec![
        r#"{"ret":0,"next_key":"cursor-2","messages":[{"message_id":"m-1","from_user_id":"peer-1","item_list":[{"type":1,"text_item":{"text":"hello"}}]}]}"#,
    ]);
    let store = MemorySecretStore::new();
    let token_ref = SecretRef::new("host:weixin/token").expect("token reference");
    store
        .put(
            token_ref.clone(),
            SecretMaterial::from_text("stored-token").expect("token"),
        )
        .expect("store token");
    let config = IlinkHttpConfig::for_loopback(endpoint, "test-account")
        .expect("config")
        .with_token_ref(token_ref);
    let mut transport = IlinkHttpTransport::new(config, &store).expect("transport");
    let batch = transport
        .get_updates("cursor-1", &RequestContext::new())
        .expect("updates");
    assert_eq!(batch.cursor, "cursor-2");
    assert_eq!(batch.messages[0].message_id, "m-1");
    server.join().expect("server");
}

#[test]
fn http_transport_rejects_a_response_over_its_configured_bound() {
    const OVERSIZED: &str =
        "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx";
    let (endpoint, server) = start_server(vec![OVERSIZED]);
    let store = MemorySecretStore::new();
    let config = IlinkHttpConfig::for_loopback(endpoint, "test-account")
        .expect("config")
        .with_response_limit(64)
        .expect("response limit");
    let mut transport = IlinkHttpTransport::new(config, &store).expect("transport");
    let error = transport
        .fetch_qr(&RequestContext::new())
        .expect_err("oversized response");
    assert!(matches!(
        error,
        yunxi_weixin::IlinkError::ResponseTooLarge { maximum: 64 }
    ));
    server.join().expect("server");
}

#[test]
fn http_transport_surfaces_session_expiry_as_a_relogin_signal() {
    let (endpoint, server) = start_server(vec![r#"{"ret":-14,"errmsg":"session timeout"}"#]);
    let store = MemorySecretStore::new();
    let token_ref = SecretRef::new("host:weixin/token").expect("token reference");
    store
        .put(
            token_ref.clone(),
            SecretMaterial::from_text("stored-token").expect("token"),
        )
        .expect("store token");
    let config = IlinkHttpConfig::for_loopback(endpoint, "test-account")
        .expect("config")
        .with_token_ref(token_ref);
    let mut transport = IlinkHttpTransport::new(config, &store).expect("transport");
    assert!(matches!(
        transport.get_updates("", &RequestContext::new()),
        Err(yunxi_weixin::IlinkError::SessionExpired)
    ));
    server.join().expect("server");
}

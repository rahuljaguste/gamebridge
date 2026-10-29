// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! End-to-end pipeline test against a mock console speaking the real wire
//! protocol (registration is covered by vectors; this exercises session,
//! ctrl, Takion, crypto, launch spec, media ingest and feedback).

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio::time::timeout;

use chiaki_core::crypto::ecdh::Ecdh;
use chiaki_core::crypto::gkcrypt::GKCrypt;
use chiaki_core::crypto::rpcrypt::RPCrypt;
use chiaki_core::ctrl::{MSG_LOGIN, MSG_LOGIN_PIN_REP, MSG_LOGIN_PIN_REQ, MSG_SESSION_ID};
use chiaki_core::error::QuitReason;
use chiaki_core::proto::{Reader, Writer, as_bytes, as_string};
use chiaki_core::session::{ConnectInfo, Session, SessionCommand, SessionEvent, SessionOptions};
use chiaki_core::stream::protobuf;
use chiaki_core::takion::KeyState;
use chiaki_core::takion::packet::{
    CHUNK_TYPE_COOKIE_ACK, CHUNK_TYPE_DATA, PACKET_TYPE_CONTROL, base_type, parse_message,
    write_message_header,
};
use chiaki_core::target::{Fps, Resolution, Target, VideoProfile};
use chiaki_core::util::base64;

const NONCE: [u8; 16] = [0x33; 16];
const REGIST_KEY: [u8; 16] = [0x11; 16];
const MORNING: [u8; 16] = [0x22; 16];
const MOCK_TAG: u32 = 0x1234_5678;
const TINY_AU_B64: &str = include_str!("vectors/tiny_h264.b64");

fn build_control_data(
    tag: u32,
    crypt: Option<&mut GKCrypt>,
    key_pos: u64,
    seq: u32,
    flags: u8,
    channel: u16,
    payload: &[u8],
) -> Vec<u8> {
    let mut packet = vec![0u8; 1 + 16 + 9 + payload.len()];
    packet[0] = PACKET_TYPE_CONTROL;
    write_message_header(
        &mut packet[1..],
        tag,
        key_pos,
        CHUNK_TYPE_DATA,
        flags,
        9 + payload.len(),
    );
    let p = &mut packet[1 + 16..];
    p[0..4].copy_from_slice(&seq.to_be_bytes());
    p[4..6].copy_from_slice(&channel.to_be_bytes());
    p[6..8].fill(0);
    p[8] = 0;
    p[9..].copy_from_slice(payload);
    if let Some(crypt) = crypt {
        let tag = control_mac(crypt, key_pos, &mut packet);
        packet[5..9].copy_from_slice(&tag);
    }
    packet
}

fn control_mac(crypt: &mut GKCrypt, key_pos: u64, packet: &mut [u8]) -> [u8; 4] {
    let saved_key = [packet[9], packet[10], packet[11], packet[12]];
    packet[5..9].fill(0);
    packet[9..13].fill(0);
    let tag = crypt.gmac(key_pos, packet).unwrap();
    packet[9..13].copy_from_slice(&saved_key);
    tag
}

fn verify_control_mac(crypt: &mut GKCrypt, key_state: &mut KeyState, packet: &mut [u8]) -> bool {
    let low = u32::from_be_bytes([packet[9], packet[10], packet[11], packet[12]]);
    let key_pos = key_state.request_pos(low, false);
    let actual = [packet[5], packet[6], packet[7], packet[8]];
    let expected = control_mac(crypt, key_pos, packet);
    if actual != expected {
        return false;
    }
    key_state.commit(key_pos);
    true
}

async fn tcp_console(listener: TcpListener, crypt: RPCrypt) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            break;
        };
        let crypt = crypt.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            let header_end = loop {
                let Ok(n) = stream.read(&mut chunk).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            };
            let head = String::from_utf8_lossy(&buf[..header_end]);
            if head.starts_with("GET /sie/ps4/rp/sess/init") {
                let nonce = base64::encode(&NONCE);
                let response =
                    format!("HTTP/1.1 200 OK\r\nRP-Nonce: {nonce}\r\nContent-Length: 0\r\n\r\n");
                let _ = stream.write_all(response.as_bytes()).await;
            } else if head.starts_with("GET /sie/ps4/rp/sess/ctrl") {
                let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                let _ = stream.write_all(response.as_bytes()).await;
                let mut remote_counter = 0u64;
                // Ask for a PIN, reject the first attempt, accept the second.
                write_ctrl_frame(
                    &mut stream,
                    MSG_LOGIN_PIN_REQ,
                    &[],
                    &crypt,
                    &mut remote_counter,
                )
                .await;
                for attempt in 0..2 {
                    let Some((ty, _)) = read_ctrl_frame(&mut stream).await else {
                        return;
                    };
                    if ty != MSG_LOGIN_PIN_REP {
                        return;
                    }
                    let status = if attempt == 0 { 1 } else { 0 };
                    write_ctrl_frame(
                        &mut stream,
                        MSG_LOGIN,
                        &[status],
                        &crypt,
                        &mut remote_counter,
                    )
                    .await;
                    if status == 0 {
                        break;
                    }
                }
                let mut payload = vec![0x4a];
                payload.extend_from_slice(b"TEST1234567890ABCDEFGHIJ");
                write_ctrl_frame(
                    &mut stream,
                    MSG_SESSION_ID,
                    &payload,
                    &crypt,
                    &mut remote_counter,
                )
                .await;
                // Keep the connection open and discard anything else.
                let mut sink = [0u8; 1024];
                loop {
                    match stream.read(&mut sink).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }
            }
        });
    }
}

async fn write_ctrl_frame(
    stream: &mut tokio::net::TcpStream,
    ty: u16,
    payload: &[u8],
    crypt: &RPCrypt,
    counter: &mut u64,
) {
    let mut enc = payload.to_vec();
    if !payload.is_empty() {
        crypt.encrypt(*counter, &mut enc).unwrap();
        *counter += 1;
    }
    let mut header = [0u8; 8];
    header[0..4].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    header[4..6].copy_from_slice(&ty.to_be_bytes());
    stream.write_all(&header).await.unwrap();
    if !enc.is_empty() {
        stream.write_all(&enc).await.unwrap();
    }
}

async fn read_ctrl_frame(stream: &mut tokio::net::TcpStream) -> Option<(u16, Vec<u8>)> {
    let mut header = [0u8; 8];
    stream.read_exact(&mut header).await.ok()?;
    let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    let ty = u16::from_be_bytes([header[4], header[5]]);
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await.ok()?;
    Some((ty, payload))
}

fn parse_init(buffer: &[u8]) -> Option<u32> {
    if buffer.len() != 33 || buffer[0] != PACKET_TYPE_CONTROL {
        return None;
    }
    let message = &buffer[1..];
    if message[12] != 1 {
        return None; // INIT
    }
    Some(u32::from_be_bytes([
        message[16],
        message[17],
        message[18],
        message[19],
    ]))
}

async fn recv_message<F>(
    udp: &UdpSocket,
    tag_local: u32,
    key_state: &mut KeyState,
    mut crypt: Option<&mut GKCrypt>,
    filter: F,
) -> Option<Vec<u8>>
where
    F: Fn(&[u8]) -> bool,
{
    let mut buf = [0u8; 65535];
    for _ in 0..128 {
        let n = timeout(Duration::from_secs(5), udp.recv(&mut buf))
            .await
            .ok()?
            .ok()?;
        let mut packet = buf[..n].to_vec();
        if base_type(&packet).ok()? != PACKET_TYPE_CONTROL {
            continue;
        }
        let header = parse_message(&packet[1..], tag_local, key_state).ok()?;
        if header.chunk_type != CHUNK_TYPE_DATA {
            continue;
        }
        let payload_start = 1 + header.payload_offset;
        let payload_end = payload_start + header.payload_size;
        if payload_start + 9 > payload_end {
            continue;
        }
        let data_start = payload_start + 9;
        let data_type = packet[data_start - 1];
        // A BIG retransmission can arrive after the client installed its
        // crypt keys; it was built before encryption, so ignore it. The
        // first BIG (before any crypt keys exist) must be accepted.
        if crypt.is_some() && data_type == 0 {
            let data = &packet[data_start..payload_end];
            if protobuf::message_type(data) == Ok(protobuf::TYPE_BIG) {
                continue;
            }
        }
        if let Some(crypt) = crypt.as_deref_mut()
            && !verify_control_mac(crypt, key_state, &mut packet)
        {
            panic!("mock: client MAC verification failed (data_type {data_type:#x})");
        }
        let data = &packet[data_start..payload_end];
        if filter(data) {
            return Some(data.to_vec());
        }
    }
    None
}

async fn udp_console(udp: UdpSocket, _crypt: RPCrypt) {
    let mut buf = [0u8; 2048];
    let (n, peer) = timeout(Duration::from_secs(5), udp.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    udp.connect(peer).await.unwrap();
    let client_tag = parse_init(&buf[..n]).expect("INIT");
    let mut init_ack = vec![0u8; 1 + 16 + 0x30];
    init_ack[0] = PACKET_TYPE_CONTROL;
    write_message_header(&mut init_ack[1..], client_tag, 0, 2, 0, 0x30);
    let p = &mut init_ack[17..];
    p[0..4].copy_from_slice(&MOCK_TAG.to_be_bytes());
    p[4..8].copy_from_slice(&0x19000u32.to_be_bytes());
    p[8..10].copy_from_slice(&0x64u16.to_be_bytes());
    p[10..12].copy_from_slice(&0x64u16.to_be_bytes());
    p[12..16].copy_from_slice(&MOCK_TAG.to_be_bytes());
    p[16..48].fill(0x44);
    udp.send(&init_ack).await.unwrap();

    let n = timeout(Duration::from_secs(5), udp.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let mut key_state = KeyState::new();
    let cookie = parse_message(&buf[1..n], MOCK_TAG, &mut key_state).expect("COOKIE");
    assert_eq!(cookie.chunk_type, 0xa);
    let mut cookie_ack = vec![0u8; 17];
    cookie_ack[0] = PACKET_TYPE_CONTROL;
    write_message_header(
        &mut cookie_ack[1..],
        client_tag,
        0,
        CHUNK_TYPE_COOKIE_ACK,
        0,
        0,
    );
    udp.send(&cookie_ack).await.unwrap();

    // BIG (no crypt yet).
    let big = recv_message(&udp, MOCK_TAG, &mut key_state, None, |data| {
        !data.is_empty()
    })
    .await
    .expect("BIG");
    let (handshake_key, client_pub, client_sig, session_key) = parse_big(&big);
    assert_eq!(session_key, "TEST1234567890ABCDEFGHIJ");

    let mock_ecdh = Ecdh::new().unwrap();
    let secret = mock_ecdh
        .derive_secret(&client_pub, &handshake_key, &client_sig, true)
        .expect("client ECDH signature");
    let mock_pub = mock_ecdh.public_key();
    let mock_sig = mock_ecdh.sign_public_key(&handshake_key);
    let mut local_crypt = GKCrypt::new(3, &handshake_key, &secret).unwrap();
    let mut remote_crypt = GKCrypt::new(2, &handshake_key, &secret).unwrap();
    let mut local_key_pos: u64 = 0;
    // Chiaki expects the console's DATA sequence numbers to start at the
    // console tag (seq_num_remote_initial = tag_remote).
    let mut local_seq: u32 = MOCK_TAG;

    let bang_payload = encode_bang(&mock_pub, &mock_sig, "TEST1234567890ABCDEFGHIJ");
    let bang = build_control_data(
        client_tag,
        Some(&mut local_crypt),
        local_key_pos,
        local_seq,
        1,
        1,
        &bang_payload,
    );
    local_key_pos += bang_payload.len() as u64;
    local_seq += 1;
    udp.send(&bang).await.unwrap();

    let au = base64::decode(TINY_AU_B64).unwrap();
    let audio_header = [2u8, 16, 0, 0, 0xbb, 0x80, 0, 0, 0x01, 0xe0, 0, 0, 0, 0];
    let stream_info = encode_stream_info(160, 90, &au, &audio_header);
    let packet = build_control_data(
        client_tag,
        Some(&mut local_crypt),
        local_key_pos,
        local_seq,
        1,
        1,
        &stream_info,
    );
    local_key_pos += stream_info.len() as u64;
    local_seq += 1;
    udp.send(&packet).await.unwrap();

    // Stream info ack.
    recv_message(
        &udp,
        MOCK_TAG,
        &mut key_state,
        Some(&mut remote_crypt),
        |data| protobuf::message_type(data) == Ok(protobuf::TYPE_STREAMINFOACK),
    )
    .await
    .expect("stream info ack");

    // Controller connection follows the stream-info ACK (chiaki-ng ordering).
    let controller = recv_message(
        &udp,
        MOCK_TAG,
        &mut key_state,
        Some(&mut remote_crypt),
        |data| protobuf::message_type(data) == Ok(protobuf::TYPE_CONTROLLERCONNECTION),
    )
    .await
    .expect("controller connection");

    // Console connection quality report.
    let quality = encode_connection_quality(0.0125);
    let packet = build_control_data(
        client_tag,
        Some(&mut local_crypt),
        local_key_pos,
        local_seq,
        1,
        1,
        &quality,
    );
    local_key_pos += quality.len() as u64;
    udp.send(&packet).await.unwrap();

    // Video frame.
    let video = build_video_packet(&mut local_crypt, 0, local_key_pos, &au);
    udp.send(&video).await.unwrap();
    local_key_pos += au.len() as u64 + 2;

    // Audio frame (index 1; index 0 is the receiver's startup sentinel).
    let audio = build_audio_packet(&mut local_crypt, 1, local_key_pos, &[1, 2, 3, 4]);
    udp.send(&audio).await.unwrap();

    // Keep the mock alive until the client disconnects.
    let _ = timeout(Duration::from_secs(8), async {
        let mut sink = [0u8; 2048];
        loop {
            match udp.recv(&mut sink).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    })
    .await;
    let _ = controller;
    let _ = audio;
}

fn parse_big(payload: &[u8]) -> ([u8; 16], Vec<u8>, Vec<u8>, String) {
    let mut outer = Reader::new(payload);
    let mut big = None;
    while let Some((field, value)) = outer.next_field().unwrap() {
        if field == 2 {
            big = Some(as_bytes(value).unwrap().to_vec());
        }
    }
    let big = big.expect("big payload");
    let mut r = Reader::new(&big);
    let mut launch_b64 = String::new();
    let mut client_pub = Vec::new();
    let mut client_sig = Vec::new();
    let mut session_key = String::new();
    while let Some((field, value)) = r.next_field().unwrap() {
        match field {
            2 => session_key = as_string(value).unwrap(),
            3 => launch_b64 = as_string(value).unwrap(),
            5 => client_pub = as_bytes(value).unwrap().to_vec(),
            6 => client_sig = as_bytes(value).unwrap().to_vec(),
            _ => {}
        }
    }
    let mut launch = base64::decode(&launch_b64).unwrap();
    let crypt = RPCrypt::init_auth(Target::Ps4_10, &NONCE, &MORNING).unwrap();
    // The launch spec is sent as RPCrypt(zeros) XOR plaintext, not as a
    // direct CFB encryption of the plaintext.
    let mut keystream = vec![0u8; launch.len()];
    crypt.encrypt(0, &mut keystream).unwrap();
    for (byte, stream) in launch.iter_mut().zip(keystream) {
        *byte ^= stream;
    }
    let json = String::from_utf8_lossy(&launch);
    let marker = "\"handshakeKey\":\"";
    let start = json.find(marker).expect("handshakeKey") + marker.len();
    let end = json[start..].find('"').expect("handshakeKey end") + start;
    let key = base64::decode(&json[start..end]).unwrap();
    let mut handshake_key = [0u8; 16];
    handshake_key.copy_from_slice(&key);
    (handshake_key, client_pub, client_sig, session_key)
}

fn encode_bang(pubkey: &[u8; 65], sig: &[u8; 32], session_key: &str) -> Vec<u8> {
    let mut payload = Writer::new();
    payload.uint32(1, 9);
    payload.uint32(2, 0);
    payload.bool(3, true);
    payload.bool(4, true);
    payload.string(5, session_key);
    payload.bytes(8, pubkey);
    payload.bytes(9, sig);
    let mut msg = Writer::new();
    msg.uint32(1, protobuf::TYPE_BANG);
    msg.bytes(3, &payload.finish());
    msg.finish()
}

fn encode_connection_quality(rtt: f64) -> Vec<u8> {
    let mut payload = vec![0x29]; // field 5, wire type 1 (double)
    payload.extend_from_slice(&rtt.to_le_bytes());
    let mut msg = Writer::new();
    msg.uint32(1, protobuf::TYPE_CONNECTIONQUALITY);
    msg.bytes(17, &payload);
    msg.finish()
}

fn encode_stream_info(width: u32, height: u32, header: &[u8], audio: &[u8]) -> Vec<u8> {
    let mut resolution = Writer::new();
    resolution.uint32(1, width);
    resolution.uint32(2, height);
    resolution.bytes(3, header);
    let mut payload = Writer::new();
    payload.bytes(1, &resolution.finish());
    payload.bytes(2, audio);
    let mut msg = Writer::new();
    msg.uint32(1, protobuf::TYPE_STREAMINFO);
    msg.bytes(15, &payload.finish());
    msg.finish()
}

fn build_video_packet(crypt: &mut GKCrypt, frame_index: u16, key_pos: u64, au: &[u8]) -> Vec<u8> {
    let mut data = vec![0u8, 0u8];
    data.extend_from_slice(au);
    let mut packet = vec![0u8; 1 + 20 + data.len()];
    packet[0] = 2;
    packet[1..3].copy_from_slice(&0u16.to_be_bytes());
    packet[3..5].copy_from_slice(&frame_index.to_be_bytes());
    packet[5..9].fill(0); // unit 0, total 1, fec 0
    packet[9] = 3;
    packet[14..18].copy_from_slice(&(key_pos as u32).to_be_bytes());
    packet[21..21 + data.len()].copy_from_slice(&data);
    encrypt_av(crypt, key_pos, 21, &mut packet);
    packet
}

fn build_audio_packet(crypt: &mut GKCrypt, frame_index: u16, key_pos: u64, data: &[u8]) -> Vec<u8> {
    let unit_size = data.len() as u16;
    let fec_word = (unit_size << 8) | 1; // one source unit, no FEC
    let mut packet = vec![0u8; 1 + 18 + data.len()];
    packet[0] = 3;
    packet[1..3].copy_from_slice(&0u16.to_be_bytes());
    packet[3..5].copy_from_slice(&frame_index.to_be_bytes());
    // dword2: unit index (byte 4), total-1 (byte 5), FEC word (low 16).
    packet[5] = 0;
    packet[6] = 0;
    packet[7..9].copy_from_slice(&fec_word.to_be_bytes());
    packet[9] = 5;
    packet[14..18].copy_from_slice(&(key_pos as u32).to_be_bytes());
    packet[19..].copy_from_slice(data);
    encrypt_av(crypt, key_pos, 19, &mut packet);
    packet
}

fn encrypt_av(crypt: &mut GKCrypt, key_pos: u64, data_offset: usize, packet: &mut [u8]) {
    crypt
        .encrypt(key_pos + 16, &mut packet[data_offset..])
        .unwrap();
    let tag = crypt.gmac(key_pos, packet).unwrap();
    packet[10..14].copy_from_slice(&tag);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mock_console_pipeline() {
    let tcp = TcpListener::bind("127.0.0.1:9295").await.unwrap();
    let udp = UdpSocket::bind("127.0.0.1:9296").await.unwrap();
    let crypt = RPCrypt::init_auth(Target::Ps4_10, &NONCE, &MORNING).unwrap();

    let tcp_task = tokio::spawn(tcp_console(tcp, crypt.clone()));
    let udp_task = tokio::spawn(udp_console(udp, crypt));

    let profile = VideoProfile::preset(Resolution::P720, Fps::Fps60).unwrap();
    let info = ConnectInfo::new(
        Target::Ps4_10,
        "127.0.0.1".to_string(),
        REGIST_KEY,
        MORNING,
        profile,
    );
    let mut session = Session::start(
        info,
        SessionOptions {
            senkusha: false,
            ..Default::default()
        },
    )
    .unwrap();
    let mut video = session.take_video();
    let mut audio = session.take_audio();
    let mut events = session.take_events();
    let pin_commands = session.commands.clone();

    let event_task = tokio::spawn(async move {
        let mut kinds = Vec::new();
        let mut pin_incorrect = Vec::new();
        let mut latencies = Vec::new();
        while let Some(event) = events.recv().await {
            match &event {
                SessionEvent::Connected => kinds.push("connected"),
                SessionEvent::ConnectionQuality { rtt_ms } => latencies.push(*rtt_ms),
                SessionEvent::StreamInfo { .. } => kinds.push("stream_info"),
                SessionEvent::LoginPinRequest { incorrect } => {
                    kinds.push("pin_request");
                    pin_incorrect.push(*incorrect);
                    // The mock rejects the first attempt and accepts the second.
                    let pin = if *incorrect { "5678" } else { "1234" };
                    let _ = pin_commands.try_send(SessionCommand::SubmitPin(pin.into()));
                }
                SessionEvent::Quit { .. } => break,
                _ => {}
            }
        }
        (kinds, pin_incorrect, latencies)
    });

    let frame = timeout(Duration::from_secs(10), video.recv())
        .await
        .expect("video timeout")
        .expect("video channel");
    let expected_au = base64::decode(TINY_AU_B64).unwrap();
    assert!(frame.is_idr);
    assert_eq!(frame.data.as_ref(), expected_au.as_slice());

    let audio_frame = timeout(Duration::from_secs(10), audio.recv())
        .await
        .expect("audio timeout")
        .expect("audio channel");
    assert_eq!(audio_frame.data.as_ref(), &[1, 2, 3, 4]);

    let _ = session.send_command(SessionCommand::Stop);
    let outcome = timeout(Duration::from_secs(5), session.wait())
        .await
        .expect("outcome timeout");
    assert_eq!(outcome.reason(), QuitReason::Stopped);
    let (kinds, pin_incorrect, latencies) = timeout(Duration::from_secs(3), event_task)
        .await
        .unwrap()
        .unwrap();
    assert!(kinds.contains(&"connected"), "events: {kinds:?}");
    assert!(kinds.contains(&"stream_info"), "events: {kinds:?}");
    assert_eq!(
        pin_incorrect,
        [false, true],
        "wrong PIN must re-open the PIN prompt"
    );
    assert!(
        latencies.iter().any(|rtt| (*rtt - 12.5).abs() < 1e-6),
        "console quality must surface as normalized latency: {latencies:?}"
    );

    session.stop().await;
    tcp_task.abort();
    udp_task.abort();
}

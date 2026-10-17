// SPDX-License-Identifier: AGPL-3.0-only
//! End-to-end Senkusha test against a mock console on UDP 9297.

use std::net::IpAddr;
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use chiaki_core::proto::{Reader, as_bytes, as_u32};
use chiaki_core::senkusha;
use chiaki_core::takion::packet::{
    CHUNK_TYPE_COOKIE_ACK, CHUNK_TYPE_DATA, CHUNK_TYPE_DATA_ACK, PACKET_TYPE_AUDIO,
    PACKET_TYPE_CONTROL, PACKET_TYPE_VIDEO, parse_message, write_message_header,
};

const MOCK_TAG: u32 = 0x0bad_f00d;

fn init_ack(client_tag: u32) -> Vec<u8> {
    let mut p = vec![0u8; 1 + 16 + 0x30];
    p[0] = PACKET_TYPE_CONTROL;
    write_message_header(&mut p[1..], client_tag, 0, 2, 0, 0x30);
    let payload = &mut p[17..];
    payload[0..4].copy_from_slice(&MOCK_TAG.to_be_bytes());
    payload[4..8].copy_from_slice(&0x19000u32.to_be_bytes());
    payload[8..10].copy_from_slice(&0x64u16.to_be_bytes());
    payload[10..12].copy_from_slice(&0x64u16.to_be_bytes());
    payload[12..16].copy_from_slice(&MOCK_TAG.to_be_bytes());
    payload[16..48].fill(0x44);
    p
}

fn cookie_ack(client_tag: u32) -> Vec<u8> {
    let mut p = vec![0u8; 17];
    p[0] = PACKET_TYPE_CONTROL;
    write_message_header(&mut p[1..], client_tag, 0, CHUNK_TYPE_COOKIE_ACK, 0, 0);
    p
}

fn data_packet(client_tag: u32, seq: u32, channel: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 1 + 16 + 9 + payload.len()];
    p[0] = PACKET_TYPE_CONTROL;
    write_message_header(
        &mut p[1..],
        client_tag,
        0,
        CHUNK_TYPE_DATA,
        1,
        9 + payload.len(),
    );
    let pl = &mut p[17..];
    pl[0..4].copy_from_slice(&seq.to_be_bytes());
    pl[4..6].copy_from_slice(&channel.to_be_bytes());
    pl[6..8].fill(0);
    pl[8] = 0;
    pl[9..].copy_from_slice(payload);
    p
}

fn ack_packet(client_tag: u32, seq: u32) -> Vec<u8> {
    let mut p = vec![0u8; 29];
    p[0] = PACKET_TYPE_CONTROL;
    write_message_header(&mut p[1..], client_tag, 0, CHUNK_TYPE_DATA_ACK, 0, 0xc);
    let pl = &mut p[17..];
    pl[0..4].copy_from_slice(&seq.to_be_bytes());
    pl[4..8].copy_from_slice(&0x19000u32.to_be_bytes());
    pl[8..12].fill(0);
    p
}

fn av_packet(is_video: bool, frame_index: u16, unit_index: u16, data: &[u8]) -> Vec<u8> {
    let header = 0x12 + if is_video { 3 } else { 0 };
    let mut p = vec![0u8; header + data.len()];
    p[0] = if is_video {
        PACKET_TYPE_VIDEO
    } else {
        PACKET_TYPE_AUDIO
    };
    p[3..5].copy_from_slice(&frame_index.to_be_bytes());
    let dword = 0x7ffu32 << 0xa | ((unit_index as u32) & 0x7ff) << 0x15;
    p[5..9].copy_from_slice(&dword.to_be_bytes());
    p[9] = if is_video { 3 } else { 0xff };
    p[header..].copy_from_slice(data);
    p
}

fn parse_data(
    packet: &[u8],
    key_state: &mut chiaki_core::takion::KeyState,
) -> Option<(u32, u16, Vec<u8>)> {
    if packet.is_empty() || packet[0] & 0x0f != PACKET_TYPE_CONTROL {
        return None;
    }
    let header = parse_message(&packet[1..], MOCK_TAG, key_state).ok()?;
    if header.chunk_type != CHUNK_TYPE_DATA {
        return None;
    }
    let start = 1 + header.payload_offset;
    let end = start + header.payload_size;
    if end > packet.len() || end - start < 9 {
        return None;
    }
    let payload = &packet[start..end];
    let seq = u32::from_be_bytes(payload[0..4].try_into().unwrap());
    let channel = u16::from_be_bytes(payload[4..6].try_into().unwrap());
    Some((seq, channel, payload[9..].to_vec()))
}

fn message_type(payload: &[u8]) -> Option<u32> {
    let mut r = Reader::new(payload);
    while let Ok(Some((field, value))) = r.next_field() {
        if field == 1 {
            return as_u32(value).ok();
        }
    }
    None
}

struct SenkushaFields<'a> {
    command: u32,
    mtu: Option<&'a [u8]>,
    client_mtu: Option<&'a [u8]>,
}

fn senkusha_fields(payload: &[u8]) -> Option<SenkushaFields<'_>> {
    let mut r = Reader::new(payload);
    let mut senkusha = None;
    while let Ok(Some((field, value))) = r.next_field() {
        if field == 14 {
            senkusha = as_bytes(value).ok();
        }
    }
    let mut r = Reader::new(senkusha?);
    let mut command = None;
    let mut mtu = None;
    let mut client = None;
    while let Ok(Some((field, value))) = r.next_field() {
        match field {
            1 => command = as_u32(value).ok(),
            3 => mtu = as_bytes(value).ok(),
            5 => client = as_bytes(value).ok(),
            _ => {}
        }
    }
    Some(SenkushaFields {
        command: command?,
        mtu,
        client_mtu: client,
    })
}

fn field_u32(payload: &[u8], want: u32) -> Option<u32> {
    let mut r = Reader::new(payload);
    while let Ok(Some((field, value))) = r.next_field() {
        if field == want {
            return as_u32(value).ok();
        }
    }
    None
}

async fn mock_console(sock: UdpSocket) {
    let mut buf = [0u8; 4096];
    let (n, peer) = timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    sock.connect(peer).await.unwrap();
    assert_eq!(n, 33);
    let client_tag = u32::from_be_bytes(buf[17..21].try_into().unwrap());
    sock.send(&init_ack(client_tag)).await.unwrap();

    let n = timeout(Duration::from_secs(5), sock.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let mut key_state = chiaki_core::takion::KeyState::new();
    let cookie = parse_message(&buf[1..n], MOCK_TAG, &mut key_state).unwrap();
    assert_eq!(cookie.chunk_type, 0x0a);
    sock.send(&cookie_ack(client_tag)).await.unwrap();

    let mut seq: u32 = 0x1000;
    loop {
        let n = match timeout(Duration::from_secs(10), sock.recv(&mut buf)).await {
            Ok(Ok(n)) => n,
            _ => break,
        };
        let packet = &buf[..n];
        if packet[0] & 0x0f == PACKET_TYPE_VIDEO || packet[0] & 0x0f == PACKET_TYPE_AUDIO {
            if packet[0] & 0x0f == PACKET_TYPE_AUDIO {
                // Echo the ping back as a pong with the same unit index and tag.
                let dword = u32::from_be_bytes(packet[5..9].try_into().unwrap());
                let unit = ((dword >> 0x15) & 0x7ff) as u16;
                let mut data = [0u8; 8];
                data[4..8].copy_from_slice(&packet[0x16..0x1a]);
                let pong = av_packet(false, 0, unit, &data);
                sock.send(&pong).await.unwrap();
            }
            continue;
        }
        let Some((client_seq, channel, payload)) = parse_data(packet, &mut key_state) else {
            continue;
        };
        sock.send(&ack_packet(client_tag, client_seq))
            .await
            .unwrap();
        match message_type(&payload) {
            Some(31) => {
                // Takion protocol request -> ack (type 32, field 32, empty)
                let ack = [0x08, 0x20, 0x82, 0x02, 0x00];
                sock.send(&data_packet(client_tag, seq, 1, &ack))
                    .await
                    .unwrap();
                seq += 1;
            }
            Some(0) if channel == 1 => {
                // BIG -> BANG
                let bang = [0x08, 0x01, 0x1a, 0x00];
                sock.send(&data_packet(client_tag, seq, 1, &bang))
                    .await
                    .unwrap();
                seq += 1;
            }
            Some(12) => {
                let Some(fields) = senkusha_fields(&payload) else {
                    continue;
                };
                match fields.command {
                    1 => {
                        // MTU command -> video response with matching frame index.
                        let id = fields.mtu.and_then(|m| field_u32(m, 1)).unwrap_or(0) as u16;
                        let video = av_packet(true, id, 0, &[0u8; 8]);
                        sock.send(&video).await.unwrap();
                    }
                    4 => {
                        let id = fields.client_mtu.and_then(|c| field_u32(c, 1)).unwrap_or(0);
                        if id == 1 {
                            // Reply with a matching client MTU command (id 1).
                            let reply =
                                [0x08, 0x0c, 0x72, 0x06, 0x08, 0x04, 0x2a, 0x02, 0x08, 0x01];
                            sock.send(&data_packet(client_tag, seq, 8, &reply))
                                .await
                                .unwrap();
                            seq += 1;
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn senkusha_mock_pipeline() {
    let sock = UdpSocket::bind("127.0.0.1:9297").await.unwrap();
    let mock = tokio::spawn(mock_console(sock));

    let cancel = CancellationToken::new();
    let result = timeout(
        Duration::from_secs(30),
        senkusha::run(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), &cancel),
    )
    .await
    .expect("senkusha timeout")
    .expect("senkusha failed");

    assert_eq!(result.mtu_in, 1454);
    assert_eq!(result.mtu_out, 1454);
    assert!(result.rtt_us > 0);
    mock.abort();
}

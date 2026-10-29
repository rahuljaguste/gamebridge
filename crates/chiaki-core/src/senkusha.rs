// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Senkusha: pre-stream MTU/RTT probing on UDP 9297.
//!
//! Ported from Chiaki's `lib/src/senkusha.c`. Current PS5 firmware expects
//! this initialization before the stream connection; chiaki-ng always runs
//! it, while mainline Chiaki 2.2.0 leaves it behind a disabled compile flag.

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use crate::error::{Error, Result};
use crate::proto::{Reader, Writer, as_bytes, as_u32};
use crate::takion::packet::{
    A_RWND, CHUNK_TYPE_DATA, CHUNK_TYPE_DATA_ACK, MESSAGE_HEADER_SIZE, PACKET_TYPE_AUDIO,
    PACKET_TYPE_CONTROL, PACKET_TYPE_VIDEO, parse_message, write_message_header,
};
use crate::takion::{KeyState, handshake, make_socket};
use crate::util::rng;

pub const SENKUSHA_PORT: u16 = 9297;

const EXPECT_TIMEOUT: Duration = Duration::from_secs(5);
const PONG_TIMEOUT: Duration = Duration::from_millis(1000);
const PING_COUNT: u16 = 10;
const RESEND_INTERVAL: Duration = Duration::from_millis(200);
const MTU_MIN: u32 = 576;
const MTU_MAX: u32 = 1454;
const MTU_UDP_PACKET_ADD: u32 = 0x1c;
const MTU_AV_PACKET_ADD: usize = 0x12;
const MTU_RETRIES: usize = 3;

// TakionMessage.PayloadType
const TYPE_BANG: u32 = 1;
const TYPE_DISCONNECT: u32 = 8;
const TYPE_SENKUSHA: u32 = 12;
const TYPE_TAKION_PROTOCOL_REQUEST: u32 = 31;
const TYPE_TAKION_PROTOCOL_REQUEST_ACK: u32 = 32;

// SenkushaPayload.Command
const CMD_ECHO: u32 = 0;
const CMD_MTU: u32 = 1;
const CMD_CLIENT_MTU: u32 = 4;

#[derive(Debug, Clone, Copy)]
pub struct SenkushaResult {
    pub mtu_in: u32,
    pub mtu_out: u32,
    pub rtt_us: u64,
}

#[derive(Debug)]
struct V7Av {
    is_video: bool,
    frame_index: u16,
    unit_index: u16,
    data: Vec<u8>,
}

enum Event {
    Data { payload: Vec<u8> },
    Ack(u32),
    Av(V7Av),
}

struct Conn {
    sock: UdpSocket,
    tag_local: u32,
    tag_remote: u32,
    seq: u32,
    key_state: KeyState,
    /// Reused datagram buffer; senkusha receives thousands of probe replies.
    recv_buf: Vec<u8>,
}

impl Conn {
    fn data_packet(&self, seq: u32, channel: u16, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![0u8; 1 + MESSAGE_HEADER_SIZE + 9 + payload.len()];
        packet[0] = PACKET_TYPE_CONTROL;
        write_message_header(
            &mut packet[1..],
            self.tag_remote,
            0,
            CHUNK_TYPE_DATA,
            1,
            9 + payload.len(),
        );
        let p = &mut packet[1 + MESSAGE_HEADER_SIZE..];
        p[0..4].copy_from_slice(&seq.to_be_bytes());
        p[4..6].copy_from_slice(&channel.to_be_bytes());
        p[6..8].fill(0);
        p[8] = 0;
        p[9..].copy_from_slice(payload);
        packet
    }

    async fn send_data(&mut self, channel: u16, payload: &[u8]) -> Result<u32> {
        let seq = self.seq;
        self.seq = self.seq.wrapping_add(1);
        let packet = self.data_packet(seq, channel, payload);
        self.sock.send(&packet).await.map_err(|_| Error::Network)?;
        tracing::info!(
            "senkusha: sent DATA seq {seq:#x} channel {channel} ({} bytes)",
            payload.len()
        );
        Ok(seq)
    }

    async fn send_ack(&self, seq: u32) -> Result<()> {
        let mut packet = vec![0u8; 1 + MESSAGE_HEADER_SIZE + 0xc];
        packet[0] = PACKET_TYPE_CONTROL;
        write_message_header(
            &mut packet[1..],
            self.tag_remote,
            0,
            CHUNK_TYPE_DATA_ACK,
            0,
            0xc,
        );
        let p = &mut packet[1 + MESSAGE_HEADER_SIZE..];
        p[0..4].copy_from_slice(&seq.to_be_bytes());
        p[4..8].copy_from_slice(&A_RWND.to_be_bytes());
        p[8..12].fill(0);
        self.sock.send(&packet).await.map_err(|_| Error::Network)?;
        Ok(())
    }

    async fn send_data_wait_ack(
        &mut self,
        channel: u16,
        payload: &[u8],
        cancel: &CancellationToken,
        window: Duration,
    ) -> Result<bool> {
        let seq = self.send_data(channel, payload).await?;
        let deadline = Instant::now() + window;
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Canceled);
            }
            let now = Instant::now();
            if now >= deadline {
                tracing::warn!("senkusha: no DATA_ACK for seq {seq:#x} on channel {channel}");
                return Ok(false);
            }
            let wait = (deadline - now).min(RESEND_INTERVAL);
            match tokio::time::timeout(wait, self.recv_event()).await {
                Ok(Ok(Event::Ack(ack))) => {
                    tracing::info!("senkusha: received DATA_ACK seq {ack:#x}");
                    if ack == seq {
                        return Ok(true);
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => {
                    let packet = self.data_packet(seq, channel, payload);
                    self.sock.send(&packet).await.map_err(|_| Error::Network)?;
                }
            }
        }
    }

    async fn recv_event(&mut self) -> Result<Event> {
        loop {
            let n = self
                .sock
                .recv(&mut self.recv_buf)
                .await
                .map_err(|_| Error::Network)?;
            if n == 0 {
                continue;
            }
            let packet = &self.recv_buf[..n];
            match packet[0] & 0x0f {
                PACKET_TYPE_CONTROL => {
                    let header =
                        match parse_message(&packet[1..], self.tag_local, &mut self.key_state) {
                            Ok(header) => header,
                            Err(_) => continue,
                        };
                    let start = 1 + header.payload_offset;
                    let end = start + header.payload_size;
                    if end > packet.len() {
                        continue;
                    }
                    let payload = &packet[start..end];
                    match header.chunk_type {
                        CHUNK_TYPE_DATA => {
                            if payload.len() < 9 {
                                continue;
                            }
                            let seq = u32::from_be_bytes(payload[0..4].try_into().unwrap());
                            let channel = u16::from_be_bytes(payload[4..6].try_into().unwrap());
                            tracing::info!(
                                "senkusha: received DATA seq {seq:#x} channel {channel} ({} bytes)",
                                payload.len() - 9
                            );
                            let data = payload[9..].to_vec();
                            self.send_ack(seq).await?;
                            if payload[8] == 0 {
                                return Ok(Event::Data { payload: data });
                            }
                        }
                        CHUNK_TYPE_DATA_ACK => {
                            if payload.len() < 4 {
                                continue;
                            }
                            let seq = u32::from_be_bytes(payload[0..4].try_into().unwrap());
                            return Ok(Event::Ack(seq));
                        }
                        _ => {}
                    }
                }
                PACKET_TYPE_VIDEO | PACKET_TYPE_AUDIO => {
                    if let Some(av) = parse_v7_av(packet) {
                        return Ok(Event::Av(av));
                    }
                }
                _ => {}
            }
        }
    }

    async fn wait_data<F>(
        &mut self,
        deadline: Instant,
        cancel: &CancellationToken,
        predicate: F,
    ) -> Result<bool>
    where
        F: Fn(&[u8]) -> bool,
    {
        self.wait_event(
            deadline,
            cancel,
            |event| matches!(event, Event::Data { payload } if predicate(payload)),
        )
        .await
    }

    /// Poll inbound events until `predicate` accepts one, the deadline passes,
    /// or the session is cancelled.
    async fn wait_event(
        &mut self,
        deadline: Instant,
        cancel: &CancellationToken,
        mut predicate: impl FnMut(&Event) -> bool,
    ) -> Result<bool> {
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Canceled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(false);
            }
            let wait = (deadline - now).min(RESEND_INTERVAL);
            match tokio::time::timeout(wait, self.recv_event()).await {
                Ok(Ok(event)) => {
                    if predicate(&event) {
                        return Ok(true);
                    }
                }
                Ok(Err(e)) => return Err(e),
                Err(_) => {}
            }
        }
    }
}

/// Run Senkusha against the console. On success returns the measured MTU and
/// RTT; on any failure the caller falls back to the reference defaults.
pub async fn run(host: IpAddr, cancel: &CancellationToken) -> Result<SenkushaResult> {
    let addr = SocketAddr::new(host, SENKUSHA_PORT);
    tracing::info!("senkusha: connecting to {addr}");
    let sock = make_socket(addr)?;
    let (tag_local, tag_remote, seq_num_local) = handshake(&sock, 7).await?;
    tracing::info!("senkusha: connected (local tag {tag_local:#x}, remote tag {tag_remote:#x})");
    let mut conn = Conn {
        sock,
        tag_local,
        tag_remote,
        seq: seq_num_local,
        key_state: KeyState::new(),
        recv_buf: vec![0u8; 65535],
    };

    // Negotiate the Takion protocol version first. chiaki-ng sends a
    // TAKIONPROTOCOLREQUEST advertising version 9 and waits for the ACK.
    let request = {
        let mut versions = Writer::new();
        versions.uint32(1, 9);
        encode_message(TYPE_TAKION_PROTOCOL_REQUEST, 31, &versions.finish())
    };
    conn.send_data(1, &request).await?;
    let deadline = Instant::now() + EXPECT_TIMEOUT;
    let negotiated = conn
        .wait_data(deadline, cancel, |payload| {
            message_type(payload) == Some(TYPE_TAKION_PROTOCOL_REQUEST_ACK)
        })
        .await?;
    if negotiated {
        tracing::info!("senkusha: takion protocol version negotiated");
    } else {
        tracing::warn!("senkusha: no takion protocol ack; using BIG version 7");
    }
    let big_version = if negotiated { 9 } else { 7 };

    // BIG with empty session/launch/key fields.
    let big = encode_message(0, 2, &{
        let mut p = Writer::new();
        p.uint32(1, big_version);
        p.string(2, "");
        p.string(3, "");
        p.bytes(4, &[]);
        p.finish()
    });
    conn.send_data(1, &big).await?;
    tracing::info!("senkusha: sent BIG, waiting for BANG");
    let deadline = Instant::now() + EXPECT_TIMEOUT;
    if !conn
        .wait_data(deadline, cancel, |payload| {
            message_type(payload) == Some(TYPE_BANG)
        })
        .await?
    {
        return Err(Error::Timeout);
    }
    tracing::info!("senkusha: received BANG");

    let rtt_us = rtt_test(&mut conn, cancel).await?;
    tracing::info!("senkusha: average RTT {rtt_us} us");

    let mtu_in = mtu_in_test(&mut conn, rtt_us, cancel).await?;
    tracing::info!("senkusha: inbound MTU {mtu_in}");

    let mtu_out = mtu_out_test(&mut conn, mtu_in, rtt_us, cancel).await?;
    tracing::info!("senkusha: outbound MTU {mtu_out}");

    let disconnect = encode_message(TYPE_DISCONNECT, 10, &{
        let mut p = Writer::new();
        p.string(1, "Client Disconnecting");
        p.finish()
    });
    conn.send_data(1, &disconnect).await?;
    tracing::info!("senkusha: completed");
    Ok(SenkushaResult {
        mtu_in,
        mtu_out,
        rtt_us,
    })
}

async fn rtt_test(conn: &mut Conn, cancel: &CancellationToken) -> Result<u64> {
    let echo_on = encode_echo(true);
    if !conn
        .send_data_wait_ack(8, &echo_on, cancel, Duration::from_secs(1))
        .await?
    {
        return Err(Error::Timeout);
    }

    let mut acc = 0u64;
    let mut successful = 0u64;
    for ping in 0..PING_COUNT {
        let tag = rng::u32();
        let packet = rtt_ping(0, ping, tag);
        let start = Instant::now();
        conn.sock.send(&packet).await.map_err(|_| Error::Network)?;
        if wait_pong(conn, 0, ping, tag, PONG_TIMEOUT, cancel).await? {
            acc += start.elapsed().as_micros() as u64;
            successful += 1;
        }
    }

    let echo_off = encode_echo(false);
    if !conn
        .send_data_wait_ack(8, &echo_off, cancel, Duration::from_secs(1))
        .await?
    {
        return Err(Error::Timeout);
    }

    if successful == 0 {
        return Err(Error::Timeout);
    }
    Ok(acc / successful)
}

async fn wait_pong(
    conn: &mut Conn,
    frame_index: u16,
    unit_index: u16,
    tag: u32,
    window: Duration,
    cancel: &CancellationToken,
) -> Result<bool> {
    let deadline = Instant::now() + window;
    conn.wait_event(deadline, cancel, |event| {
        matches!(event, Event::Av(av)
            if !av.is_video
                && av.frame_index == frame_index
                && av.unit_index == unit_index
                && av.data.len() >= 8
                && u32::from_be_bytes(av.data[4..8].try_into().unwrap()) == tag)
    })
    .await
}

async fn mtu_in_test(conn: &mut Conn, rtt_us: u64, cancel: &CancellationToken) -> Result<u32> {
    let timeout = mtu_timeout(rtt_us);
    let mut min = MTU_MIN;
    let mut max = MTU_MAX;
    let mut cur = max;
    let mut request_id: u32 = 0;
    while max - min > 1 {
        let mut success = false;
        for _ in 0..MTU_RETRIES {
            request_id += 1;
            let id = request_id as u16;
            let payload = encode_mtu_command(request_id, cur, 1);
            conn.send_data(8, &payload).await?;
            let deadline = Instant::now() + timeout;
            if wait_mtu_response(conn, id, deadline, cancel).await? {
                success = true;
                break;
            }
        }
        if success {
            min = cur;
        } else {
            max = cur;
        }
        cur = min + (max - min) / 2;
    }
    Ok(min)
}

async fn wait_mtu_response(
    conn: &mut Conn,
    id: u16,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<bool> {
    conn.wait_event(
        deadline,
        cancel,
        |event| matches!(event, Event::Av(av) if av.is_video && av.frame_index == id),
    )
    .await
}

async fn mtu_out_test(
    conn: &mut Conn,
    mtu_in: u32,
    rtt_us: u64,
    cancel: &CancellationToken,
) -> Result<u32> {
    if !(MTU_MIN..=MTU_MAX).contains(&mtu_in) {
        return Err(Error::InvalidData);
    }
    let timeout = mtu_timeout(rtt_us);

    let handshake = encode_client_mtu_command(1, mtu_in, true, Some(mtu_in));
    conn.send_data(8, &handshake).await?;
    let deadline = Instant::now() + EXPECT_TIMEOUT;
    if !conn
        .wait_data(deadline, cancel, |payload| {
            decode_client_mtu_id(payload) == Some(1)
        })
        .await?
    {
        return Err(Error::Timeout);
    }
    tracing::info!("senkusha: received client MTU response");

    let mut min = MTU_MIN;
    let mut max = MTU_MAX;
    let mut cur = mtu_in;
    while max - min > 1 {
        let mut success = false;
        for attempt in 0..MTU_RETRIES {
            let tag = rng::u32();
            let packet = mtu_ping(attempt as u16, tag);
            let send_len = cur as usize - MTU_UDP_PACKET_ADD as usize;
            conn.sock
                .send(&packet[..send_len])
                .await
                .map_err(|_| Error::Network)?;
            if wait_pong(conn, 0, attempt as u16, tag, timeout, cancel).await? {
                success = true;
                break;
            }
        }
        if success {
            min = cur;
        } else {
            max = cur;
        }
        cur = min + (max - min) / 2;
    }

    let final_cmd = encode_client_mtu_command(2, max, false, Some(mtu_in));
    // chiaki-ng sends the final client-MTU command with id 2 and waits only
    // for its transport ACK; the command is advisory and has no id-2 protocol
    // response. Still fail fast when that ACK never arrives.
    if !conn
        .send_data_wait_ack(8, &final_cmd, cancel, Duration::from_secs(2))
        .await?
    {
        return Err(Error::Timeout);
    }
    Ok(min)
}

fn mtu_timeout(rtt_us: u64) -> Duration {
    let ms = (rtt_us * 5 / 1000).clamp(5, 500);
    Duration::from_millis(ms)
}

fn message_type(payload: &[u8]) -> Option<u32> {
    let mut reader = Reader::new(payload);
    while let Ok(Some((field, value))) = reader.next_field() {
        if field == 1 {
            return as_u32(value).ok();
        }
    }
    None
}

fn decode_client_mtu_id(payload: &[u8]) -> Option<u32> {
    let mut reader = Reader::new(payload);
    let mut kind = None;
    let mut senkusha = None;
    while let Ok(Some((field, value))) = reader.next_field() {
        match field {
            1 => kind = as_u32(value).ok(),
            14 => senkusha = as_bytes(value).ok(),
            _ => {}
        }
    }
    if kind? != TYPE_SENKUSHA {
        return None;
    }
    let mut reader = Reader::new(senkusha?);
    let mut command = None;
    let mut client_mtu = None;
    while let Ok(Some((field, value))) = reader.next_field() {
        match field {
            1 => command = as_u32(value).ok(),
            5 => client_mtu = as_bytes(value).ok(),
            _ => {}
        }
    }
    if command? != CMD_CLIENT_MTU {
        return None;
    }
    let mut reader = Reader::new(client_mtu?);
    while let Ok(Some((field, value))) = reader.next_field() {
        if field == 1 {
            return as_u32(value).ok();
        }
    }
    None
}

fn encode_message(kind: u32, payload_field: u32, payload: &[u8]) -> Vec<u8> {
    let mut msg = Writer::new();
    msg.uint32(1, kind);
    msg.bytes(payload_field, payload);
    msg.finish()
}

fn encode_echo(enable: bool) -> Vec<u8> {
    let mut echo = Writer::new();
    echo.bool(1, enable);
    let mut payload = Writer::new();
    payload.uint32(1, CMD_ECHO);
    payload.bytes(2, &echo.finish());
    encode_message(TYPE_SENKUSHA, 14, &payload.finish())
}

fn encode_mtu_command(id: u32, mtu_req: u32, num: u32) -> Vec<u8> {
    let mut cmd = Writer::new();
    cmd.uint32(1, id);
    cmd.uint32(2, mtu_req);
    cmd.uint32(4, num);
    let mut payload = Writer::new();
    payload.uint32(1, CMD_MTU);
    payload.bytes(3, &cmd.finish());
    encode_message(TYPE_SENKUSHA, 14, &payload.finish())
}

fn encode_client_mtu_command(id: u32, mtu_req: u32, state: bool, mtu_down: Option<u32>) -> Vec<u8> {
    let mut cmd = Writer::new();
    cmd.uint32(1, id);
    cmd.uint32(2, mtu_req);
    cmd.bool(3, state);
    if let Some(down) = mtu_down {
        cmd.uint32(4, down);
    }
    let mut payload = Writer::new();
    payload.uint32(1, CMD_CLIENT_MTU);
    payload.bytes(5, &cmd.finish());
    encode_message(TYPE_SENKUSHA, 14, &payload.finish())
}

fn v7_av_header(
    is_video: bool,
    packet_index: u16,
    frame_index: u16,
    unit_index: u16,
    units_total: u16,
    fec: u16,
    codec: u8,
) -> Vec<u8> {
    let header = MTU_AV_PACKET_ADD + if is_video { 3 } else { 0 };
    let mut buf = vec![0u8; header];
    buf[0] = if is_video {
        PACKET_TYPE_VIDEO
    } else {
        PACKET_TYPE_AUDIO
    };
    buf[1..3].copy_from_slice(&packet_index.to_be_bytes());
    buf[3..5].copy_from_slice(&frame_index.to_be_bytes());
    let dword = (fec as u32 & 0x3ff)
        | (((units_total.wrapping_sub(1)) as u32 & 0x7ff) << 0xa)
        | (((unit_index as u32) & 0xffff) << 0x15);
    buf[5..9].copy_from_slice(&dword.to_be_bytes());
    buf[9] = codec;
    buf[10..14].fill(0);
    buf[14..18].fill(0);
    if is_video {
        buf[0x12..0x15].fill(0);
    }
    buf
}

fn rtt_ping(frame_index: u16, unit_index: u16, tag: u32) -> Vec<u8> {
    let mut buf = v7_av_header(false, 0, frame_index, unit_index, 0x800, 0, 0xff);
    buf.resize(0x224, 0);
    buf[MTU_AV_PACKET_ADD + 4..MTU_AV_PACKET_ADD + 8].copy_from_slice(&tag.to_be_bytes());
    buf
}

fn mtu_ping(unit_index: u16, tag: u32) -> Vec<u8> {
    let mut buf = v7_av_header(false, 0, 0, unit_index, 0x800, 0, 0xff);
    buf.resize(MTU_MAX as usize - MTU_UDP_PACKET_ADD as usize, 0);
    buf[MTU_AV_PACKET_ADD + 4..MTU_AV_PACKET_ADD + 8].copy_from_slice(&tag.to_be_bytes());
    let padding = *b"CHIAKI";
    for (i, byte) in buf[MTU_AV_PACKET_ADD + 8..].iter_mut().enumerate() {
        *byte = padding[i % padding.len()];
    }
    buf
}

fn parse_v7_av(packet: &[u8]) -> Option<V7Av> {
    if packet.is_empty() {
        return None;
    }
    let base = packet[0] & 0x0f;
    if base != PACKET_TYPE_VIDEO && base != PACKET_TYPE_AUDIO {
        return None;
    }
    let is_video = base == PACKET_TYPE_VIDEO;
    let mut header = MTU_AV_PACKET_ADD + if is_video { 3 } else { 0 };
    if (packet[0] >> 4) & 1 != 0 {
        header += 3;
    }
    if packet.len() < header {
        return None;
    }
    let frame_index = u16::from_be_bytes(packet[3..5].try_into().ok()?);
    let dword = u32::from_be_bytes(packet[5..9].try_into().ok()?);
    let unit_index = ((dword >> 0x15) & 0x7ff) as u16;
    Some(V7Av {
        is_video,
        frame_index,
        unit_index,
        data: packet[header..].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_matches_reference_layout() {
        let big = encode_message(0, 2, &{
            let mut p = Writer::new();
            p.uint32(1, 7);
            p.string(2, "");
            p.string(3, "");
            p.bytes(4, &[]);
            p.finish()
        });
        // type=BIG (field 1, 0), big_payload field 2 with 6 content bytes.
        assert_eq!(big[0], 0x08);
        assert_eq!(big[1], 0x00);
        assert_eq!(big[2], 0x12);
        assert_eq!(big[3], 0x08);
    }

    #[test]
    fn echo_encoding_decodes() {
        let msg = encode_echo(true);
        assert_eq!(message_type(&msg), Some(TYPE_SENKUSHA));
        let mut reader = Reader::new(&msg);
        let mut payload = None;
        while let Ok(Some((field, value))) = reader.next_field() {
            if field == 14 {
                payload = as_bytes(value).ok();
            }
        }
        let payload = payload.expect("senkusha payload");
        let mut reader = Reader::new(payload);
        let mut command = None;
        let mut echo = None;
        while let Ok(Some((field, value))) = reader.next_field() {
            match field {
                1 => command = as_u32(value).ok(),
                2 => echo = as_bytes(value).ok(),
                _ => {}
            }
        }
        assert_eq!(command, Some(CMD_ECHO));
        let mut reader = Reader::new(echo.unwrap());
        let mut state = None;
        while let Ok(Some((field, value))) = reader.next_field() {
            if field == 1 {
                state = match value {
                    crate::proto::Value::Varint(v) => Some(v != 0),
                    _ => None,
                };
            }
        }
        assert_eq!(state, Some(true));
    }

    #[test]
    fn mtu_ping_size_and_tag() {
        let packet = mtu_ping(2, 0x1234_5678);
        assert_eq!(packet.len(), MTU_MAX as usize - MTU_UDP_PACKET_ADD as usize);
        assert_eq!(packet[0] & 0x0f, PACKET_TYPE_AUDIO);
        assert_eq!(
            u32::from_be_bytes(packet[0x16..0x1a].try_into().unwrap()),
            0x1234_5678
        );
        assert_eq!(&packet[0x1a..0x20], b"CHIAKI");
        let parsed = parse_v7_av(&packet).unwrap();
        assert!(!parsed.is_video);
        assert_eq!(parsed.unit_index, 2);
    }

    #[test]
    fn rtt_ping_layout() {
        let packet = rtt_ping(0, 7, 0xdead_beef);
        assert_eq!(packet.len(), 0x224);
        let parsed = parse_v7_av(&packet).unwrap();
        assert!(!parsed.is_video);
        assert_eq!(parsed.frame_index, 0);
        assert_eq!(parsed.unit_index, 7);
        assert_eq!(
            u32::from_be_bytes(parsed.data[4..8].try_into().unwrap()),
            0xdead_beef
        );
    }

    #[test]
    fn client_mtu_round_trip() {
        let msg = encode_client_mtu_command(2, 1454, false, Some(1200));
        assert_eq!(decode_client_mtu_id(&msg), Some(2));
    }
}

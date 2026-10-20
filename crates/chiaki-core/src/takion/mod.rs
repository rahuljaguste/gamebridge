// SPDX-License-Identifier: AGPL-3.0-only
//! Takion, the SCTP-like UDP transport used by Remote Play.

pub mod key_state;
pub mod packet;
pub mod reorder;
pub mod send_buffer;

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::crypto::gkcrypt::{GKCRYPT_BLOCK_SIZE, GKCRYPT_GMAC_SIZE, GKCrypt};
use crate::error::{Error, Result};
use crate::feedback::{FeedbackState, format_v9, format_v12};
use packet::*;
use reorder::ReorderQueue;
use send_buffer::SendBuffer;

pub use key_state::{KeyState, seq16_gt, seq16_lt, seq32_gt, seq32_lt};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const POSTPONE_MAX: usize = 32;
const DATA_QUEUE_EXP: u8 = 4; // 16 entries
const SEND_BUFFER_SIZE: usize = 16; // matches TAKION_SEND_BUFFER_SIZE
const RECV_BUFFER_SIZE: usize = 65535;
/// Drain up to this many queued datagrams before returning to the select
/// loop. This keeps the kernel receive buffer from overflowing during a
/// burst without starving commands or the retransmit tick.
const RECV_BATCH_MAX: usize = 64;
/// Fail only when nothing at all has arrived from the console for this long.
/// A brief run of lost control ACKs no longer kills a session whose media is
/// still flowing.
const PEER_SILENCE_TIMEOUT: Duration = Duration::from_secs(15);
/// Stream-control messages (corrupt-frame reports and IDR requests) use chunk
/// flags 1 and channel 2. Swapping these makes the console silently ignore
/// frame-loss reports.
const STREAM_CONTROL_FLAGS: u8 = 1;
const STREAM_CONTROL_CHANNEL: u16 = 2;

#[derive(Debug, Clone)]
pub struct TakionConnectInfo {
    pub version: u8,
    pub enable_crypt: bool,
    pub enable_dualsense: bool,
}

#[derive(Debug)]
pub enum TakionCommand {
    SendMessageData {
        flags: u8,
        channel: u16,
        payload: Vec<u8>,
    },
    /// Continuation chunk of a fragmented DATA message, as produced by
    /// chiaki-ng's `chiaki_takion_send_message_data_cont`.
    SendMessageDataCont {
        flags: u8,
        channel: u16,
        payload: Vec<u8>,
    },
    SendFeedbackState {
        seq: u16,
        state: FeedbackState,
    },
    SendFeedbackHistory {
        seq: u16,
        payload: Vec<u8>,
    },
    SendCongestion(CongestionPacket),
    SendCorruptFrame {
        start: u16,
        end: u16,
    },
    /// Ask the console for an immediate IDR (Takion payload type 25).
    RequestIdr,
    SetCrypt {
        local: Box<GKCrypt>,
        remote: Box<GKCrypt>,
    },
    Close,
}

#[derive(Debug)]
pub enum TakionEvent {
    Disconnected,
    MessageData { data_type: u8, payload: Bytes },
    Av { packet: AvPacket, data: Bytes },
}

pub struct TakionHandle {
    pub commands: mpsc::Sender<TakionCommand>,
    pub events: mpsc::UnboundedReceiver<TakionEvent>,
    cancel: CancellationToken,
    join: JoinHandle<()>,
}

impl TakionHandle {
    pub async fn stop(mut self) {
        let _ = self.commands.send(TakionCommand::Close).await;
        self.cancel.cancel();
        let _ = timeout(Duration::from_secs(2), &mut self.join).await;
        self.join.abort();
    }
}

impl Drop for TakionHandle {
    fn drop(&mut self) {
        // A dropped handle must not leave the UDP driver task running after
        // the owning session has ended.
        self.cancel.cancel();
        self.join.abort();
    }
}

struct DataEntry {
    data_type: u8,
    payload: Bytes,
}

struct State {
    version: u8,
    enable_crypt: bool,
    tag_local: u32,
    tag_remote: u32,
    key_state: KeyState,
    gkcrypt_local: Option<GKCrypt>,
    gkcrypt_remote: Option<GKCrypt>,
    key_pos_local: u64,
    seq_num_local: u32,
    a_rwnd: u32,
    data_queue: ReorderQueue<DataEntry>,
    data_seen: bool,
    ack_logs: u32,
    send_buffer: SendBuffer,
    postponed: Vec<Vec<u8>>,
    pending_ack: Option<u32>,
}

impl State {
    fn crypt_advance(&mut self, data_size: usize) -> u64 {
        if self.gkcrypt_local.is_some() {
            // chiaki-ng advances by size + size % block size; match it for
            // newer console firmware.
            let advance = data_size + data_size % GKCRYPT_BLOCK_SIZE;
            let cur = self.key_pos_local;
            self.key_pos_local = cur.wrapping_add(advance as u64);
            cur
        } else {
            0
        }
    }

    /// Compute the GMAC over `buf`, temporarily zeroing the MAC and (for
    /// control/congestion packets) the key-position field.
    fn packet_mac(&mut self, key_pos: u64, buf: &mut [u8]) -> Result<[u8; 4]> {
        let base = base_type(buf)?;
        let mac_off = mac_offset(base).ok_or(Error::InvalidData)?;
        let key_off = key_pos_offset(base).ok_or(Error::InvalidData)?;
        if buf.len() < mac_off + GKCRYPT_GMAC_SIZE || buf.len() < key_off + 4 {
            return Err(Error::BufferTooSmall);
        }
        let saved_mac: [u8; 4] = buf[mac_off..mac_off + 4].try_into().unwrap();
        buf[mac_off..mac_off + 4].fill(0);
        let zero_key_pos = matches!(base, PACKET_TYPE_CONTROL | PACKET_TYPE_CONGESTION);
        let saved_key_pos: Option<[u8; 4]> = if zero_key_pos {
            let k: [u8; 4] = buf[key_off..key_off + 4].try_into().unwrap();
            buf[key_off..key_off + 4].fill(0);
            Some(k)
        } else {
            None
        };
        let crypt = self.gkcrypt_local.as_mut().ok_or(Error::Uninitialized)?;
        let tag = crypt.gmac(key_pos, buf)?;
        if let Some(k) = saved_key_pos {
            buf[key_off..key_off + 4].copy_from_slice(&k);
        }
        buf[mac_off..mac_off + 4].copy_from_slice(&saved_mac);
        Ok(tag)
    }

    fn verify_packet_mac(&mut self, buf: &mut [u8]) -> Result<()> {
        let Some(_) = self.gkcrypt_remote else {
            return Ok(());
        };
        let base = base_type(buf)?;
        let key_off = key_pos_offset(base).ok_or(Error::InvalidData)?;
        let mac_off = mac_offset(base).ok_or(Error::InvalidData)?;
        if buf.len() < key_off + 4 || buf.len() < mac_off + 4 {
            return Err(Error::BufferTooSmall);
        }
        let low = u32::from_be_bytes(buf[key_off..key_off + 4].try_into().unwrap());
        let key_pos = self.key_state.request_pos(low, false);
        let expected = self.packet_mac_remote(key_pos, buf)?;
        let actual: [u8; 4] = buf[mac_off..mac_off + 4].try_into().unwrap();
        if !crate::util::be::ct_eq(&expected, &actual) {
            return Err(Error::InvalidMac);
        }
        self.key_state.commit(key_pos);
        Ok(())
    }

    fn packet_mac_remote(&mut self, key_pos: u64, buf: &mut [u8]) -> Result<[u8; 4]> {
        let base = base_type(buf)?;
        let mac_off = mac_offset(base).ok_or(Error::InvalidData)?;
        let key_off = key_pos_offset(base).ok_or(Error::InvalidData)?;
        let saved_mac: [u8; 4] = buf[mac_off..mac_off + 4].try_into().unwrap();
        buf[mac_off..mac_off + 4].fill(0);
        let zero_key_pos = matches!(base, PACKET_TYPE_CONTROL | PACKET_TYPE_CONGESTION);
        let saved_key_pos = if zero_key_pos {
            let k: [u8; 4] = buf[key_off..key_off + 4].try_into().unwrap();
            buf[key_off..key_off + 4].fill(0);
            Some(k)
        } else {
            None
        };
        let crypt = self.gkcrypt_remote.as_mut().ok_or(Error::Uninitialized)?;
        let tag = crypt.gmac(key_pos, buf)?;
        if let Some(k) = saved_key_pos {
            buf[key_off..key_off + 4].copy_from_slice(&k);
        }
        buf[mac_off..mac_off + 4].copy_from_slice(&saved_mac);
        Ok(tag)
    }

    fn build_message_data(&mut self, flags: u8, channel: u16, payload: &[u8]) -> Result<Vec<u8>> {
        let key_pos = self.crypt_advance(payload.len());
        let packet_size = 1 + MESSAGE_HEADER_SIZE + 9 + payload.len();
        let mut packet = vec![0u8; packet_size];
        packet[0] = PACKET_TYPE_CONTROL;
        write_message_header(
            &mut packet[1..],
            self.tag_remote,
            key_pos,
            CHUNK_TYPE_DATA,
            flags,
            9 + payload.len(),
        );
        let seq = self.seq_num_local;
        self.seq_num_local = self.seq_num_local.wrapping_add(1);
        let p = &mut packet[1 + MESSAGE_HEADER_SIZE..];
        p[0..4].copy_from_slice(&seq.to_be_bytes());
        p[4..6].copy_from_slice(&channel.to_be_bytes());
        p[6..8].fill(0);
        p[8] = 0; // protobuf data type
        p[9..].copy_from_slice(payload);
        if self.gkcrypt_local.is_some() {
            let tag = self.packet_mac(key_pos, &mut packet)?;
            packet[5..9].copy_from_slice(&tag);
        }
        Ok(packet)
    }

    /// Build a stream-control DATA message (CORRUPTFRAME / IDRREQUEST).
    /// Chiaki sends these on chunk flags 1 and channel 2; centralizing it here
    /// prevents the two call sites from drifting apart again.
    fn build_stream_control(&mut self, payload: &[u8]) -> Result<Vec<u8>> {
        self.build_message_data(STREAM_CONTROL_FLAGS, STREAM_CONTROL_CHANNEL, payload)
    }

    /// Build a continuation chunk of a fragmented DATA message. Unlike a
    /// normal DATA chunk the 8-byte data header carries no protobuf data-type
    /// byte, so the payload starts at offset 8 (chiaki-ng
    /// `chiaki_takion_send_message_data_cont`).
    fn build_message_data_cont(
        &mut self,
        flags: u8,
        channel: u16,
        payload: &[u8],
    ) -> Result<Vec<u8>> {
        let key_pos = self.crypt_advance(payload.len());
        let packet_size = 1 + MESSAGE_HEADER_SIZE + 8 + payload.len();
        let mut packet = vec![0u8; packet_size];
        packet[0] = PACKET_TYPE_CONTROL;
        write_message_header(
            &mut packet[1..],
            self.tag_remote,
            key_pos,
            CHUNK_TYPE_DATA,
            flags,
            8 + payload.len(),
        );
        let seq = self.seq_num_local;
        self.seq_num_local = self.seq_num_local.wrapping_add(1);
        let p = &mut packet[1 + MESSAGE_HEADER_SIZE..];
        p[0..4].copy_from_slice(&seq.to_be_bytes());
        p[4..6].copy_from_slice(&channel.to_be_bytes());
        p[6..8].fill(0);
        p[8..].copy_from_slice(payload);
        if self.gkcrypt_local.is_some() {
            let tag = self.packet_mac(key_pos, &mut packet)?;
            packet[5..9].copy_from_slice(&tag);
        }
        Ok(packet)
    }

    fn build_data_ack(&mut self, seq_num: u32) -> Result<Vec<u8>> {
        let key_pos = self.crypt_advance(29);
        let mut packet = vec![0u8; 1 + MESSAGE_HEADER_SIZE + 0xc];
        packet[0] = PACKET_TYPE_CONTROL;
        write_message_header(
            &mut packet[1..],
            self.tag_remote,
            key_pos,
            CHUNK_TYPE_DATA_ACK,
            0,
            0xc,
        );
        let p = &mut packet[1 + MESSAGE_HEADER_SIZE..];
        p[0..4].copy_from_slice(&seq_num.to_be_bytes());
        p[4..8].copy_from_slice(&self.a_rwnd.to_be_bytes());
        p[8..12].fill(0);
        if self.gkcrypt_local.is_some() {
            let tag = self.packet_mac(key_pos, &mut packet)?;
            packet[5..9].copy_from_slice(&tag);
        }
        Ok(packet)
    }

    fn build_congestion(&mut self, congestion: CongestionPacket) -> Result<Vec<u8>> {
        let key_pos = self.crypt_advance(CONGESTION_PACKET_SIZE);
        let mut packet = [0u8; CONGESTION_PACKET_SIZE];
        format_congestion(&mut packet, congestion, key_pos);
        if self.gkcrypt_local.is_some() {
            let tag = self.packet_mac(key_pos, &mut packet)?;
            packet[7..11].copy_from_slice(&tag);
        }
        Ok(packet.to_vec())
    }

    fn build_feedback(&mut self, seq_num: u16, payload: &[u8]) -> Result<Vec<u8>> {
        let key_pos = self.crypt_advance(payload.len() + GKCRYPT_BLOCK_SIZE);
        let mut packet = vec![0u8; 0xc + payload.len()];
        packet[0] = PACKET_TYPE_FEEDBACK_STATE;
        packet[1..3].copy_from_slice(&seq_num.to_be_bytes());
        packet[3] = 0;
        packet[4..8].copy_from_slice(&(key_pos as u32).to_be_bytes());
        packet[8..12].fill(0);
        packet[12..].copy_from_slice(payload);
        let crypt = self.gkcrypt_local.as_mut().ok_or(Error::Uninitialized)?;
        crypt.encrypt(key_pos + GKCRYPT_BLOCK_SIZE as u64, &mut packet[12..])?;
        let tag = crypt.gmac(key_pos, &packet)?;
        packet[8..12].copy_from_slice(&tag);
        Ok(packet)
    }

    fn build_feedback_history(&mut self, seq_num: u16, payload: &[u8]) -> Result<Vec<u8>> {
        let key_pos = self.crypt_advance(payload.len() + GKCRYPT_BLOCK_SIZE);
        let mut packet = vec![0u8; 0xc + payload.len()];
        packet[0] = PACKET_TYPE_FEEDBACK_HISTORY;
        packet[1..3].copy_from_slice(&seq_num.to_be_bytes());
        packet[3] = 0;
        packet[4..8].copy_from_slice(&(key_pos as u32).to_be_bytes());
        packet[8..12].fill(0);
        packet[12..].copy_from_slice(payload);
        let crypt = self.gkcrypt_local.as_mut().ok_or(Error::Uninitialized)?;
        crypt.encrypt(key_pos + GKCRYPT_BLOCK_SIZE as u64, &mut packet[12..])?;
        let tag = crypt.gmac(key_pos, &packet)?;
        packet[8..12].copy_from_slice(&tag);
        Ok(packet)
    }

    fn build_feedback_state(&mut self, seq_num: u16, state: FeedbackState) -> Result<Vec<u8>> {
        let payload = if self.version <= 9 {
            format_v9(&state).to_vec()
        } else {
            format_v12(&state).to_vec()
        };
        self.build_feedback(seq_num, &payload)
    }

    fn flush_data_queue(
        &mut self,
        events: &mpsc::UnboundedSender<TakionEvent>,
    ) -> Result<Option<u32>> {
        let mut last_ack = None;
        while let Some((seq, entry)) = self.data_queue.pull() {
            last_ack = Some(seq);
            match entry.data_type {
                0 | 7 | 9 | 11 => {
                    let _ = events.send(TakionEvent::MessageData {
                        data_type: entry.data_type,
                        payload: entry.payload,
                    });
                }
                other => tracing::warn!("takion data type {other} ignored"),
            }
        }
        Ok(last_ack)
    }
}

pub(crate) fn make_socket(addr: SocketAddr) -> Result<UdpSocket> {
    let domain = if addr.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let socket =
        Socket::new(domain, Type::DGRAM, Some(Protocol::UDP)).map_err(|_| Error::Network)?;
    // Absorb a burst while frame assembly is busy. The kernel may cap this
    // below the request; buffers are an optimisation, not a hard requirement.
    let _ = socket.set_recv_buffer_size(4 << 20);
    let _ = socket.set_send_buffer_size(4 << 20);
    socket.connect(&addr.into()).map_err(|_| Error::Network)?;
    socket.set_nonblocking(true).map_err(|_| Error::Network)?;
    let std_sock: std::net::UdpSocket = socket.into();
    UdpSocket::from_std(std_sock).map_err(|_| Error::Network)
}

async fn recv_handshake(sock: &UdpSocket, buf: &mut [u8]) -> Result<usize> {
    sock.recv(buf).await.map_err(|_| Error::Network)
}

fn parse_handshake_message(packet: &[u8], tag_local: u32) -> Result<(MessageHeader, &[u8])> {
    if packet.len() < 1 + MESSAGE_HEADER_SIZE {
        return Err(Error::InvalidResponse);
    }
    if packet[0] != PACKET_TYPE_CONTROL {
        return Err(Error::InvalidResponse);
    }
    let mut key_state = KeyState::new();
    let header = parse_message(&packet[1..], tag_local, &mut key_state)?;
    let payload = &packet[1 + header.payload_offset..];
    Ok((header, payload))
}

pub(crate) async fn handshake(sock: &UdpSocket, version: u8) -> Result<(u32, u32, u32)> {
    let tag_local = crate::util::rng::u32();
    let seq_num_local = tag_local;
    let mut init = [0u8; 1 + MESSAGE_HEADER_SIZE + 0x10];
    init[0] = PACKET_TYPE_CONTROL;
    write_message_header(&mut init[1..], 0, 0, CHUNK_TYPE_INIT, 0, 0x10);
    let p = &mut init[1 + MESSAGE_HEADER_SIZE..];
    p[0..4].copy_from_slice(&tag_local.to_be_bytes());
    p[4..8].copy_from_slice(&A_RWND.to_be_bytes());
    p[8..10].copy_from_slice(&OUTBOUND_STREAMS.to_be_bytes());
    p[10..12].copy_from_slice(&INBOUND_STREAMS.to_be_bytes());
    p[12..16].copy_from_slice(&seq_num_local.to_be_bytes());
    sock.send(&init).await.map_err(|_| Error::Network)?;
    tracing::info!("takion: sending INIT (version {version}, tag {tag_local:#x})");

    let mut buf = [0u8; 2048];
    let (tag_remote, cookie, _outbound, _inbound) = timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            let n = recv_handshake(sock, &mut buf).await?;
            let packet = &buf[..n];
            if packet.len() != 1 + MESSAGE_HEADER_SIZE + 0x10 + COOKIE_SIZE {
                continue;
            }
            let (header, payload) = parse_handshake_message(packet, tag_local)?;
            if header.chunk_type != CHUNK_TYPE_INIT_ACK || header.chunk_flags != 0 {
                continue;
            }
            if payload.len() != 0x10 + COOKIE_SIZE {
                return Err(Error::InvalidResponse);
            }
            let tag = u32::from_be_bytes(payload[0..4].try_into().unwrap());
            if tag == 0 {
                return Err(Error::InvalidResponse);
            }
            let outbound = u16::from_be_bytes(payload[8..10].try_into().unwrap());
            let inbound = u16::from_be_bytes(payload[10..12].try_into().unwrap());
            if outbound == 0
                || inbound == 0
                || outbound > INBOUND_STREAMS
                || inbound < OUTBOUND_STREAMS
            {
                return Err(Error::InvalidResponse);
            }
            let mut cookie = [0u8; COOKIE_SIZE];
            cookie.copy_from_slice(&payload[0x10..]);
            let initial_seq = u32::from_be_bytes(payload[12..16].try_into().unwrap());
            tracing::info!(
                "takion: received INIT_ACK (remote tag {tag:#x}, initial data seq {initial_seq:#x})"
            );
            return Ok::<_, Error>((tag, cookie, outbound, inbound));
        }
    })
    .await
    .map_err(|_| Error::Timeout)??;

    // Send COOKIE.
    let mut msg = [0u8; 1 + MESSAGE_HEADER_SIZE + COOKIE_SIZE];
    msg[0] = PACKET_TYPE_CONTROL;
    write_message_header(
        &mut msg[1..],
        tag_remote,
        0,
        CHUNK_TYPE_COOKIE,
        0,
        COOKIE_SIZE,
    );
    msg[1 + MESSAGE_HEADER_SIZE..].copy_from_slice(&cookie);
    sock.send(&msg).await.map_err(|_| Error::Network)?;
    tracing::info!("takion: sending COOKIE");

    timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            let n = recv_handshake(sock, &mut buf).await?;
            let packet = &buf[..n];
            if packet.len() < 1 + MESSAGE_HEADER_SIZE {
                continue;
            }
            let (header, payload) = parse_handshake_message(packet, tag_local)?;
            if header.chunk_type == CHUNK_TYPE_COOKIE_ACK && header.chunk_flags == 0 {
                if !payload.is_empty() {
                    return Err(Error::InvalidResponse);
                }
                tracing::info!("takion: received COOKIE_ACK");
                return Ok::<_, Error>(());
            }
        }
    })
    .await
    .map_err(|_| Error::Timeout)??;

    Ok((tag_local, tag_remote, seq_num_local))
}

/// Connect and perform the Takion handshake, then spawn the driver task.
pub async fn connect(addr: SocketAddr, info: TakionConnectInfo) -> Result<TakionHandle> {
    tracing::info!("takion: connecting to {addr} (version {})", info.version);
    let sock = make_socket(addr)?;
    let (tag_local, tag_remote, seq_num_local) = handshake(&sock, info.version).await?;
    tracing::info!("takion: connected (local tag {tag_local:#x}, remote tag {tag_remote:#x})");
    let (cmd_tx, cmd_rx) = mpsc::channel(256);
    let (evt_tx, evt_rx) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();

    let state = State {
        version: info.version,
        enable_crypt: info.enable_crypt,
        tag_local,
        tag_remote,
        key_state: KeyState::new(),
        gkcrypt_local: None,
        gkcrypt_remote: None,
        key_pos_local: 0,
        seq_num_local,
        a_rwnd: A_RWND,
        data_queue: ReorderQueue::new(DATA_QUEUE_EXP, tag_remote),
        data_seen: false,
        ack_logs: 0,
        send_buffer: SendBuffer::new(SEND_BUFFER_SIZE),
        postponed: Vec::new(),
        pending_ack: None,
    };

    let task_cancel = cancel.clone();
    let join = tokio::spawn(run(sock, state, cmd_rx, evt_tx, task_cancel));
    Ok(TakionHandle {
        commands: cmd_tx,
        events: evt_rx,
        cancel,
        join,
    })
}

fn log_received(datagrams: &mut u32, n: usize, buf: &[u8]) {
    *datagrams += 1;
    if *datagrams <= 8 {
        tracing::info!(
            "takion: datagram {}: {n} bytes (base type {})",
            *datagrams,
            buf[0] & PACKET_BASE_TYPE_MASK
        );
    }
}

async fn run(
    sock: UdpSocket,
    mut state: State,
    mut commands: mpsc::Receiver<TakionCommand>,
    events: mpsc::UnboundedSender<TakionEvent>,
    cancel: CancellationToken,
) {
    let mut buf = vec![0u8; RECV_BUFFER_SIZE];
    let mut tick = tokio::time::interval(Duration::from_millis(50));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut datagrams = 0u32;
    let mut last_rx = Instant::now();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            received = sock.recv(&mut buf) => {
                match received {
                    Ok(n) if n > 0 => {
                        last_rx = Instant::now();
                        log_received(&mut datagrams, n, &buf);
                        if let Err(e) = handle_datagram(&sock, &mut state, &mut buf[..n], &events).await {
                            tracing::warn!("takion packet rejected: {e}");
                        }
                        // Drain datagrams already waiting in the kernel
                        // without another scheduler round-trip. A burst that
                        // arrives while one packet is being processed would
                        // otherwise compete with a 1 MiB socket buffer.
                        for _ in 1..RECV_BATCH_MAX {
                            match sock.try_recv(&mut buf) {
                                Ok(n) if n > 0 => {
                                    last_rx = Instant::now();
                                    log_received(&mut datagrams, n, &buf);
                                    if let Err(e) =
                                        handle_datagram(&sock, &mut state, &mut buf[..n], &events).await
                                    {
                                        tracing::warn!("takion packet rejected: {e}");
                                    }
                                }
                                Ok(_) => break,
                                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                                Err(_) => break,
                            }
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            command = commands.recv() => {
                let Some(command) = command else { break };
                if !handle_command(&sock, &mut state, command, &events).await {
                    break;
                }
            }
            _ = tick.tick() => {
                if last_rx.elapsed() >= PEER_SILENCE_TIMEOUT {
                    tracing::warn!(
                        "takion: no packets from the console for {} s; closing the session",
                        PEER_SILENCE_TIMEOUT.as_secs()
                    );
                    break;
                }
                let (due, gave_up) = state.send_buffer.take_due(Instant::now());
                if !due.is_empty() {
                    tracing::info!("takion: retransmitting {} packet(s)", due.len());
                }
                for packet in due {
                    if sock.send(&packet).await.is_err() {
                        break;
                    }
                }
                if gave_up {
                    // Lost ACKs for control messages do not imply a dead
                    // console. The inbound-silence watchdog above is the
                    // liveness check; keep forwarding media in the meantime.
                    tracing::warn!(
                        "takion: dropped control packets the console did not acknowledge; media continues"
                    );
                }
            }
        }
    }
    let _ = events.send(TakionEvent::Disconnected);
}

async fn send_data_packet(
    sock: &UdpSocket,
    state: &mut State,
    packet: Result<Vec<u8>>,
    channel: u16,
) -> bool {
    match packet {
        Ok(packet) => {
            if sock.send(&packet).await.is_err() {
                return false;
            }
            let seq =
                u32::from_be_bytes(packet[1 + MESSAGE_HEADER_SIZE..][0..4].try_into().unwrap());
            if state.gkcrypt_local.is_none() {
                tracing::info!(
                    "takion: sending pre-crypt DATA seq {seq:#x} channel {channel} ({} bytes)",
                    packet.len()
                );
            }
            let now = Instant::now();
            let _ = state.send_buffer.push(seq, packet, now);
        }
        Err(e) => tracing::warn!("takion send failed: {e}"),
    }
    true
}

async fn handle_command(
    sock: &UdpSocket,
    state: &mut State,
    command: TakionCommand,
    events: &mpsc::UnboundedSender<TakionEvent>,
) -> bool {
    match command {
        TakionCommand::SendMessageData {
            flags,
            channel,
            payload,
        } => {
            let packet = state.build_message_data(flags, channel, &payload);
            if !send_data_packet(sock, state, packet, channel).await {
                return false;
            }
        }
        TakionCommand::SendMessageDataCont {
            flags,
            channel,
            payload,
        } => {
            let packet = state.build_message_data_cont(flags, channel, &payload);
            if !send_data_packet(sock, state, packet, channel).await {
                return false;
            }
        }
        TakionCommand::SendFeedbackState { seq, state: fb } => {
            if let Ok(packet) = state.build_feedback_state(seq, fb) {
                let _ = sock.send(&packet).await;
            }
        }
        TakionCommand::SendFeedbackHistory { seq, payload } => {
            if let Ok(packet) = state.build_feedback_history(seq, &payload) {
                let _ = sock.send(&packet).await;
            }
        }
        TakionCommand::SendCongestion(packet) => {
            if let Ok(packet) = state.build_congestion(packet) {
                let _ = sock.send(&packet).await;
            }
        }
        TakionCommand::SendCorruptFrame { start, end } => {
            let payload = crate::stream::protobuf::encode_corrupt_frame(start, end);
            let packet = state.build_stream_control(&payload);
            if !send_data_packet(sock, state, packet, STREAM_CONTROL_CHANNEL).await {
                tracing::warn!("takion: corrupt-frame report send failed");
            }
        }
        TakionCommand::RequestIdr => {
            let payload = crate::stream::protobuf::encode_idr_request();
            let packet = state.build_stream_control(&payload);
            if !send_data_packet(sock, state, packet, STREAM_CONTROL_CHANNEL).await {
                tracing::warn!("takion: IDR request send failed");
            }
        }
        TakionCommand::SetCrypt { local, remote } => {
            state.gkcrypt_local = Some(*local);
            state.gkcrypt_remote = Some(*remote);
            // The BIG packet was sent without a MAC. The console already
            // answered it, so there is no reason to retransmit it.
            state.send_buffer.clear();
            // ACKs generated for the BANG were held back until a MAC key
            // existed; send the cumulative ACK now.
            if let Some(seq) = state.pending_ack.take()
                && let Ok(ack) = state.build_data_ack(seq)
            {
                let _ = sock.send(&ack).await;
            }
            // Re-process AV packets that arrived before encryption was ready.
            let postponed = std::mem::take(&mut state.postponed);
            for mut packet in postponed {
                let _ = handle_datagram(sock, state, &mut packet, events).await;
            }
        }
        TakionCommand::Close => return false,
    }
    true
}

async fn handle_datagram(
    sock: &UdpSocket,
    state: &mut State,
    buf: &mut [u8],
    events: &mpsc::UnboundedSender<TakionEvent>,
) -> Result<()> {
    if buf.is_empty() {
        return Err(Error::BufferTooSmall);
    }
    let base = base_type(buf)?;
    let is_av = matches!(base, PACKET_TYPE_VIDEO | PACKET_TYPE_AUDIO);

    if is_av && state.enable_crypt && state.gkcrypt_remote.is_none() {
        if state.postponed.len() < POSTPONE_MAX {
            state.postponed.push(buf.to_vec());
        } else {
            tracing::warn!("takion postpone buffer full");
        }
        return Ok(());
    }

    if state.gkcrypt_remote.is_some() {
        state.verify_packet_mac(buf)?;
    }

    match base {
        PACKET_TYPE_CONTROL => {
            let header = parse_message(&buf[1..], state.tag_local, &mut state.key_state)?;
            match header.chunk_type {
                CHUNK_TYPE_DATA => {
                    let payload = &buf[1 + header.payload_offset..];
                    if payload.len() < 9 {
                        return Err(Error::InvalidData);
                    }
                    let seq = u32::from_be_bytes(payload[0..4].try_into().unwrap());
                    let channel = u16::from_be_bytes(payload[4..6].try_into().unwrap());
                    let data_type = payload[8];
                    let data = Bytes::copy_from_slice(&payload[9..]);
                    if !state.data_seen {
                        state.data_seen = true;
                        tracing::info!(
                            "takion: first DATA chunk seq {seq} on channel {channel} \
                             (remote tag {:#x}); anchoring the reorder window",
                            state.tag_remote
                        );
                        // Chiaki assumes DATA starts at the console tag. Some
                        // consoles start at a different sequence (the
                        // INIT_ACK initial sequence), which would leave the
                        // BANG outside a tag-anchored window forever.
                        state.data_queue = ReorderQueue::new(DATA_QUEUE_EXP, seq);
                    }
                    if let Some(dropped) = state.data_queue.push(
                        seq,
                        DataEntry {
                            data_type,
                            payload: data,
                        },
                    ) {
                        tracing::warn!(
                            "takion dropped data seq {seq} on channel {channel}: {dropped:?}",
                            dropped = dropped.data_type
                        );
                    }
                    if let Some(seq) = state.flush_data_queue(events)? {
                        if state.gkcrypt_local.is_some() || !state.enable_crypt {
                            if let Ok(ack) = state.build_data_ack(seq) {
                                let _ = sock.send(&ack).await;
                            }
                        } else {
                            state.pending_ack = Some(seq);
                        }
                    }
                }
                CHUNK_TYPE_DATA_ACK => {
                    let payload = &buf[1 + header.payload_offset..];
                    if payload.len() < 0xc {
                        return Err(Error::InvalidData);
                    }
                    let cumulative = u32::from_be_bytes(payload[0..4].try_into().unwrap());
                    let before = state.send_buffer.len();
                    state.send_buffer.ack(cumulative);
                    // Before encryption is installed the only buffered packet
                    // is the BIG. Never let a sequence mismatch cause
                    // duplicate BIG transmissions.
                    if state.gkcrypt_local.is_none() && !state.send_buffer.is_empty() {
                        tracing::info!(
                            "takion: clearing pre-crypt send buffer on DATA_ACK seq {cumulative:#x}"
                        );
                        state.send_buffer.clear();
                    }
                    if state.ack_logs < 10 {
                        state.ack_logs += 1;
                        tracing::info!(
                            "takion: DATA_ACK seq {cumulative:#x}; send buffer {before} -> {}",
                            state.send_buffer.len()
                        );
                    }
                }
                _ => {}
            }
        }
        PACKET_TYPE_VIDEO | PACKET_TYPE_AUDIO => {
            let v12 = state.version >= 12;
            let av = parse_av(buf, v12, &mut state.key_state)?;
            if av.data_len == 0 {
                return Err(Error::InvalidData);
            }
            let crypt = state.gkcrypt_remote.as_mut().ok_or(Error::Uninitialized)?;
            crypt.decrypt(
                av.key_pos + GKCRYPT_BLOCK_SIZE as u64,
                &mut buf[av.data_offset..av.data_offset + av.data_len],
            )?;
            let data = av.data(buf);
            let _ = events.send(TakionEvent::Av { packet: av, data });
        }
        _ => {
            // Congestion/feedback/handshake packets from the console are not
            // needed by the client.
        }
    }
    Ok(())
}

#[cfg(test)]
mod format_tests {
    use super::*;

    fn test_state() -> State {
        State {
            version: 9,
            enable_crypt: false,
            tag_local: 0,
            tag_remote: 0x1122_3344,
            key_state: KeyState::new(),
            gkcrypt_local: None,
            gkcrypt_remote: None,
            key_pos_local: 0,
            seq_num_local: 0xabcd,
            a_rwnd: A_RWND,
            data_queue: ReorderQueue::new(DATA_QUEUE_EXP, 0),
            data_seen: false,
            ack_logs: 0,
            send_buffer: SendBuffer::new(SEND_BUFFER_SIZE),
            postponed: Vec::new(),
            pending_ack: None,
        }
    }

    #[test]
    fn continuation_data_header_has_no_type_byte() {
        let mut state = test_state();
        let payload = [0xde, 0xad, 0xbe, 0xef];
        let packet = state.build_message_data_cont(0, 1, &payload).unwrap();
        assert_eq!(packet.len(), 1 + MESSAGE_HEADER_SIZE + 8 + payload.len());
        assert_eq!(packet[0], PACKET_TYPE_CONTROL);
        assert_eq!(&packet[1..5], &0x1122_3344u32.to_be_bytes());
        assert_eq!(packet[12 + 1], CHUNK_TYPE_DATA);
        assert_eq!(packet[12 + 2], 0);
        // Wire length is payload_data_size + 4, i.e. 8 + payload + 4.
        assert_eq!(
            u16::from_be_bytes([packet[15], packet[16]]) as usize,
            8 + payload.len() + 4
        );
        assert_eq!(&packet[17..21], &0xabcdu32.to_be_bytes());
        assert_eq!(&packet[21..23], &1u16.to_be_bytes());
        assert_eq!(&packet[23..25], &[0, 0]);
        assert_eq!(&packet[25..], &payload);
    }

    #[test]
    fn stream_control_messages_use_flags_one_channel_two() {
        let mut state = test_state();
        let payload = crate::stream::protobuf::encode_idr_request();
        let packet = state.build_stream_control(&payload).unwrap();
        assert_eq!(packet[0], PACKET_TYPE_CONTROL);
        assert_eq!(packet[13], CHUNK_TYPE_DATA);
        assert_eq!(packet[14], STREAM_CONTROL_FLAGS);
        assert_eq!(&packet[21..23], &STREAM_CONTROL_CHANNEL.to_be_bytes());
        assert_eq!(&packet[26..], &payload);
    }

    #[test]
    fn congestion_mac_vector() {
        let handshake = [
            0x54, 0x65, 0x4c, 0x34, 0x5c, 0xac, 0x56, 0xb8, 0xea, 0xe6, 0x15, 0x2a, 0xde, 0x1c,
            0xe2, 0xe8,
        ];
        let secret = [
            0x00, 0x34, 0xf8, 0x21, 0xc7, 0xd9, 0xde, 0xa9, 0xe9, 0x11, 0xca, 0x5a, 0xd6, 0x7d,
            0x11, 0xce, 0x4f, 0x02, 0xb1, 0xce, 0x1e, 0xe7, 0xc3, 0x8d, 0x54, 0x39, 0xfa, 0x64,
            0xe3, 0xdb, 0xd8, 0x0d,
        ];
        let mut state = test_state();
        state.enable_crypt = true;
        state.tag_remote = 0;
        state.seq_num_local = 0;
        state.gkcrypt_local = Some(GKCrypt::new(2, &handshake, &secret).unwrap());
        let mut buf = [0u8; CONGESTION_PACKET_SIZE];
        format_congestion(
            &mut buf,
            CongestionPacket {
                word_0: 0x42,
                received: 26,
                lost: 10,
            },
            0x1e5,
        );
        let tag = state.packet_mac(0x1e5, &mut buf).unwrap();
        assert_eq!(tag, [0x64, 0x8a, 0x7c, 0x74]);
        assert_eq!(
            buf,
            [
                0x05, 0x00, 0x42, 0x00, 0x1a, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
                0xe5
            ]
        );
    }

    #[test]
    fn key_position_advance_is_pinned_per_packet_type() {
        let handshake = [0x11; 16];
        let secret = [0x22; 32];
        let mut state = test_state();
        state.enable_crypt = true;
        state.gkcrypt_local = Some(GKCrypt::new(2, &handshake, &secret).unwrap());
        state.key_pos_local = 0;

        // DATA advances by len + len % 16.
        state.build_message_data(1, 1, &[0; 10]).unwrap();
        assert_eq!(state.key_pos_local, 20);
        state.build_message_data_cont(0, 1, &[0; 16]).unwrap();
        assert_eq!(state.key_pos_local, 36);

        // DATA_ACK uses the fixed 29-byte size, rounded to 32.
        state.build_data_ack(0).unwrap();
        assert_eq!(state.key_pos_local, 78);

        // Congestion packets use their 15-byte wire size, rounded to 30.
        state
            .build_congestion(CongestionPacket {
                word_0: 0,
                received: 0,
                lost: 0,
            })
            .unwrap();
        assert_eq!(state.key_pos_local, 108);

        // Feedback advances by payload + 16, rounded up.
        state.build_feedback(0, &[0; 17]).unwrap();
        assert_eq!(state.key_pos_local, 142);
    }
}

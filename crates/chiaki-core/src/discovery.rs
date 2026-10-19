// SPDX-License-Identifier: AGPL-3.0-only
//! LAN discovery and wakeup.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::UdpSocket;
use tokio::time::{Instant, timeout_at};

use crate::error::{Error, Result};
use crate::http;
use crate::target::Target;

pub const DISCOVERY_PORT_PS4: u16 = 987;
pub const DISCOVERY_PORT_PS5: u16 = 9302;
pub const PROTOCOL_VERSION_PS4: &str = "00020020";
pub const PROTOCOL_VERSION_PS5: &str = "00030010";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostState {
    Unknown,
    Ready,
    Standby,
}

impl HostState {
    pub fn as_str(self) -> &'static str {
        match self {
            HostState::Ready => "ready",
            HostState::Standby => "standby",
            HostState::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DiscoveryHost {
    pub host_addr: String,
    pub state: Option<HostState>,
    pub host_request_port: u16,
    pub system_version: String,
    pub device_discovery_protocol_version: String,
    pub host_name: String,
    pub host_type: String,
    pub host_id: String,
    pub running_app_titleid: String,
    pub running_app_name: String,
}

impl DiscoveryHost {
    pub fn is_ps5(&self) -> bool {
        self.device_discovery_protocol_version == PROTOCOL_VERSION_PS5
    }

    pub fn target(&self) -> Target {
        Target::from_system_version(&self.system_version, self.is_ps5())
    }
}

pub fn srch_packet(ps5: bool) -> Vec<u8> {
    let version = if ps5 {
        PROTOCOL_VERSION_PS5
    } else {
        PROTOCOL_VERSION_PS4
    };
    let mut v =
        format!("SRCH * HTTP/1.1\ndevice-discovery-protocol-version:{version}\n").into_bytes();
    v.push(0);
    v
}

pub fn wakeup_packet(credential: u64, ps5: bool) -> Vec<u8> {
    let version = if ps5 {
        PROTOCOL_VERSION_PS5
    } else {
        PROTOCOL_VERSION_PS4
    };
    let mut v = format!(
        "WAKEUP * HTTP/1.1\nclient-type:vr\nauth-type:R\nmodel:w\napp-type:r\nuser-credential:{credential}\ndevice-discovery-protocol-version:{version}\n"
    )
    .into_bytes();
    v.push(0);
    v
}

pub fn parse_srch_response(addr: SocketAddr, buf: &[u8]) -> Result<DiscoveryHost> {
    let response = http::parse_response(buf)?;
    let state = match response.code {
        200 => HostState::Ready,
        620 => HostState::Standby,
        _ => HostState::Unknown,
    };
    let mut host = DiscoveryHost {
        host_addr: addr.ip().to_string(),
        state: Some(state),
        ..Default::default()
    };
    for h in &response.headers {
        match h.key.as_str() {
            "system-version" => host.system_version = h.value.clone(),
            "device-discovery-protocol-version" => {
                host.device_discovery_protocol_version = h.value.clone()
            }
            "host-request-port" => {
                host.host_request_port = h.value.parse().unwrap_or(0);
            }
            "host-name" => host.host_name = h.value.clone(),
            "host-type" => host.host_type = h.value.clone(),
            "host-id" => host.host_id = h.value.clone(),
            "running-app-titleid" => host.running_app_titleid = h.value.clone(),
            "running-app-name" => host.running_app_name = h.value.clone(),
            _ => {}
        }
    }
    Ok(host)
}

fn broadcast_addresses() -> Vec<Ipv4Addr> {
    let mut out = vec![Ipv4Addr::BROADCAST];
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for iface in ifaces {
            if let if_addrs::IfAddr::V4(v4) = iface.addr {
                let ip = u32::from(v4.ip);
                let mask = u32::from(v4.netmask);
                let broadcast = ip | !mask;
                let addr = Ipv4Addr::from(broadcast);
                if !out.contains(&addr) {
                    out.push(addr);
                }
            }
        }
    }
    out
}

/// Send SRCH on both protocols and collect responses for `duration`.
pub async fn discover(duration: Duration) -> Result<Vec<DiscoveryHost>> {
    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|_| Error::Network)?;
    sock.set_broadcast(true).map_err(|_| Error::Network)?;

    let srch4 = srch_packet(false);
    let srch5 = srch_packet(true);
    for addr in broadcast_addresses() {
        let _ = sock
            .send_to(
                &srch4,
                SocketAddr::new(IpAddr::V4(addr), DISCOVERY_PORT_PS4),
            )
            .await;
        let _ = sock
            .send_to(
                &srch5,
                SocketAddr::new(IpAddr::V4(addr), DISCOVERY_PORT_PS5),
            )
            .await;
    }

    let deadline = Instant::now() + duration;
    let mut hosts: BTreeMap<String, DiscoveryHost> = BTreeMap::new();
    let mut buf = [0u8; 1024];
    loop {
        let received = timeout_at(deadline, sock.recv_from(&mut buf)).await;
        match received {
            Ok(Ok((n, peer))) => {
                if let Ok(host) = parse_srch_response(peer, &buf[..n]) {
                    let key = if host.host_id.is_empty() {
                        host.host_addr.clone()
                    } else {
                        host.host_id.clone()
                    };
                    hosts.insert(key, host);
                }
            }
            Ok(Err(_)) => return Err(Error::Network),
            Err(_) => break,
        }
    }
    Ok(hosts.into_values().collect())
}

/// Send SRCH to one host on both discovery ports and return the first
/// response. This is the unicast form used when a user enters a console IP
/// instead of broadcasting.
pub async fn discover_host(host: IpAddr, duration: Duration) -> Result<Option<DiscoveryHost>> {
    if !host.is_ipv4() {
        return Err(Error::Unsupported("discovery is IPv4-only".into()));
    }
    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|_| Error::Network)?;
    let srch4 = srch_packet(false);
    let srch5 = srch_packet(true);
    for (packet, port) in [(&srch4, DISCOVERY_PORT_PS4), (&srch5, DISCOVERY_PORT_PS5)] {
        let _ = sock.send_to(packet, SocketAddr::new(host, port)).await;
    }

    let deadline = Instant::now() + duration;
    let mut buf = [0u8; 1024];
    loop {
        match timeout_at(deadline, sock.recv_from(&mut buf)).await {
            Ok(Ok((n, peer))) => {
                if peer.ip() == host
                    && let Ok(host) = parse_srch_response(peer, &buf[..n])
                {
                    return Ok(Some(host));
                }
            }
            Ok(Err(_)) => return Err(Error::Network),
            Err(_) => return Ok(None),
        }
    }
}

/// Send a wakeup packet. The reference client parses the regist key's raw
/// bytes as an ASCII hex string; we do the same and report unsupported when
/// the key does not fit.
pub async fn wakeup(host: IpAddr, regist_key: &[u8; 16], ps5: bool) -> Result<()> {
    let end = regist_key.iter().position(|&b| b == 0).unwrap_or(16);
    let key = &regist_key[..end];
    if key.len() > 8 {
        return Err(Error::Unsupported(
            "wakeup requires a regist key of at most 8 hex characters".into(),
        ));
    }
    let text = std::str::from_utf8(key)
        .map_err(|_| Error::Unsupported("regist key is not ASCII hex".into()))?;
    let credential = u64::from_str_radix(text, 16)
        .map_err(|_| Error::Unsupported("regist key is not ASCII hex".into()))?;
    let sock = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|_| Error::Network)?;
    let packet = wakeup_packet(credential, ps5);
    let port = if ps5 {
        DISCOVERY_PORT_PS5
    } else {
        DISCOVERY_PORT_PS4
    };
    sock.send_to(&packet, SocketAddr::new(host, port))
        .await
        .map_err(|_| Error::Network)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddrV4;

    #[test]
    fn srch_packet_format() {
        assert_eq!(
            srch_packet(false),
            b"SRCH * HTTP/1.1\ndevice-discovery-protocol-version:00020020\n\0"
        );
        assert_eq!(
            srch_packet(true),
            b"SRCH * HTTP/1.1\ndevice-discovery-protocol-version:00030010\n\0"
        );
    }

    #[test]
    fn wakeup_packet_format() {
        let p = wakeup_packet(0x1234, true);
        let s = String::from_utf8_lossy(&p);
        assert!(s.starts_with("WAKEUP * HTTP/1.1\n"));
        assert!(s.contains("user-credential:4660\n"));
        assert!(s.contains("device-discovery-protocol-version:00030010\n"));
        assert_eq!(*p.last().unwrap(), 0);
    }

    #[test]
    fn parse_ready_ps5() {
        let buf = b"HTTP/1.1 200 OK\r\nhost-id: abc\r\nhost-name: Living Room\r\nhost-type: PS5\r\nhost-request-port: 9295\r\nsystem-version: 8050001\r\ndevice-discovery-protocol-version: 00030010\r\nrunning-app-titleid: PPSA00000\r\nrunning-app-name: Game\r\n\r\n";
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 20), 9302));
        let host = parse_srch_response(addr, buf).unwrap();
        assert_eq!(host.host_name, "Living Room");
        assert_eq!(host.state, Some(HostState::Ready));
        assert_eq!(host.host_request_port, 9295);
        assert!(host.is_ps5());
        assert_eq!(host.target(), Target::Ps5_1);
    }

    #[test]
    fn parse_standby_ps4() {
        let buf =
            b"HTTP/1.1 620 Standby\r\nhost-id: x\r\nsystem-version: 9000000\r\ndevice-discovery-protocol-version: 00020020\r\n\r\n";
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 21), 987));
        let host = parse_srch_response(addr, buf).unwrap();
        assert_eq!(host.state, Some(HostState::Standby));
        assert_eq!(host.target(), Target::Ps4_10);
    }

    #[test]
    fn target_from_system_versions() {
        assert_eq!(Target::from_system_version("7000000", false), Target::Ps4_9);
        assert_eq!(Target::from_system_version("6500000", false), Target::Ps4_8);
        assert_eq!(Target::from_system_version("8050001", true), Target::Ps5_1);
    }

    #[tokio::test]
    async fn unicast_rejects_ipv6() {
        let result = discover_host(
            std::net::Ipv6Addr::LOCALHOST.into(),
            Duration::from_millis(1),
        )
        .await;
        assert!(matches!(result, Err(Error::Unsupported(_))));
    }
}

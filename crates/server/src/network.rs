// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! LAN address discovery and terminal QR rendering for the startup banner.

use std::net::{IpAddr, SocketAddr};

pub fn url_for(ip: IpAddr, port: u16) -> String {
    match ip {
        IpAddr::V4(ip) => format!("http://{ip}:{port}"),
        IpAddr::V6(ip) => format!("http://[{ip}]:{port}"),
    }
}

pub fn lan_urls(bind: SocketAddr) -> Vec<String> {
    let interfaces: Vec<(String, IpAddr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|iface| iface.is_oper_up())
        .map(|iface| (iface.name, iface.addr.ip()))
        .collect();
    urls_for(bind, &interfaces)
}

fn urls_for(bind: SocketAddr, interfaces: &[(String, IpAddr)]) -> Vec<String> {
    if bind.ip().is_loopback() {
        return Vec::new();
    }

    let port = bind.port();
    let mut entries: Vec<(u8, String)> = Vec::new();
    if bind.ip().is_unspecified() {
        for (name, ip) in interfaces {
            if ip.is_loopback() || ip.is_unspecified() || is_link_local(*ip) {
                continue;
            }
            entries.push((interface_rank(name), url_for(*ip, port)));
        }
    } else if !is_link_local(bind.ip()) {
        entries.push((0, url_for(bind.ip(), port)));
    }

    // Prefer IPv4 for the connection banner; IPv6 addresses are kept only when
    // no IPv4 address is available (for example, an IPv6-only LAN).
    if entries.iter().any(|(_, url)| !url.starts_with("http://[")) {
        entries.retain(|(_, url)| !url.starts_with("http://["));
    }

    entries.sort_by(|a, b| a.1.cmp(&b.1));
    entries.dedup_by(|a, b| a.1 == b.1);
    entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    entries.into_iter().map(|(_, url)| url).collect()
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_unicast_link_local(),
    }
}

fn interface_rank(name: &str) -> u8 {
    let name = name.to_ascii_lowercase();
    if name.starts_with("en") || name.starts_with("wl") {
        0
    } else if name.starts_with("utun")
        || name.starts_with("bridge")
        || name.starts_with("docker")
        || name.starts_with("veth")
    {
        2
    } else {
        1
    }
}

/// Render a QR code as ANSI black-on-white terminal cells. Returns `None` if
/// the data is too large for a QR code.
pub fn terminal_qr(data: &str) -> Option<String> {
    use qrcode::QrCode;
    use qrcode::types::Color;

    let code = QrCode::new(data.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let quiet = 4usize;
    let total = width + quiet * 2;
    let dark = |x: usize, y: usize| {
        if x < quiet || y < quiet {
            return false;
        }
        let x = x - quiet;
        let y = y - quiet;
        x < width && y < width && colors[y * width + x] == Color::Dark
    };

    let mut lines = Vec::new();
    let mut y = 0;
    while y < total {
        let mut line = String::from("\x1b[30;47m");
        for x in 0..total {
            let top = dark(x, y);
            let bottom = dark(x, y + 1);
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        line.push_str("\x1b[0m");
        lines.push(line);
        y += 2;
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::{lan_urls, terminal_qr, url_for, urls_for};

    fn ip(value: &str) -> std::net::IpAddr {
        value.parse().expect("IP address")
    }

    #[test]
    fn skips_loopback_and_link_local_interfaces() {
        let bind = "0.0.0.0:8080".parse().expect("bind");
        let interfaces = vec![
            ("lo0".to_string(), ip("127.0.0.1")),
            ("en0".to_string(), ip("192.168.1.20")),
            ("en5".to_string(), ip("169.254.10.20")),
        ];
        assert_eq!(
            urls_for(bind, &interfaces),
            vec!["http://192.168.1.20:8080".to_string()]
        );
    }

    #[test]
    fn prefers_ipv4_when_both_are_available() {
        let bind = "0.0.0.0:8080".parse().expect("bind");
        let interfaces = vec![
            ("en1".to_string(), ip("192.168.1.20")),
            ("en1".to_string(), ip("2a02:6b6f:eabc:ab00::1")),
        ];
        assert_eq!(
            urls_for(bind, &interfaces),
            vec!["http://192.168.1.20:8080".to_string()]
        );
    }

    #[test]
    fn loopback_bind_has_no_lan_urls() {
        let bind = "127.0.0.1:8080".parse().expect("bind");
        assert!(urls_for(bind, &[]).is_empty());
        assert!(lan_urls(bind).is_empty());
    }

    #[test]
    fn specific_bind_reports_that_address() {
        let bind = "192.168.1.20:9000".parse().expect("bind");
        let interfaces = vec![("en0".to_string(), ip("192.168.1.20"))];
        assert_eq!(
            urls_for(bind, &interfaces),
            vec!["http://192.168.1.20:9000".to_string()]
        );
    }

    #[test]
    fn formats_ipv6_with_brackets() {
        assert_eq!(
            url_for(ip("2a02:6b6f:eabc:ab00::1"), 8080),
            "http://[2a02:6b6f:eabc:ab00::1]:8080"
        );
    }

    #[test]
    fn renders_terminal_qr() {
        let qr = terminal_qr("http://192.168.1.20:8080").expect("qr");
        assert!(qr.contains("\x1b[30;47m"));
        assert!(qr.contains('█'));
        assert!(qr.lines().count() > 10);
    }
}

// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Rahul Pramod Jaguste and contributors
//! Reference check for the launch-spec RPCrypt encryption:
//! `<nonce-hex> <morning-hex> <json> <counter>`.

use chiaki_core::crypto::rpcrypt::RPCrypt;
use chiaki_core::target::Target;

fn parse_hex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex"))
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 5 {
        eprintln!("usage: encrypt_launch <nonce-hex> <morning-hex> <json> <counter>");
        std::process::exit(2);
    }
    let nonce: [u8; 16] = parse_hex(&args[1]).try_into().expect("nonce 16 bytes");
    let morning: [u8; 16] = parse_hex(&args[2]).try_into().expect("morning 16 bytes");
    let counter: u64 = args[4].parse().expect("counter");
    let mut data = args[3].as_bytes().to_vec();
    data.push(0);
    let crypt = RPCrypt::init_auth(Target::Ps5_1, &nonce, &morning).expect("rpcrypt init");
    crypt.encrypt(counter, &mut data).expect("encrypt");
    let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
    println!("{hex}");
}

// SPDX-License-Identifier: AGPL-3.0-only
//! Decrypt a captured launch spec: `<nonce-b64> <morning-b64> <launch-b64>`.

use chiaki_core::crypto::rpcrypt::RPCrypt;
use chiaki_core::target::Target;
use chiaki_core::util::base64;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: decrypt_launch <nonce-b64> <morning-b64> <launch-b64>");
        std::process::exit(2);
    }
    let nonce = base64::decode(&args[1]).expect("nonce base64");
    let morning = base64::decode(&args[2]).expect("morning base64");
    let mut data = base64::decode(&args[3]).expect("launch base64");
    let nonce: [u8; 16] = nonce.try_into().expect("nonce must be 16 bytes");
    let morning: [u8; 16] = morning.try_into().expect("morning must be 16 bytes");
    let crypt = RPCrypt::init_auth(Target::Ps5_1, &nonce, &morning).expect("rpcrypt init");
    crypt.decrypt(0, &mut data).expect("decrypt");
    println!("{}", String::from_utf8_lossy(&data));
}

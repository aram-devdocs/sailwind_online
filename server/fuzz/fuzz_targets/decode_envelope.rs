#![no_main]
//! Fuzz target for the wire decoder.
//!
//! `sw_contracts::decode_envelope` is the sole entry point for every inbound
//! datagram, run on arbitrary, possibly-hostile bytes straight off the socket.
//! Its contract is that malformed input yields `Err` — never a panic, never
//! undefined behaviour. This target hands libFuzzer's coverage-guided input
//! straight to it and then walks the accessor surface on any buffer that
//! verifies, so a regression that reintroduces a panic or an out-of-bounds read
//! shows up as a fuzzing crash. The stable `decode_rejects_garbage` unit test
//! pins the same property for a handful of inputs; this explores the rest.

use libfuzzer_sys::fuzz_target;
use sw_contracts::{decode_envelope, sw_proto as p};

fuzz_target!(|data: &[u8]| {
    if let Ok(env) = decode_envelope(data) {
        // Touch the root accessors and each verified union arm; the verifier
        // promised these are safe to read on a buffer it accepted.
        let _ = env.seq();
        match env.payload_type() {
            p::Payload::ClientHello => {
                let _ = env.payload_as_client_hello();
            }
            p::Payload::ClientState => {
                let _ = env.payload_as_client_state();
            }
            p::Payload::EconTxn => {
                let _ = env.payload_as_econ_txn();
            }
            p::Payload::MarketTradeRequest => {
                let _ = env.payload_as_market_trade_request();
            }
            p::Payload::MoorRequest => {
                let _ = env.payload_as_moor_request();
            }
            p::Payload::ChatSend => {
                let _ = env.payload_as_chat_send();
            }
            _ => {}
        }
    }
});

//! Wire contract types for Sailwind Online.
//!
//! The FlatBuffers Rust bindings are generated from `contracts/fbs/envelope.fbs`
//! by the pinned `flatc` and committed under `src/generated/`. This crate wraps
//! them in a stable, ergonomic surface: the `sw_proto` module (all generated
//! tables/structs/unions) plus a few helpers for building and reading the root
//! `Envelope`. Never edit the generated file by hand — regenerate it via
//! `make contracts`.

// The generated code is verbatim compiler output; silence its lints locally so
// the rest of this crate stays clean under the workspace deny floor. These allows
// are scoped to this module alone and must name every group the workspace/crate
// denies (a narrower deny group like `-D unused` outranks a broad `allow(warnings)`
// no matter the scope). `allow(unsafe_code)` covers the flatbuffers accessor impls;
// the crate's own hand-written code stays under the `unsafe_code` deny bar.
mod generated {
    #![allow(unused, future_incompatible, nonstandard_style, unsafe_code)]
    #![allow(clippy::all)]
    include!("generated/envelope_generated.rs");
}

/// All generated protocol types (tables, structs, the `Payload` union, and the
/// root `Envelope`), re-exported under one path shared by every server crate.
pub use generated::sw_proto;

/// Re-export the exact `flatbuffers` runtime the bindings were generated
/// against so downstream crates never risk a version skew.
pub use flatbuffers;

/// Protocol version negotiated in the hello handshake. Single source of truth
/// for both languages; mirrors `ProtocolVersion.Current` in the schema.
pub const PROTOCOL_VERSION: u16 = 1;

/// FlatBuffers file identifier stamped on every `Envelope` buffer.
pub const FILE_IDENTIFIER: &str = "SWO0";

/// Errors that can occur while decoding an [`sw_proto::Envelope`].
pub type DecodeError = flatbuffers::InvalidFlatbuffer;

/// Verify `buf` and return the root [`sw_proto::Envelope`] view.
///
/// This runs the generated verifier, so it is safe to call on arbitrary,
/// possibly-hostile datagrams: malformed input yields `Err` instead of
/// undefined behaviour.
#[inline]
pub fn decode_envelope(buf: &[u8]) -> Result<sw_proto::Envelope<'_>, DecodeError> {
    sw_proto::root_as_envelope(buf)
}

/// Finish `fbb` as a root `Envelope { seq, payload_type, payload }` buffer and
/// return the owned bytes ready to hand to the transport.
///
/// `payload` is the union value offset produced by
/// `Xxx::create(fbb, ..).as_union_value()`, and `payload_type` must be the
/// matching [`sw_proto::Payload`] discriminant.
#[inline]
pub fn finish_envelope(
    fbb: &mut flatbuffers::FlatBufferBuilder,
    seq: u32,
    payload_type: sw_proto::Payload,
    payload: flatbuffers::WIPOffset<flatbuffers::UnionWIPOffset>,
) -> Vec<u8> {
    let env = sw_proto::Envelope::create(
        fbb,
        &sw_proto::EnvelopeArgs {
            seq,
            payload_type,
            payload: Some(payload),
        },
    );
    sw_proto::finish_envelope_buffer(fbb, env);
    let out = fbb.finished_data().to_vec();
    fbb.reset();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_version_is_one() {
        // Mirrors `ProtocolVersion.Current` in envelope.fbs.
        assert_eq!(PROTOCOL_VERSION, 1);
    }

    #[test]
    fn file_identifier_matches_generated() {
        assert_eq!(FILE_IDENTIFIER, sw_proto::ENVELOPE_IDENTIFIER);
    }

    #[test]
    fn envelope_client_hello_roundtrip() {
        let mut fbb = flatbuffers::FlatBufferBuilder::new();
        let name = fbb.create_string("skipper");
        let token = fbb.create_string("tok-123");
        let hello = sw_proto::ClientHello::create(
            &mut fbb,
            &sw_proto::ClientHelloArgs {
                protocol_version: PROTOCOL_VERSION,
                display_name: Some(name),
                token: Some(token),
                ..Default::default()
            },
        );
        let bytes = finish_envelope(
            &mut fbb,
            7,
            sw_proto::Payload::ClientHello,
            hello.as_union_value(),
        );

        assert!(sw_proto::envelope_buffer_has_identifier(&bytes));
        let env = decode_envelope(&bytes).expect("valid envelope");
        assert_eq!(env.seq(), 7);
        assert_eq!(env.payload_type(), sw_proto::Payload::ClientHello);
        let got = env.payload_as_client_hello().expect("client hello payload");
        assert_eq!(got.protocol_version(), PROTOCOL_VERSION);
        assert_eq!(got.display_name(), Some("skipper"));
        assert_eq!(got.token(), Some("tok-123"));
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_envelope(&[0u8; 3]).is_err());
        assert!(decode_envelope(&[0xff; 32]).is_err());
    }
}

//! Per-message inbound validation, beyond the FlatBuffers verifier.
//!
//! The generated verifier ([`sw_contracts::decode_envelope`]) proves a datagram
//! is a *structurally* valid `Envelope`; it says nothing about the *values*
//! inside. A hostile client can still ship a non-finite or absurd float, an
//! over-long string, or a nonsensical scalar through a perfectly well-formed
//! table. These guards run at the handler boundary so a poison value never
//! reaches the cell math, the spatial index, the database, or a broadcast.
//!
//! Scope: this module owns the *motion* (position / rotation / velocity) and
//! *string-length* guards. Scalar quantity overflow and non-negativity for the
//! economy and market (`amount_gold`, `qty`, `unit_price`) are already enforced
//! authoritatively — with `checked_add` and explicit rejection — inside
//! [`sw_econ::Ledger`] / [`sw_econ::Market`]; duplicating them here would be a
//! second, driftable implementation of one concern, so it is deliberately not
//! done. See the crate module docs for the layering.

/// Largest absolute world coordinate (metres) accepted on an inbound position
/// (`ClientState`, `MoorRequest`). Sailwind's playable world is far smaller, so
/// a coordinate past this bound is a bug or a hostile packet. Clamping to the
/// bound *before* the value reaches [`sw_world::Grid::cell_of`] keeps the
/// `(x / cell_size).floor() as i32` cast finite and, crucially, keeps the
/// `center.cx + dx` block offset inside [`sw_world::cells_in_radius`] from
/// overflowing `i32` — the concrete #19 nit, where a non-finite pos saturates
/// the cast to `i32::MAX` and the block add then wraps (or panics in debug).
pub const MAX_WORLD_COORD_M: f32 = 1.0e7;

/// Largest absolute velocity component (m/s) accepted on an inbound
/// `ClientState`. Far beyond any in-game hull speed; clamping keeps a hostile
/// value at a finite magnitude so any downstream extrapolation stays bounded.
pub const MAX_VELOCITY_MPS: f32 = 1.0e4;

/// A sanitized motion triplet: every component is finite and within bounds, so
/// it is safe to feed to the grid, the index, and a snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub pos: [f32; 3],
    pub rot: [f32; 4],
    pub vel: [f32; 3],
}

/// A finite scalar clamped into `[-limit, limit]`, or `None` when the input is
/// NaN or infinite. Rejecting on non-finite input (rather than substituting a
/// fabricated value) is deliberate: a NaN or Inf is never a legitimate motion
/// component, so the caller drops the whole message.
fn finite_clamped(v: f32, limit: f32) -> Option<f32> {
    if v.is_finite() {
        Some(v.clamp(-limit, limit))
    } else {
        None
    }
}

/// Sanitize an inbound `(pos, rot, vel)` motion triplet.
///
/// Returns `None` if *any* component is non-finite (NaN/Inf) — the caller then
/// drops the message rather than moving the player to a fabricated cell. On
/// success every position component is clamped into
/// `[-MAX_WORLD_COORD_M, MAX_WORLD_COORD_M]`, every velocity component into
/// `[-MAX_VELOCITY_MPS, MAX_VELOCITY_MPS]`, and every quaternion component into
/// `[-1.0, 1.0]` (a unit quaternion's components never exceed that magnitude).
pub fn sanitize_motion(pos: [f32; 3], rot: [f32; 4], vel: [f32; 3]) -> Option<Motion> {
    let pos = [
        finite_clamped(pos[0], MAX_WORLD_COORD_M)?,
        finite_clamped(pos[1], MAX_WORLD_COORD_M)?,
        finite_clamped(pos[2], MAX_WORLD_COORD_M)?,
    ];
    let rot = [
        finite_clamped(rot[0], 1.0)?,
        finite_clamped(rot[1], 1.0)?,
        finite_clamped(rot[2], 1.0)?,
        finite_clamped(rot[3], 1.0)?,
    ];
    let vel = [
        finite_clamped(vel[0], MAX_VELOCITY_MPS)?,
        finite_clamped(vel[1], MAX_VELOCITY_MPS)?,
        finite_clamped(vel[2], MAX_VELOCITY_MPS)?,
    ];
    Some(Motion { pos, rot, vel })
}

/// True when `s` fits within `max_len` bytes. A wire string longer than the
/// configured cap (see [`crate::config::Config::max_wire_string_len_usize`]) is
/// rejected by the caller: an over-long chat line, econ note, or mooring name is
/// hostile input, and refusing it keeps a single datagram from amplifying into
/// unbounded stored or rebroadcast bytes.
pub fn string_within_limit(s: &str, max_len: usize) -> bool {
    s.len() <= max_len
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finite_in_bounds_is_passed_through() {
        let m = sanitize_motion([1.0, 2.0, 3.0], [0.0, 0.0, 0.0, 1.0], [4.0, 5.0, 6.0]).unwrap();
        assert_eq!(m.pos, [1.0, 2.0, 3.0]);
        assert_eq!(m.rot, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(m.vel, [4.0, 5.0, 6.0]);
    }

    #[test]
    fn non_finite_position_is_rejected() {
        // Each of NaN and both infinities, in each position slot, drops the whole
        // motion: the handler must never hand a non-finite coordinate to the grid.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for slot in 0..3 {
                let mut pos = [0.0f32; 3];
                pos[slot] = bad;
                assert!(
                    sanitize_motion(pos, [0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0]).is_none(),
                    "pos[{slot}] = {bad} must be rejected"
                );
            }
        }
    }

    #[test]
    fn non_finite_rotation_or_velocity_is_rejected() {
        assert!(
            sanitize_motion([0.0, 0.0, 0.0], [f32::NAN, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0]).is_none()
        );
        assert!(sanitize_motion(
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
            [f32::INFINITY, 0.0, 0.0]
        )
        .is_none());
    }

    #[test]
    fn huge_finite_position_is_clamped_into_bounds() {
        // A finite-but-absurd coordinate (which would otherwise saturate the
        // `as i32` cast and overflow the block add) is clamped to the bound, so
        // the downstream cell math stays finite.
        let m = sanitize_motion(
            [1.0e30, -1.0e30, 5.0],
            [0.0, 0.0, 0.0, 1.0],
            [0.0, 0.0, 0.0],
        )
        .unwrap();
        assert_eq!(m.pos[0], MAX_WORLD_COORD_M);
        assert_eq!(m.pos[1], -MAX_WORLD_COORD_M);
        assert_eq!(m.pos[2], 5.0);
    }

    #[test]
    fn huge_finite_velocity_and_rotation_are_clamped() {
        let m =
            sanitize_motion([0.0, 0.0, 0.0], [9.0, -9.0, 0.0, 1.0], [1.0e9, -1.0e9, 0.0]).unwrap();
        assert_eq!(m.rot[0], 1.0);
        assert_eq!(m.rot[1], -1.0);
        assert_eq!(m.vel[0], MAX_VELOCITY_MPS);
        assert_eq!(m.vel[1], -MAX_VELOCITY_MPS);
    }

    #[test]
    fn string_length_cap_admits_short_and_rejects_long() {
        assert!(string_within_limit("", 4));
        assert!(string_within_limit("abcd", 4));
        assert!(!string_within_limit("abcde", 4));
    }
}

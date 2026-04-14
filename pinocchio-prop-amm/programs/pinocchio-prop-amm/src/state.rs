//! Pool state byte-offset constants for Pinocchio Prop AMM (304-byte account)

/// Pool state total size in bytes.
pub const POOL_SIZE: usize = 304;

// ── byte offsets ──────────────────────────────────────────────────────────────
pub const OFF_MAGIC:        usize = 0;    // u64  [0..8]
pub const OFF_ADMIN:        usize = 8;    // [u8;32]  [8..40]
pub const OFF_AUTH_BUMP:    usize = 40;   // u8   [40]
pub const OFF_DEAD_BUMP:    usize = 41;   // u8   [41]
pub const OFF_MAX_STALE:    usize = 42;   // u32  [42..46]
//                                        // [u8;2] pad [46..48]
pub const OFF_RESERVE_A:    usize = 48;   // u64  [48..56]
pub const OFF_RESERVE_B:    usize = 56;   // u64  [56..64]
pub const OFF_TARGET_A:     usize = 64;   // u64  [64..72]
pub const OFF_TARGET_B:     usize = 72;   // u64  [72..80]
pub const OFF_LP_SUPPLY:    usize = 80;   // u64  [80..88]
pub const OFF_FEE_A:        usize = 88;   // u64  [88..96]
pub const OFF_FEE_B:        usize = 96;   // u64  [96..104]
pub const OFF_ORACLE_PRICE: usize = 104;  // u64  [104..112]
pub const OFF_SPREAD_BPS:   usize = 112;  // u32  [112..116]
pub const OFF_K_PARAM:      usize = 116;  // u32  [116..120]
pub const OFF_VOL_ADJ:      usize = 120;  // u32  [120..124]
pub const OFF_FEE_BPS:      usize = 124;  // u32  [124..128]
pub const OFF_LAST_UPDATE:  usize = 128;  // i64  [128..136]
pub const OFF_TWAP_CURSOR:  usize = 136;  // u8   [136]
//                                        // [u8;7] pad [137..144]
pub const OFF_TWAP_OBS:     usize = 144;  // [(u64,i64);10]  [144..304]

pub const TWAP_SLOTS: usize = 10;

// ── sentinel values ───────────────────────────────────────────────────────────
/// "PMMPOOL1" in little-endian ASCII bytes.
pub const MAGIC: u64 = 0x504d4d504f4f4c31;

/// LP shares locked to dead PDA on first deposit.
pub const MINIMUM_LIQUIDITY: u64 = 1_000;

/// Bot may not set spread_bps + vol_adj above this value.
pub const MAX_SPREAD_BPS: u32 = 2_000;

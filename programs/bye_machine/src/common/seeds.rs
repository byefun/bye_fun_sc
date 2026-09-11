//! PDA seed byte-strings for slice 1. Dynamic segments (`pool_id`, `nft_mint`, `owner`, ...)
//! are appended at each instruction's own `#[derive(Accounts)]`, not here.
//!
//! | Account                | Seeds                                   |
//! |-------------------------|-----------------------------------------|
//! | `ProtocolConfig`        | `[PROTOCOL_CONFIG_SEED]`                |
//! | `Pool`                  | `[POOL_SEED, pool_id: u16 le]`          |
//! | `PoolCollection`        | `[COLLECTION_SEED, pool, collection]`   |
//! | Principal vault         | `[PRINCIPAL_VAULT_SEED, pool]`          |
//! | Treasury 04             | `[TREASURY_04_SEED, pool]`              |
//! | Treasury 02 identity    | `[TREASURY_02_SEED, pool]`              |
//! | `Position`               | `[POSITION_SEED, pool, nft_mint]`       |
//! | Position NFT vault      | `[POSITION_VAULT_SEED, position]`       |
//! | `WalletStats`            | `[WALLET_STATS_SEED, pool, owner]`      |
//! | `TopTier`                | `[TOP_TIER_SEED, pool]`                 |
//! | `RollBatch`              | `[BATCH_SEED, pool, batch_id: u64 le]`  |
//! | VRF request identity    | `[VRF_IDENTITY_SEED]`                   |
//!
//! A `Pool`'s own PDA doubles as its vault-signing authority — it needs no separate seed.
//! `WeightIndex` is a pre-created keypair account pinned by pubkey, not a PDA.

pub const PROTOCOL_CONFIG_SEED: &[u8] = b"protocol";
pub const POOL_SEED: &[u8] = b"pool";
pub const COLLECTION_SEED: &[u8] = b"collection";
pub const PRINCIPAL_VAULT_SEED: &[u8] = b"principal";
pub const TREASURY_04_SEED: &[u8] = b"t04";
pub const TREASURY_02_SEED: &[u8] = b"t02";
pub const POSITION_SEED: &[u8] = b"position";
pub const POSITION_VAULT_SEED: &[u8] = b"vault";
pub const WALLET_STATS_SEED: &[u8] = b"wallet";
pub const TOP_TIER_SEED: &[u8] = b"tier";
pub const BATCH_SEED: &[u8] = b"batch";

/// The VRF request identity, `PDA([VRF_IDENTITY_SEED], bye_machine)` — the account that signs
/// `commit_rolls`' request CPI. The byte-string is the **provider's** constant, not a choice of
/// ours: it derives the same address from this program's id and rejects a request signed by
/// anything else, so changing this value does not rename a namespace, it breaks every request.
pub const VRF_IDENTITY_SEED: &[u8] = b"identity";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::production;

    /// None of these constant *values* has a literal pin anywhere else in the crate —
    /// every accounts-struct pin in `instructions/mod.rs` checks the constant's
    /// *name* inside `seeds = [...]`, which is blind by construction to what that name expands
    /// to. Relocating `TREASURY_04_SEED` from `b"t04"` to `b"t4"` leaves every one of the 15
    /// accounts structs byte-identical and silently moves the treasury PDA namespace — the
    /// custody-relevant hole this test closes.
    ///
    /// **When this fails:** update the literal to match the constant's new value — never delete
    /// or weaken this assertion. A failure here is this instrument doing its job.
    #[test]
    fn every_seed_constant_is_pinned_to_its_exact_byte_value() {
        assert_eq!(PROTOCOL_CONFIG_SEED, b"protocol");
        assert_eq!(POOL_SEED, b"pool");
        assert_eq!(COLLECTION_SEED, b"collection");
        assert_eq!(PRINCIPAL_VAULT_SEED, b"principal");
        assert_eq!(TREASURY_04_SEED, b"t04");
        assert_eq!(TREASURY_02_SEED, b"t02");
        assert_eq!(POSITION_SEED, b"position");
        assert_eq!(POSITION_VAULT_SEED, b"vault");
        assert_eq!(WALLET_STATS_SEED, b"wallet");
        assert_eq!(TOP_TIER_SEED, b"tier");
        assert_eq!(BATCH_SEED, b"batch");
        assert_eq!(VRF_IDENTITY_SEED, b"identity");
    }

    /// Pins this module's own constant count — a 10th seed constant added without a matching
    /// assertion above fails this test rather than escaping the sweep.
    #[test]
    fn the_pinned_constants_cover_every_seed_declared_in_this_module() {
        let declared = production(include_str!("seeds.rs"))
            .lines()
            .filter(|line| line.trim_start().starts_with("pub const") && line.contains("_SEED"))
            .count();
        assert_eq!(
            declared, 12,
            "a seed constant was added or removed in this module"
        );
    }
}

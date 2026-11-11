//! The pinned $BYE/USDC swap venue: what makes an account eligible to be it, and the wire format
//! of the swap this program sends it.
//!
//! **This file encodes one specific program's layout.** The offsets, discriminators and enum
//! values below are Meteora DAMM v2's, read off its IDL rather than its documentation. Re-pointing
//! the protocol at a different AMM replaces this module wholesale rather than editing it, which is
//! why it is named for the venue instead of for the config field it validates.
//!
//! **Nothing here reads an `Account`.** The pool belongs to another program, so Anchor can neither
//! deserialize it nor check it; every read is a bounds-checked slice of raw account data at a
//! pinned offset. A read past the end is a returned error, never a panic — which is what keeps a
//! truncated or substituted account from aborting the transaction with nothing named.
//!
//! **An owner check alone is not eligibility.** It admits any account the venue program owns — a
//! position, a config, or a pool for an entirely different pair — and pinning one of those would
//! leave the blank settlement path permanently unable to pay. So identity is five conditions, and
//! the four immutable ones are asserted both where the pool is pinned and again wherever it is
//! read, because the account's *contents* can be substituted even when its address cannot.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};

use crate::common::errors::ByeMachineError;
use crate::common::swap_math::PairOrdering;

/// Meteora DAMM v2. A compile-time constant rather than config: which pool is pinned is a deploy
/// choice, which program owns it is not.
pub const CP_AMM_ID: Pubkey = pubkey!("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");

/// The venue's single global authority, the same address for every pool it hosts.
pub const POOL_AUTHORITY: Pubkey = pubkey!("HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC");

/// Anchor account discriminator for the venue's `Pool`.
pub const POOL_DISCRIMINATOR: [u8; 8] = [241, 154, 109, 4, 17, 177, 109, 188];

/// Anchor instruction discriminator for the venue's `swap`.
pub const SWAP_DISCRIMINATOR: [u8; 8] = [248, 198, 158, 145, 225, 117, 135, 200];

/// Seed prefix of a pool's token vaults: `["token_vault", mint, pool]` under the venue program.
pub const TOKEN_VAULT_SEED: &[u8] = b"token_vault";

/// `pool_status` value that permits swapping; any other value does not.
const POOL_STATUS_ENABLE: u8 = 0;

/// `activation_type` selecting which clock `activation_point` is denominated in.
const ACTIVATION_TYPE_SLOT: u8 = 0;

// Byte offsets into the pool account, discriminator included. Taken from the venue's IDL by
// summing declared field sizes; a layout change upstream moves them and the discriminator check
// alone would not notice, which is why `MIN_POOL_LEN` is asserted rather than assumed.
const OFF_TOKEN_A_MINT: usize = 168;
const OFF_TOKEN_B_MINT: usize = 200;
const OFF_TOKEN_A_VAULT: usize = 232;
const OFF_TOKEN_B_VAULT: usize = 264;
const OFF_SQRT_PRICE: usize = 456;
const OFF_ACTIVATION_POINT: usize = 472;
const OFF_ACTIVATION_TYPE: usize = 480;
const OFF_POOL_STATUS: usize = 481;

/// Total size of the venue's `Pool`, discriminator included.
const MIN_POOL_LEN: usize = 1112;

fn slice_at(data: &[u8], offset: usize, len: usize) -> Result<&[u8]> {
    data.get(
        offset
            ..offset
                .checked_add(len)
                .ok_or(ByeMachineError::SwapPoolInvalid)?,
    )
    .ok_or_else(|| ByeMachineError::SwapPoolInvalid.into())
}

fn read_pubkey(data: &[u8], offset: usize) -> Result<Pubkey> {
    let bytes: [u8; 32] = slice_at(data, offset, 32)?
        .try_into()
        .map_err(|_| ByeMachineError::SwapPoolInvalid)?;
    Ok(Pubkey::new_from_array(bytes))
}

fn read_u128(data: &[u8], offset: usize) -> Result<u128> {
    let bytes: [u8; 16] = slice_at(data, offset, 16)?
        .try_into()
        .map_err(|_| ByeMachineError::SwapPoolInvalid)?;
    Ok(u128::from_le_bytes(bytes))
}

fn read_u64(data: &[u8], offset: usize) -> Result<u64> {
    let bytes: [u8; 8] = slice_at(data, offset, 8)?
        .try_into()
        .map_err(|_| ByeMachineError::SwapPoolInvalid)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_u8(data: &[u8], offset: usize) -> Result<u8> {
    slice_at(data, offset, 1).map(|s| s[0])
}

/// The four immutable conditions, asserted together because each alone admits an account that
/// cannot serve a settlement.
///
/// Returns which side of the pair holds $BYE, since establishing that **is** the mint-set check —
/// deriving it a second time elsewhere would be a second chance to get the direction backwards.
///
/// The two vault derivations dominate this function's cost. They are `find_program_address`
/// rather than a stored bump because the bumps belong to another program's accounts and this
/// program has nowhere truthful to cache them.
pub fn assert_immutable_identity(
    pool_key: &Pubkey,
    owner: &Pubkey,
    data: &[u8],
    bye_mint: &Pubkey,
    usdc_mint: &Pubkey,
) -> Result<PairOrdering> {
    require_keys_eq!(*owner, CP_AMM_ID, ByeMachineError::SwapPoolInvalid);

    require!(data.len() >= MIN_POOL_LEN, ByeMachineError::SwapPoolInvalid);
    require!(
        slice_at(data, 0, 8)? == POOL_DISCRIMINATOR.as_slice(),
        ByeMachineError::SwapPoolInvalid
    );

    let token_a_mint = read_pubkey(data, OFF_TOKEN_A_MINT)?;
    let token_b_mint = read_pubkey(data, OFF_TOKEN_B_MINT)?;

    // The pair is a set, not a sequence: either arrangement is a valid pool, and which one it is
    // decides the direction the price identity is applied in.
    let ordering = if token_a_mint == *bye_mint && token_b_mint == *usdc_mint {
        PairOrdering::ByeIsA
    } else if token_a_mint == *usdc_mint && token_b_mint == *bye_mint {
        PairOrdering::ByeIsB
    } else {
        return Err(ByeMachineError::SwapPoolInvalid.into());
    };

    let token_a_vault = read_pubkey(data, OFF_TOKEN_A_VAULT)?;
    let token_b_vault = read_pubkey(data, OFF_TOKEN_B_VAULT)?;
    require_keys_eq!(
        token_a_vault,
        derive_token_vault(&token_a_mint, pool_key),
        ByeMachineError::SwapPoolInvalid
    );
    require_keys_eq!(
        token_b_vault,
        derive_token_vault(&token_b_mint, pool_key),
        ByeMachineError::SwapPoolInvalid
    );

    Ok(ordering)
}

/// A pool's token vault address for one side of its pair.
pub fn derive_token_vault(mint: &Pubkey, pool: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[TOKEN_VAULT_SEED, mint.as_ref(), pool.as_ref()],
        &CP_AMM_ID,
    )
    .0
}

/// The fifth condition, which unlike the other four is mutable: the venue may disable a pool, and
/// a pool that has not reached its activation point rejects every swap.
///
/// `activation_point` is denominated in slots or in unix seconds depending on the pool's own
/// `activation_type`, so the comparison reads the matching clock. Comparing against the wrong one
/// silently succeeds — slot and timestamp are both large monotonic integers — which is why the
/// selection is explicit rather than defaulted.
pub fn assert_swappable(data: &[u8], clock: &Clock) -> Result<()> {
    require!(
        read_u8(data, OFF_POOL_STATUS)? == POOL_STATUS_ENABLE,
        ByeMachineError::SwapPoolNotSwappable
    );

    let activation_point = read_u64(data, OFF_ACTIVATION_POINT)?;
    let now = if read_u8(data, OFF_ACTIVATION_TYPE)? == ACTIVATION_TYPE_SLOT {
        clock.slot
    } else {
        u64::try_from(clock.unix_timestamp).map_err(|_| ByeMachineError::SwapPoolNotSwappable)?
    };
    require!(
        now >= activation_point,
        ByeMachineError::SwapPoolNotSwappable
    );

    Ok(())
}

/// The two token vaults the pool records for itself, in `(token_a, token_b)` order.
///
/// Read back rather than re-derived at the call site: `assert_immutable_identity` has already
/// checked these are that pool's own derived vaults, so comparing a caller's accounts against
/// them is the cheaper half of the same guarantee.
pub fn token_vaults(data: &[u8]) -> Result<(Pubkey, Pubkey)> {
    Ok((
        read_pubkey(data, OFF_TOKEN_A_VAULT)?,
        read_pubkey(data, OFF_TOKEN_B_VAULT)?,
    ))
}

/// The pool's live price, Q64.64 over base units.
///
/// Read in the same instruction as the swap that consumes it — the bound this feeds covers the
/// venue's fee and the curve's impact, and a price read in an earlier instruction would have to
/// cover drift as well.
pub fn sqrt_price(data: &[u8]) -> Result<u128> {
    read_u128(data, OFF_SQRT_PRICE)
}

/// Instruction data for the venue's `swap`: its discriminator followed by the two amounts, both
/// little-endian, in declaration order.
///
/// Written out rather than derived from a local struct: the argument order is the venue's wire
/// contract, and two adjacent `u64`s swapped by a refactor would still serialize, still fit, and
/// silently send the minimum as the input amount.
pub fn swap_instruction_data(amount_in: u64, minimum_amount_out: u64) -> [u8; 24] {
    let mut data = [0u8; 24];
    data[0..8].copy_from_slice(&SWAP_DISCRIMINATOR);
    data[8..16].copy_from_slice(&amount_in.to_le_bytes());
    data[16..24].copy_from_slice(&minimum_amount_out.to_le_bytes());
    data
}

/// Seed of the venue's Anchor event-authority PDA.
pub const EVENT_AUTHORITY_SEED: &[u8] = b"__event_authority";

/// The venue's `swap`, in its declared account order.
///
/// Held as a struct rather than passed positionally because the order is the venue's wire
/// contract and two adjacent slots of the same type — the two vaults, the two mints, the two
/// token programs — transpose silently: the instruction still builds, still has fourteen
/// accounts, and fails inside a program that names none of this one's fields.
pub struct SwapCpi<'info> {
    pub pool_authority: AccountInfo<'info>,
    pub pool: AccountInfo<'info>,
    pub input_token_account: AccountInfo<'info>,
    pub output_token_account: AccountInfo<'info>,
    pub token_a_vault: AccountInfo<'info>,
    pub token_b_vault: AccountInfo<'info>,
    pub token_a_mint: AccountInfo<'info>,
    pub token_b_mint: AccountInfo<'info>,
    /// Authorizes the transfer out of `input_token_account`, so it must be that account's own
    /// token authority — here the pool PDA, which signs with its seeds.
    pub payer: AccountInfo<'info>,
    pub token_a_program: AccountInfo<'info>,
    pub token_b_program: AccountInfo<'info>,
    pub event_authority: AccountInfo<'info>,
    pub cp_amm_program: AccountInfo<'info>,
}

/// The addresses the venue's `swap` names, in its declared order.
pub struct SwapKeys {
    pub pool_authority: Pubkey,
    pub pool: Pubkey,
    pub input_token_account: Pubkey,
    pub output_token_account: Pubkey,
    pub token_a_vault: Pubkey,
    pub token_b_vault: Pubkey,
    pub token_a_mint: Pubkey,
    pub token_b_mint: Pubkey,
    pub payer: Pubkey,
    pub token_a_program: Pubkey,
    pub token_b_program: Pubkey,
    pub event_authority: Pubkey,
}

/// The account list the venue's `swap` expects, in order, with each slot's writable and signer
/// flags. Pure over addresses so the order can be asserted directly rather than inferred from a
/// transaction that failed.
pub(crate) fn swap_account_metas(keys: &SwapKeys) -> [AccountMeta; 14] {
    [
        AccountMeta::new_readonly(keys.pool_authority, false),
        AccountMeta::new(keys.pool, false),
        AccountMeta::new(keys.input_token_account, false),
        AccountMeta::new(keys.output_token_account, false),
        AccountMeta::new(keys.token_a_vault, false),
        AccountMeta::new(keys.token_b_vault, false),
        AccountMeta::new_readonly(keys.token_a_mint, false),
        AccountMeta::new_readonly(keys.token_b_mint, false),
        AccountMeta::new_readonly(keys.payer, true),
        AccountMeta::new_readonly(keys.token_a_program, false),
        AccountMeta::new_readonly(keys.token_b_program, false),
        // `referral_token_account`, unused. Anchor signals an absent optional account by putting
        // the callee's own program id in the slot, so it is filled rather than dropped —
        // dropping it would shift every account after it by one. Although a *present* referral
        // account is writable in the venue IDL, its program-id `None` sentinel must be readonly:
        // asking the CPI runtime to make the executable writable would escalate the readonly
        // `cp_amm_program` privilege carried by `settle_roll`.
        AccountMeta::new_readonly(CP_AMM_ID, false),
        AccountMeta::new_readonly(keys.event_authority, false),
        AccountMeta::new_readonly(CP_AMM_ID, false),
    ]
}

/// Exact-in swap of `amount_in` for at least `minimum_amount_out`.
///
/// **The bound is enforced by the venue, and a miss aborts the transaction.** A failed inner
/// instruction cannot be caught, so there is no branch here for a swap that comes up short — the
/// roll stays unsettled and is retried, which is a condition a later attempt can clear.
///
/// `referral_token_account` is the venue's one optional account and is not used. Anchor signals
/// an absent optional account by putting the callee's own program id in the slot, so the slot is
/// filled rather than dropped: removing it would shift every account after it by one.
pub fn swap_exact_in(
    accounts: &SwapCpi<'_>,
    amount_in: u64,
    minimum_amount_out: u64,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let metas = swap_account_metas(&SwapKeys {
        pool_authority: accounts.pool_authority.key(),
        pool: accounts.pool.key(),
        input_token_account: accounts.input_token_account.key(),
        output_token_account: accounts.output_token_account.key(),
        token_a_vault: accounts.token_a_vault.key(),
        token_b_vault: accounts.token_b_vault.key(),
        token_a_mint: accounts.token_a_mint.key(),
        token_b_mint: accounts.token_b_mint.key(),
        payer: accounts.payer.key(),
        token_a_program: accounts.token_a_program.key(),
        token_b_program: accounts.token_b_program.key(),
        event_authority: accounts.event_authority.key(),
    });

    // Order here is not the wire contract: the runtime matches these to the metas above by
    // key, so what matters is that every account the metas name is present.
    let infos = [
        accounts.pool_authority.clone(),
        accounts.pool.clone(),
        accounts.input_token_account.clone(),
        accounts.output_token_account.clone(),
        accounts.token_a_vault.clone(),
        accounts.token_b_vault.clone(),
        accounts.token_a_mint.clone(),
        accounts.token_b_mint.clone(),
        accounts.payer.clone(),
        accounts.token_a_program.clone(),
        accounts.token_b_program.clone(),
        accounts.cp_amm_program.clone(),
        accounts.event_authority.clone(),
        accounts.cp_amm_program.clone(),
    ];

    anchor_lang::solana_program::program::invoke_signed(
        &Instruction {
            program_id: CP_AMM_ID,
            accounts: metas.to_vec(),
            data: swap_instruction_data(amount_in, minimum_amount_out).to_vec(),
        },
        &infos,
        signer_seeds,
    )
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::swap_math::{in_pair_order, SQRT_PRICE_BYE_IS_A};

    const BYE: Pubkey = Pubkey::new_from_array([1u8; 32]);
    const USDC: Pubkey = Pubkey::new_from_array([2u8; 32]);
    const OTHER: Pubkey = Pubkey::new_from_array([3u8; 32]);

    /// A pool account whose vaults are the real PDAs for the mints it declares, so a test that
    /// means to break one condition does not accidentally break two.
    fn pool_bytes(pool: &Pubkey, mint_a: &Pubkey, mint_b: &Pubkey) -> Vec<u8> {
        let mut data = vec![0u8; MIN_POOL_LEN];
        data[0..8].copy_from_slice(&POOL_DISCRIMINATOR);
        data[OFF_TOKEN_A_MINT..OFF_TOKEN_A_MINT + 32].copy_from_slice(mint_a.as_ref());
        data[OFF_TOKEN_B_MINT..OFF_TOKEN_B_MINT + 32].copy_from_slice(mint_b.as_ref());
        let vault_a = derive_token_vault(mint_a, pool);
        let vault_b = derive_token_vault(mint_b, pool);
        data[OFF_TOKEN_A_VAULT..OFF_TOKEN_A_VAULT + 32].copy_from_slice(vault_a.as_ref());
        data[OFF_TOKEN_B_VAULT..OFF_TOKEN_B_VAULT + 32].copy_from_slice(vault_b.as_ref());
        data[OFF_POOL_STATUS] = POOL_STATUS_ENABLE;
        data[OFF_ACTIVATION_TYPE] = ACTIVATION_TYPE_SLOT;
        data
    }

    fn clock_at_slot(slot: u64) -> Clock {
        Clock {
            slot,
            epoch_start_timestamp: 0,
            epoch: 0,
            leader_schedule_epoch: 0,
            unix_timestamp: 1_700_000_000,
        }
    }

    #[test]
    fn a_well_formed_pool_passes_in_both_orderings() {
        let pool = Pubkey::new_unique();
        assert_eq!(
            assert_immutable_identity(
                &pool,
                &CP_AMM_ID,
                &pool_bytes(&pool, &BYE, &USDC),
                &BYE,
                &USDC
            )
            .unwrap(),
            PairOrdering::ByeIsA
        );
        assert_eq!(
            assert_immutable_identity(
                &pool,
                &CP_AMM_ID,
                &pool_bytes(&pool, &USDC, &BYE),
                &BYE,
                &USDC
            )
            .unwrap(),
            PairOrdering::ByeIsB
        );
    }

    /// The owner check is the one most likely to be mistaken for sufficient on its own. Each of
    /// the other three must independently refuse an account the owner check would admit.
    #[test]
    fn each_immutable_condition_refuses_on_its_own() {
        let pool = Pubkey::new_unique();
        let good = pool_bytes(&pool, &BYE, &USDC);

        // Owner.
        assert_eq!(
            assert_immutable_identity(&pool, &OTHER, &good, &BYE, &USDC).unwrap_err(),
            ByeMachineError::SwapPoolInvalid.into(),
            "an account owned by another program"
        );

        // Discriminator — a cp_amm-owned account that is not a Pool.
        let mut wrong_disc = good.clone();
        wrong_disc[0] ^= 0xFF;
        assert_eq!(
            assert_immutable_identity(&pool, &CP_AMM_ID, &wrong_disc, &BYE, &USDC).unwrap_err(),
            ByeMachineError::SwapPoolInvalid.into(),
            "a cp_amm account carrying a different discriminator"
        );

        // Mint pair — a real pool for a different pair.
        let wrong_pair = pool_bytes(&pool, &OTHER, &USDC);
        assert_eq!(
            assert_immutable_identity(&pool, &CP_AMM_ID, &wrong_pair, &BYE, &USDC).unwrap_err(),
            ByeMachineError::SwapPoolInvalid.into(),
            "a pool for a pair this protocol does not settle in"
        );

        // Vaults — mints and discriminator right, vaults belonging to a different pool. Both
        // sides are corrupted independently: a check written for one side only still passes a
        // case that breaks the other.
        for (offset, side) in [(OFF_TOKEN_A_VAULT, "a"), (OFF_TOKEN_B_VAULT, "b")] {
            let mut wrong_vault = good.clone();
            let foreign = derive_token_vault(&BYE, &Pubkey::new_unique());
            wrong_vault[offset..offset + 32].copy_from_slice(foreign.as_ref());
            assert_eq!(
                assert_immutable_identity(&pool, &CP_AMM_ID, &wrong_vault, &BYE, &USDC)
                    .unwrap_err(),
                ByeMachineError::SwapPoolInvalid.into(),
                "token_{side}_vault is not this pool's"
            );
        }
    }

    /// Both mints equal to $BYE satisfies "each mint is one of the two" while being no pair at
    /// all — the shape a membership test written per-mint would admit.
    #[test]
    fn a_pool_of_one_mint_twice_is_refused() {
        let pool = Pubkey::new_unique();
        let data = pool_bytes(&pool, &BYE, &BYE);
        assert_eq!(
            assert_immutable_identity(&pool, &CP_AMM_ID, &data, &BYE, &USDC).unwrap_err(),
            ByeMachineError::SwapPoolInvalid.into()
        );
    }

    /// A truncated account must be a returned error, not an index panic.
    #[test]
    fn a_short_account_is_refused_rather_than_panicking() {
        let pool = Pubkey::new_unique();
        let mut data = pool_bytes(&pool, &BYE, &USDC);
        data.truncate(MIN_POOL_LEN - 1);
        assert_eq!(
            assert_immutable_identity(&pool, &CP_AMM_ID, &data, &BYE, &USDC).unwrap_err(),
            ByeMachineError::SwapPoolInvalid.into()
        );
        assert_eq!(
            sqrt_price(&[]).unwrap_err(),
            ByeMachineError::SwapPoolInvalid.into()
        );
    }

    #[test]
    fn a_disabled_pool_is_not_swappable() {
        let pool = Pubkey::new_unique();
        let mut data = pool_bytes(&pool, &BYE, &USDC);
        data[OFF_POOL_STATUS] = 1;
        assert_eq!(
            assert_swappable(&data, &clock_at_slot(10)).unwrap_err(),
            ByeMachineError::SwapPoolNotSwappable.into()
        );
    }

    #[test]
    fn a_pool_before_its_activation_point_is_not_swappable() {
        let pool = Pubkey::new_unique();
        let mut data = pool_bytes(&pool, &BYE, &USDC);
        data[OFF_ACTIVATION_POINT..OFF_ACTIVATION_POINT + 8].copy_from_slice(&500u64.to_le_bytes());

        assert_eq!(
            assert_swappable(&data, &clock_at_slot(499)).unwrap_err(),
            ByeMachineError::SwapPoolNotSwappable.into()
        );
        assert!(
            assert_swappable(&data, &clock_at_slot(500)).is_ok(),
            "at the point"
        );
        assert!(
            assert_swappable(&data, &clock_at_slot(501)).is_ok(),
            "past it"
        );
    }

    /// Slot and unix time are both large monotonic integers, so reading the wrong clock passes
    /// silently in one direction. This pins that the type byte actually selects.
    #[test]
    fn the_activation_clock_is_selected_by_the_pools_own_type() {
        let pool = Pubkey::new_unique();
        let mut data = pool_bytes(&pool, &BYE, &USDC);
        // A point far above any plausible slot, but below the fixture's unix timestamp.
        data[OFF_ACTIVATION_POINT..OFF_ACTIVATION_POINT + 8]
            .copy_from_slice(&1_600_000_000u64.to_le_bytes());

        data[OFF_ACTIVATION_TYPE] = ACTIVATION_TYPE_SLOT;
        assert_eq!(
            assert_swappable(&data, &clock_at_slot(10)).unwrap_err(),
            ByeMachineError::SwapPoolNotSwappable.into(),
            "as slots, slot 10 has not reached the point"
        );

        data[OFF_ACTIVATION_TYPE] = 1;
        assert!(
            assert_swappable(&data, &clock_at_slot(10)).is_ok(),
            "as seconds, the fixture's timestamp is past the point"
        );
    }

    /// A live mainnet pool, as the vector that proves the seed derivation rather than restates
    /// it. The four checked conditions are all derived or compared against bytes this program
    /// does not produce, so a seed order or program id that is wrong in the same way in both the
    /// implementation and a synthetic fixture would pass every other test in this module.
    ///
    /// Values read from the account at `HXnzoANseBAVst92PDJU6LsgZbpFpGzVShcXsM2B7PMb`.
    #[test]
    fn the_vault_derivation_reproduces_a_live_pools_own_vaults() {
        let pool = pubkey!("HXnzoANseBAVst92PDJU6LsgZbpFpGzVShcXsM2B7PMb");
        for (mint, vault) in [
            (
                pubkey!("srfmZPrbDkk9ZYvA6a9PTnzcJ7zXBnundX1PF8FT1ma"),
                pubkey!("A7tGbp3ex4ssLgH4ge1kpdrRDjQnb9s42ZQAareNAhTU"),
            ),
            (
                pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
                pubkey!("Hp7EMxLYuy9Vht9oW6Vkzy1ukRc6PvcGMtSvTdmJFFuo"),
            ),
        ] {
            assert_eq!(derive_token_vault(&mint, &pool), vault, "mint {mint}");
        }
    }

    #[test]
    fn sqrt_price_reads_the_pinned_offset() {
        let pool = Pubkey::new_unique();
        let mut data = pool_bytes(&pool, &BYE, &USDC);
        let value = 58_333_726_687_135_160u128;
        data[OFF_SQRT_PRICE..OFF_SQRT_PRICE + 16].copy_from_slice(&value.to_le_bytes());
        assert_eq!(sqrt_price(&data).unwrap(), value);
    }

    /// Minimal base64 decoder, so the fixture can be read without adding a dependency the
    /// program itself would then carry.
    fn base64_decode(input: &str) -> Vec<u8> {
        const TABLE: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for byte in input
            .bytes()
            .filter(|b| !b.is_ascii_whitespace() && *b != b'=')
        {
            let value = TABLE
                .iter()
                .position(|c| *c == byte)
                .unwrap_or_else(|| panic!("not base64: {}", byte as char));
            acc = (acc << 6) | value as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        out
    }

    /// One crude field read out of the account-dump JSON, so the fixture can be parsed without a
    /// serde dependency in the program's own test build.
    fn json_string_field(source: &str, key: &str) -> String {
        let needle = format!("\"{key}\"");
        let after = source.split(&needle).nth(1).expect("key present");
        let body = after.split_once(':').expect("key has a value").1;
        body.trim_start()
            .trim_start_matches('[')
            .trim_start()
            .split('"')
            .nth(1)
            .expect("value is a string")
            .to_string()
    }

    /// **The integration fixture is checked against the production predicate, here.** The
    /// validator loads that file as the pinned pool, and every other test in this module builds
    /// its own bytes — so a fixture that drifted from the layout these functions read would boot
    /// a suite whose blank path fails for a reason no unit test names.
    #[test]
    fn the_committed_integration_fixture_satisfies_every_condition() {
        let source = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/swap/damm-v2-pool.json"
        ))
        .expect("fixture is committed");

        let pool: Pubkey = json_string_field(&source, "pubkey").parse().unwrap();
        let owner: Pubkey = json_string_field(&source, "owner").parse().unwrap();
        let data = base64_decode(&json_string_field(&source, "data"));

        let bye: Pubkey = json_string_field(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/swap/bye-mint.json"
            ))
            .unwrap(),
            "pubkey",
        )
        .parse()
        .unwrap();
        let usdc: Pubkey = json_string_field(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/swap/usdc-mint.json"
            ))
            .unwrap(),
            "pubkey",
        )
        .parse()
        .unwrap();

        assert_eq!(
            assert_immutable_identity(&pool, &owner, &data, &bye, &usdc).unwrap(),
            PairOrdering::ByeIsA,
            "the fixture is built with $BYE as token A"
        );
        assert_swappable(&data, &clock_at_slot(1)).expect("fixture must be swappable");
        assert_eq!(
            sqrt_price(&data).unwrap(),
            SQRT_PRICE_BYE_IS_A,
            "the fixture prices at the same point the unit vectors do"
        );
    }

    /// **Each mint slot must carry the mint that derives the vault in the matching slot.** That
    /// is the pairing the venue itself checks, and it is the whole reason the ordering is read
    /// rather than assumed: the vaults are pinned to the addresses the pool records, so on a
    /// USDC-first pool slot A holds the USDC vault — and naming the mints positionally would put
    /// the $BYE mint beside it. The venue rejects that, the settlement aborts, and the roll stays
    /// unsettled with the whole batch behind it. A rejected *pool* is loud; a rejected *swap* on
    /// a valid pool is a stall.
    ///
    /// Both layouts are walked, because a pool built the other way round is exactly what the
    /// eligibility check goes out of its way to admit.
    #[test]
    fn each_mint_slot_carries_the_mint_that_derives_its_vault() {
        let pool = Pubkey::new_unique();

        for (recorded_a_mint, recorded_b_mint) in [(BYE, USDC), (USDC, BYE)] {
            let data = pool_bytes(&pool, &recorded_a_mint, &recorded_b_mint);
            let ordering =
                assert_immutable_identity(&pool, &CP_AMM_ID, &data, &BYE, &USDC).unwrap();

            let (slot_a_mint, slot_b_mint) = in_pair_order(ordering, BYE, USDC);
            let (slot_a_vault, slot_b_vault) = token_vaults(&data).unwrap();

            assert_eq!(
                derive_token_vault(&slot_a_mint, &pool),
                slot_a_vault,
                "slot A's mint does not derive slot A's vault"
            );
            assert_eq!(
                derive_token_vault(&slot_b_mint, &pool),
                slot_b_vault,
                "slot B's mint does not derive slot B's vault"
            );
            assert_eq!(
                slot_a_mint, recorded_a_mint,
                "slot A is not the pool's token A"
            );
            assert_eq!(
                slot_b_mint, recorded_b_mint,
                "slot B is not the pool's token B"
            );
        }
    }

    /// The reordering is not a no-op, and it is not an unconditional swap either.
    #[test]
    fn the_pair_order_follows_the_ordering_it_is_given() {
        assert_eq!(
            in_pair_order(PairOrdering::ByeIsA, "bye", "usdc"),
            ("bye", "usdc")
        );
        assert_eq!(
            in_pair_order(PairOrdering::ByeIsB, "bye", "usdc"),
            ("usdc", "bye")
        );
    }

    /// The venue's account order, asserted slot by slot against the IDL it was read from.
    ///
    /// The absent optional referral is the one deliberate privilege exception: its address is
    /// the venue program's `None` sentinel, not a real referral token account, and therefore must
    /// stay readonly across the CPI boundary.
    ///
    /// Every address is distinct, so a transposition shows up as a wrong address rather than as
    /// two identical ones — the two vaults, the two mints and the two token programs are each a
    /// same-typed adjacent pair, and swapping either pair still produces a fourteen-account
    /// instruction that builds and fails somewhere else.
    #[test]
    fn the_swap_account_order_is_the_venues_own() {
        let k = SwapKeys {
            pool_authority: POOL_AUTHORITY,
            pool: Pubkey::new_from_array([10u8; 32]),
            input_token_account: Pubkey::new_from_array([11u8; 32]),
            output_token_account: Pubkey::new_from_array([12u8; 32]),
            token_a_vault: Pubkey::new_from_array([13u8; 32]),
            token_b_vault: Pubkey::new_from_array([14u8; 32]),
            token_a_mint: Pubkey::new_from_array([15u8; 32]),
            token_b_mint: Pubkey::new_from_array([16u8; 32]),
            payer: Pubkey::new_from_array([17u8; 32]),
            token_a_program: Pubkey::new_from_array([18u8; 32]),
            token_b_program: Pubkey::new_from_array([19u8; 32]),
            event_authority: Pubkey::new_from_array([20u8; 32]),
        };
        let metas = swap_account_metas(&k);

        let expected: [(Pubkey, bool, bool); 14] = [
            (POOL_AUTHORITY, false, false),
            (k.pool, true, false),
            (k.input_token_account, true, false),
            (k.output_token_account, true, false),
            (k.token_a_vault, true, false),
            (k.token_b_vault, true, false),
            (k.token_a_mint, false, false),
            (k.token_b_mint, false, false),
            (k.payer, false, true),
            (k.token_a_program, false, false),
            (k.token_b_program, false, false),
            (CP_AMM_ID, false, false),
            (k.event_authority, false, false),
            (CP_AMM_ID, false, false),
        ];
        for (i, (key, writable, signer)) in expected.into_iter().enumerate() {
            assert_eq!(metas[i].pubkey, key, "slot {i} address");
            assert_eq!(metas[i].is_writable, writable, "slot {i} writable");
            assert_eq!(metas[i].is_signer, signer, "slot {i} signer");
        }
    }

    /// The only account that signs is the one authorizing the transfer out of the input account.
    #[test]
    fn exactly_one_slot_signs_and_it_is_the_payer() {
        let payer = Pubkey::new_from_array([17u8; 32]);
        let k = SwapKeys {
            pool_authority: POOL_AUTHORITY,
            pool: Pubkey::new_unique(),
            input_token_account: Pubkey::new_unique(),
            output_token_account: Pubkey::new_unique(),
            token_a_vault: Pubkey::new_unique(),
            token_b_vault: Pubkey::new_unique(),
            token_a_mint: Pubkey::new_unique(),
            token_b_mint: Pubkey::new_unique(),
            payer,
            token_a_program: Pubkey::new_unique(),
            token_b_program: Pubkey::new_unique(),
            event_authority: Pubkey::new_unique(),
        };
        let metas = swap_account_metas(&k);
        let signers: Vec<Pubkey> = metas
            .iter()
            .filter(|m| m.is_signer)
            .map(|m| m.pubkey)
            .collect();
        assert_eq!(signers, vec![payer]);
    }

    /// The event-authority address the venue derives for itself.
    #[test]
    fn the_event_authority_is_the_venues_own_pda() {
        let (derived, _) = Pubkey::find_program_address(&[EVENT_AUTHORITY_SEED], &CP_AMM_ID);
        assert_eq!(EVENT_AUTHORITY_SEED, b"__event_authority");
        assert_ne!(derived, CP_AMM_ID);
    }

    /// The venue's wire contract: discriminator, then `amount_in`, then `minimum_amount_out`.
    /// Asserted as whole bytes — a test naming only the two amounts passes with them transposed.
    #[test]
    fn swap_data_is_the_discriminator_then_input_then_minimum() {
        let data = swap_instruction_data(5_000_000, 494_999_999_999);
        let mut expected = Vec::new();
        expected.extend_from_slice(&[248, 198, 158, 145, 225, 117, 135, 200]);
        expected.extend_from_slice(&5_000_000u64.to_le_bytes());
        expected.extend_from_slice(&494_999_999_999u64.to_le_bytes());
        assert_eq!(data.as_slice(), expected.as_slice());

        let transposed = swap_instruction_data(494_999_999_999, 5_000_000);
        assert_ne!(
            data, transposed,
            "the two amounts must not be interchangeable in the encoding"
        );
    }
}

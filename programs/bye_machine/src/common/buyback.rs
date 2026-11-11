//! The buyback's non-custody logic — the half `instant_sell` and `instant_sell_core` do
//! identically, and the half `activate_acquired` performs alone when a batch deferred them.
//!
//! Two entry points, one subject. `record_acquisition` opens the treasury's position and, when
//! no batch is open, activates it on the spot; `activate_acquired_position` activates the one a
//! batch left `Pending`, on the attestation stored at acceptance. They sit together because the
//! second exists only to finish what the first deferred, and because both reach the pool's draw
//! domain through the same shared activation.
//!
//! `instant_sell_core` is `instant_sell` differing only in the custody leg. There is one
//! implementation of the shared half, so the two handlers cannot disagree about it. That half
//! includes choosing who pays — the loader's escrow or Treasury 03 — and both payment legs, in
//! `pay_for_sale`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::program_error::ProgramError;
use anchor_spl::token::{transfer, TokenAccount, Transfer};

use crate::common::activation::{
    activate_position, Activation, ActivationAccounts, ActivationTerms,
};
use crate::common::errors::ByeMachineError;
use crate::common::events::BuybackPayer;
use crate::common::guards::{
    assert_acquired_position, assert_weight_open, fits_wallet_cap, validate_admission_limits,
};
use crate::common::seeds::{
    ESCROW_SEED, ESCROW_VAULT_SEED, PROTOCOL_CONFIG_SEED, WALLET_STATS_SEED,
};
use crate::state::{
    Escrow, Pool, Position, PositionState, ProtocolConfig, TopTier, WalletStats, WeightIndex,
};

/// `payout = buyback_rate_bps × value / 10_000`, floor.
///
/// **Read live from the pool on every call and never snapshotted**: the rate in
/// force at acceptance is the one that applies, and a rate change between a card being drawn and
/// its sale applies to the sale. The `u128` widening is not decorative — `value` is micro-USDC and
/// `rate_bps` reaches 10_000, so the product leaves `u64` for values above ~1.8e15.
///
/// Truncating division floors the payout, which rounds **towards the treasury**. That direction is
/// the safe one for a solvency check that compares this figure against a balance: a payout rounded
/// up could pass the check and fail the transfer.
pub fn payout_for(buyback_rate_bps: u16, value: u64) -> Result<u64> {
    u128::from(value)
        .checked_mul(u128::from(buyback_rate_bps))
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .checked_div(10_000)
        .ok_or(ByeMachineError::ArithmeticFailure)?
        .try_into()
        .map_err(|_| ByeMachineError::ArithmeticFailure.into())
}

/// The three loader accounts as the sale received them. The Operator names the loader from the
/// off-chain record of the card's latest draw; the program checks only that the three belong to
/// one owner, never that this owner loaded the card.
pub struct LoaderAccounts<'a, 'info> {
    pub escrow: Option<&'a AccountInfo<'info>>,
    pub escrow_vault: Option<&'a AccountInfo<'info>>,
    pub wallet_stats: Option<&'a AccountInfo<'info>>,
}

/// A loader whose escrow exists, with its vault and its rollup in this pool bound to the escrow's
/// owner. `wallet_stats` is a deserialized copy; `store_loader_rollup` writes it back.
pub struct Loader<'info> {
    pub owner: Pubkey,
    pub armed: bool,
    pub vault_balance: u64,
    pub wallet_stats: WalletStats,
    escrow_bump: u8,
    escrow: AccountInfo<'info>,
    escrow_vault: AccountInfo<'info>,
    wallet_stats_account: AccountInfo<'info>,
}

/// Binds the loader accounts, stopping at the first rule that decides:
///
/// 1. None passed: no escrow path.
/// 2. A partial set: `EscrowAccountsMismatch`.
/// 3. `escrow` holds no data: a loader who never opened an escrow, so no escrow path, and the
///    other two are not read. Nothing binds an empty account's address, and nothing needs to —
///    it buys nothing, exactly as passing no accounts buys nothing.
/// 4. Otherwise `escrow` must be a program-owned `Escrow` at `[ESCROW_SEED, owner]`, the vault the
///    token account at `[ESCROW_VAULT_SEED, escrow]`, and the rollup a program-owned
///    `WalletStats` at `[WALLET_STATS_SEED, pool, owner]`, or `EscrowAccountsMismatch`.
///
/// Every address is re-derived from the bump the account stores, which the instruction that
/// created it recorded from Anchor's canonical derivation.
pub fn bind_loader<'info>(
    pool_key: &Pubkey,
    accounts: LoaderAccounts<'_, 'info>,
) -> Result<Option<Loader<'info>>> {
    let (escrow, escrow_vault, wallet_stats) = match (
        accounts.escrow,
        accounts.escrow_vault,
        accounts.wallet_stats,
    ) {
        (None, None, None) => return Ok(None),
        (Some(escrow), Some(vault), Some(stats)) => (escrow, vault, stats),
        _ => return err!(ByeMachineError::EscrowAccountsMismatch),
    };
    if escrow.data_is_empty() {
        return Ok(None);
    }

    let mismatch = || error!(ByeMachineError::EscrowAccountsMismatch);

    require_keys_eq!(
        *escrow.owner,
        crate::ID,
        ByeMachineError::EscrowAccountsMismatch
    );
    let state =
        Escrow::try_deserialize(&mut &escrow.try_borrow_data()?[..]).map_err(|_| mismatch())?;
    let escrow_address = Pubkey::create_program_address(
        &[ESCROW_SEED, state.owner.as_ref(), &[state.bump]],
        &crate::ID,
    )
    .map_err(|_| mismatch())?;
    require_keys_eq!(
        escrow_address,
        escrow.key(),
        ByeMachineError::EscrowAccountsMismatch
    );

    let vault_address = Pubkey::create_program_address(
        &[ESCROW_VAULT_SEED, escrow.key.as_ref(), &[state.vault_bump]],
        &crate::ID,
    )
    .map_err(|_| mismatch())?;
    require_keys_eq!(
        vault_address,
        escrow_vault.key(),
        ByeMachineError::EscrowAccountsMismatch
    );
    require_keys_eq!(
        *escrow_vault.owner,
        anchor_spl::token::ID,
        ByeMachineError::EscrowAccountsMismatch
    );
    let vault_balance = TokenAccount::try_deserialize(&mut &escrow_vault.try_borrow_data()?[..])
        .map_err(|_| mismatch())?
        .amount;

    require_keys_eq!(
        *wallet_stats.owner,
        crate::ID,
        ByeMachineError::EscrowAccountsMismatch
    );
    let stats = WalletStats::try_deserialize(&mut &wallet_stats.try_borrow_data()?[..])
        .map_err(|_| mismatch())?;
    let stats_address = Pubkey::create_program_address(
        &[
            WALLET_STATS_SEED,
            pool_key.as_ref(),
            state.owner.as_ref(),
            &[stats.bump],
        ],
        &crate::ID,
    )
    .map_err(|_| mismatch())?;
    require_keys_eq!(
        stats_address,
        wallet_stats.key(),
        ByeMachineError::EscrowAccountsMismatch
    );

    Ok(Some(Loader {
        owner: state.owner,
        armed: state.armed,
        vault_balance,
        wallet_stats: stats,
        escrow_bump: state.bump,
        escrow: escrow.clone(),
        escrow_vault: escrow_vault.clone(),
        wallet_stats_account: wallet_stats.clone(),
    }))
}

/// What the loader's escrow pays for this sale, or `None` when it does not qualify — which is
/// never an error: the sale then falls to Treasury 03. It qualifies when the pool has a bid, the
/// escrow is armed, its vault covers `bid × value` and the loader's own rollup fits the value
/// under the pool's wallet cap.
pub fn escrow_payment(pool: &Pool, value: u64, loader: Option<&Loader>) -> Result<Option<u64>> {
    let Some(loader) = loader else {
        return Ok(None);
    };
    if pool.escrow_bid_bps == 0 || !loader.armed {
        return Ok(None);
    }
    let escrow_paid = payout_for(pool.escrow_bid_bps, value)?;
    if escrow_paid > loader.vault_balance
        || !fits_wallet_cap(pool, value, loader.wallet_stats.active_value)?
    {
        return Ok(None);
    }
    Ok(Some(escrow_paid))
}

/// The accounts the payment reads and moves USDC between.
pub struct SaleAccounts<'a, 'info> {
    pub pool: &'a Account<'info, Pool>,
    /// Treasury 03's authority, which signs its payout.
    pub protocol_config: &'a Account<'info, ProtocolConfig>,
    pub treasury_03: &'a Account<'info, TokenAccount>,
    /// The Treasury 02 rollup. Its cap is the one a Treasury 03 sale is read against.
    pub treasury_02_stats: &'a Account<'info, WalletStats>,
    pub seller_usdc: AccountInfo<'info>,
    pub token_program: AccountInfo<'info>,
    pub loader: LoaderAccounts<'a, 'info>,
}

/// A paid sale, for the acquisition and the event.
pub struct Sale<'info> {
    pub payer: BuybackPayer,
    /// The winner offer the seller was paid at, and the seller's amount, on either path.
    pub rate_bps: u16,
    pub paid: u64,
    /// The bid in force and the loader's debit on an escrow sale; zero on a Treasury 03 one.
    pub bid_bps: u16,
    pub escrow_paid: u64,
    /// The loader that paid, on an escrow sale only.
    pub loader: Option<Loader<'info>>,
}

/// Chooses the payer and pays the seller. Runs after the sale's own checks, so a sale refused for
/// its own reasons never reaches either treasury.
///
/// **The seller's amount is `paid = rate × value` on both paths**, from one `payout_for`. On the
/// escrow path the vault pays `bid × value`: `paid` to the seller and the spread to Treasury 03,
/// both signed by the `Escrow` PDA. The spread is never negative because `update_config` keeps the
/// bid at or above the rate whenever the bid is set.
///
/// On the Treasury 03 path the Treasury 02 rollup's cap and Treasury 03's balance are checked and
/// either refuses the sale with its own code; an escrow that does not qualify never does.
pub fn pay_for_sale<'info>(accounts: SaleAccounts<'_, 'info>, value: u64) -> Result<Sale<'info>> {
    let SaleAccounts {
        pool,
        protocol_config,
        treasury_03,
        treasury_02_stats,
        seller_usdc,
        token_program,
        loader,
    } = accounts;

    // Read live off the pool, never snapshotted.
    let rate_bps = pool.buyback_rate_bps;
    let paid = payout_for(rate_bps, value)?;

    let loader = bind_loader(&pool.key(), loader)?;
    if let Some(loader) = &loader {
        // Unreachable while the Treasury 02 identity has no key to open an escrow with, and
        // checked anyway: one account in both roles would have its loader write overwritten by
        // Anchor's own write-back of the Treasury 02 rollup.
        require_keys_neq!(
            loader.wallet_stats_account.key(),
            treasury_02_stats.key(),
            ByeMachineError::EscrowAccountsMismatch
        );
    }

    if let Some(escrow_paid) = escrow_payment(pool, value, loader.as_ref())? {
        let loader = loader.expect("an escrow payment implies a bound loader");
        let spread = escrow_paid
            .checked_sub(paid)
            .ok_or(ByeMachineError::ArithmeticFailure)?;
        let escrow_seeds: &[&[u8]] = &[ESCROW_SEED, loader.owner.as_ref(), &[loader.escrow_bump]];
        let escrow_signer: &[&[&[u8]]] = &[escrow_seeds];

        transfer(
            CpiContext::new_with_signer(
                token_program.clone(),
                Transfer {
                    from: loader.escrow_vault.clone(),
                    to: seller_usdc,
                    authority: loader.escrow.clone(),
                },
                escrow_signer,
            ),
            paid,
        )?;
        if spread > 0 {
            transfer(
                CpiContext::new_with_signer(
                    token_program,
                    Transfer {
                        from: loader.escrow_vault.clone(),
                        to: treasury_03.to_account_info(),
                        authority: loader.escrow.clone(),
                    },
                    escrow_signer,
                ),
                spread,
            )?;
        }

        return Ok(Sale {
            payer: BuybackPayer::Escrow,
            rate_bps,
            paid,
            bid_bps: pool.escrow_bid_bps,
            escrow_paid,
            loader: Some(loader),
        });
    }

    // The cap reads the Treasury 02 rollup, so it bounds treasury inventory in aggregate.
    require!(
        fits_wallet_cap(pool, value, treasury_02_stats.active_value)?,
        ByeMachineError::WalletValueCapExceeded
    );
    // Checked here rather than left to the token program, whose own insufficient-funds error
    // says nothing a client can route on.
    require!(treasury_03.amount >= paid, ByeMachineError::T03Shortfall);

    // Treasury 03 is `PDA(["t03"])` with the `ProtocolConfig` PDA as its authority, so the payout
    // is a program-validated transfer rather than an authority-authorized one: no outside key can
    // move the account, and the amount and destination are both program-computed.
    let config_seeds: &[&[u8]] = &[PROTOCOL_CONFIG_SEED, &[protocol_config.bump]];
    let config_signer: &[&[&[u8]]] = &[config_seeds];

    transfer(
        CpiContext::new_with_signer(
            token_program,
            Transfer {
                from: treasury_03.to_account_info(),
                to: seller_usdc,
                authority: protocol_config.to_account_info(),
            },
            config_signer,
        ),
        paid,
    )?;

    Ok(Sale {
        payer: BuybackPayer::Treasury03,
        rate_bps,
        paid,
        bid_bps: 0,
        escrow_paid: 0,
        loader: None,
    })
}

impl<'info> Sale<'info> {
    /// The loader on an escrow sale, `Pubkey::default()` on a Treasury 03 one.
    pub fn escrow_owner(&self) -> Pubkey {
        self.loader
            .as_ref()
            .map(|loader| loader.owner)
            .unwrap_or_default()
    }

    /// Who the new position belongs to, and the rollup the acquisition credits: the loader's on
    /// an escrow sale, otherwise the Treasury 02 identity's. On an escrow sale the Treasury 02
    /// rollup, which the sale creates when absent, only has its identity stated; its counters
    /// are not touched.
    pub fn acquirer<'s>(
        &'s mut self,
        pool_key: Pubkey,
        t02_identity: Pubkey,
        treasury_02_stats: &'s mut WalletStats,
    ) -> (Pubkey, &'s mut WalletStats) {
        match self.loader.as_mut() {
            Some(loader) => {
                treasury_02_stats.pool = pool_key;
                treasury_02_stats.owner = t02_identity;
                (loader.owner, &mut loader.wallet_stats)
            }
            None => (t02_identity, treasury_02_stats),
        }
    }

    /// Writes the loader's rollup back to its account. Anchor writes back only the accounts it
    /// deserialized, and this one it did not.
    pub fn store_loader_rollup(&self) -> Result<()> {
        if let Some(loader) = &self.loader {
            let mut data = loader.wallet_stats_account.try_borrow_mut_data()?;
            loader.wallet_stats.try_serialize(&mut &mut data[..])?;
        }
        Ok(())
    }
}

/// The accounts the acquisition writes. Same shape and same reasoning as `ActivationAccounts`:
/// several `&mut` accounts off one `ctx.accounts`, passed by name so no two can be transposed.
pub struct AcquisitionAccounts<'a, 'info> {
    pub pool: &'a mut Pool,
    pub weight_index: &'a mut WeightIndex,
    pub top_tier: &'a mut TopTier,
    pub position: &'a mut Position,
    pub position_key: Pubkey,
    pub wallet_stats: &'a mut WalletStats,
    pub remaining_accounts: &'info [AccountInfo<'info>],
}

/// What the acquisition records. `standard` is the discriminant the custody leg already resolved
/// and is written, not re-derived — the two handlers reach this from different branches and the
/// asset was already transferred by the time they do.
pub struct AcquisitionTerms {
    pub pool_key: Pubkey,
    /// Who owns the new position: the Treasury 02 identity after a Treasury 03 sale, the loader
    /// after an escrow sale. The rollup in `AcquisitionAccounts` must be this owner's.
    pub depositor: Pubkey,
    pub nft_mint: Pubkey,
    pub standard: u8,
    pub value: u64,
    pub observed_at: i64,
    pub now: i64,
}

/// The recorded acquisition, for the caller's events.
pub struct Acquisition {
    pub position_id: u64,
    /// `Some` on the immediate-activation path only. `None` is a `Pending` position, and the
    /// caller must emit neither `RebalanceEvaluated` nor `TierChanged` for it — no weight moved.
    pub activation: Option<Activation>,
}

/// Create the new owner's position for an accepted sell and, when no batch is open, activate it.
///
/// **The `Pending`/`Active` branch lives here.** Paying the seller and moving the card change no
/// position's weight; only entering the draw domain does. So the sell itself is never gated on
/// `open_batches` — the position it creates follows the ordinary admission rule instead:
/// `Active` when no batch is in flight, `Pending` when one is, activating later through
/// `activate_acquired`.
///
/// **The attested `value` and `observed_at` are persisted on both branches.** That is load-bearing
/// rather than tidy: `activate_acquired` reads them back instead of re-attesting, because routing
/// a treasury position through `approve_deposit` would take a fresh observation and could activate
/// the card at a weight Treasury 03 never paid `rate × V` against.
pub fn record_acquisition(
    accounts: AcquisitionAccounts<'_, '_>,
    terms: AcquisitionTerms,
) -> Result<Acquisition> {
    let AcquisitionAccounts {
        pool,
        weight_index,
        top_tier,
        position,
        position_key,
        wallet_stats,
        remaining_accounts,
    } = accounts;
    let AcquisitionTerms {
        pool_key,
        depositor,
        nft_mint,
        standard,
        value,
        observed_at,
        now,
    } = terms;

    let position_id = pool.position_counter;
    pool.position_counter = position_id
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;

    position.pool = pool_key;
    position.depositor = depositor;
    position.nft_mint = nft_mint;
    position.position_id = position_id;
    position.state = PositionState::Pending;
    position.recorded_value = value;
    position.value_observed_at = observed_at;
    position.deposit_value = 0;
    position.slot_index = 0;
    position.activated_at = 0;
    position.equal_checkpoint = 0;
    position.accrued = 0;
    position.in_tier = false;
    position.tier_checkpoint = 0;
    position.lock_until = 0;
    position.reject_reason = 0;
    position.standard = standard;

    // The owner's rollup. The Treasury 02 one is created by the sale because nothing else ever
    // does — the treasury PDA never calls `deposit`. Written on every call and never
    // conditionally, so a second acquisition finding the account already present re-states its
    // identity and leaves the two counters alone; `init_if_needed` is exactly where a counter gets
    // silently reset instead.
    wallet_stats.pool = pool_key;
    wallet_stats.owner = depositor;

    let activation = if pool.open_batches == 0 {
        Some(activate_position(
            ActivationAccounts {
                pool,
                weight_index,
                top_tier,
                position,
                position_key,
                wallet_stats,
                remaining_accounts,
            },
            ActivationTerms {
                value,
                observed_at,
                // A treasury-acquired position earns no Season-0 score, so it carries no lock and
                // activates at 0. Not an argument on either handler:
                // there is no caller who may choose otherwise.
                lock_until: 0,
                now,
            },
        )?)
    } else {
        None
    };

    Ok(Acquisition {
        position_id,
        activation,
    })
}

/// What the delayed activation read back off the position, for the caller's event. The two
/// fields are returned rather than re-read by the handler because they are the instruction's
/// whole subject — a caller that fetched them itself could fetch different ones.
pub struct AcquiredActivation {
    pub value: u64,
    pub observed_at: i64,
    pub activation: Activation,
}

/// Activate a treasury position the sell left `Pending`, on the attestation it was bought at.
///
/// **The read-back is the point.** `value` and `observed_at` come off the position, never from a
/// caller: the sell attested them and Treasury 03 paid `rate × V` against that figure, so a fresh
/// observation here would activate the card at a weight the treasury never paid for.
///
/// **The limits are re-checked rather than trusted from acceptance.** All three are pool
/// configuration and can be introduced or tightened while the position waits, and the wallet's
/// own active value moves with every other acquisition — admissible when bought is not
/// admissible when activated, and activation is what puts the value into the pool.
pub fn activate_acquired_position(
    accounts: ActivationAccounts<'_, '_>,
    now: i64,
) -> Result<AcquiredActivation> {
    let ActivationAccounts {
        pool,
        weight_index,
        top_tier,
        position,
        position_key,
        wallet_stats,
        remaining_accounts,
    } = accounts;

    require!(
        position.state == PositionState::Pending,
        ByeMachineError::PositionNotPending
    );
    // The opposite polarity of the check `approve_deposit` and `reject_deposit` carry. Without
    // theirs a treasury position gets re-attested; without this one an ordinary depositor's
    // position activates on a stored value nobody attested at activation time, skipping the
    // attestation their path exists to consume.
    assert_acquired_position(position.recorded_value)?;
    assert_weight_open(pool)?;

    let value = position.recorded_value;
    let observed_at = position.value_observed_at;

    validate_admission_limits(pool, value, wallet_stats.active_value)?;

    let activation = activate_position(
        ActivationAccounts {
            pool,
            weight_index,
            top_tier,
            position,
            position_key,
            wallet_stats,
            remaining_accounts,
        },
        ActivationTerms {
            value,
            observed_at,
            // A treasury-acquired position earns no Season-0 score, so it carries no lock. A
            // literal, not an argument: there is no caller who may choose otherwise.
            lock_until: 0,
            now,
        },
    )?;

    Ok(AcquiredActivation {
        value,
        observed_at,
        activation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::constants::{
        DEFAULT_ADMISSION_FLOOR, DEFAULT_BLANK_FACES, DEFAULT_TICKET_TARGET,
    };
    use crate::common::math::weight_of;
    use crate::common::seeds::TREASURY_02_SEED;
    use crate::state::TierEntry;

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    /// The refusal's code, or a panic naming the activation that should not have happened.
    /// `unwrap_err` would need `AcquiredActivation: Debug`, which a production type should not
    /// derive only for tests.
    fn refusal(result: Result<AcquiredActivation>) -> u32 {
        match result {
            Ok(_) => panic!("expected a refusal, but the position activated"),
            Err(err) => error_code(err),
        }
    }

    fn boxed_zeroed_weight_index() -> Box<WeightIndex> {
        unsafe {
            let layout = std::alloc::Layout::new::<WeightIndex>();
            let ptr = std::alloc::alloc_zeroed(layout).cast::<WeightIndex>();
            assert!(!ptr.is_null(), "allocation failed");
            Box::from_raw(ptr)
        }
    }

    fn configured_pool(open_batches: u32) -> Pool {
        let mut pool = Pool::blank();
        pool.tier_size = 20;
        pool.blank_faces = DEFAULT_BLANK_FACES;
        pool.ticket_target = DEFAULT_TICKET_TARGET;
        pool.open_batches = open_batches;
        pool.position_counter = 41;
        pool
    }

    fn empty_top_tier() -> TopTier {
        TopTier {
            pool: Pubkey::default(),
            len: 0,
            entries: [TierEntry {
                position: Pubkey::default(),
                value: 0,
                activated_at: 0,
                position_id: 0,
            }; 20],
            acc_tier: 0,
            bump: 0,
        }
    }

    fn zeroed_position() -> Position {
        Position {
            pool: Pubkey::default(),
            depositor: Pubkey::default(),
            nft_mint: Pubkey::default(),
            position_id: 0,
            state: PositionState::Pending,
            recorded_value: 0,
            value_observed_at: 0,
            deposit_value: 0,
            slot_index: 0,
            activated_at: 0,
            equal_checkpoint: 0,
            accrued: 0,
            in_tier: false,
            tier_checkpoint: 0,
            lock_until: 0,
            reject_reason: 0,
            standard: 0,
            bump: 0,
            vault_bump: 0,
        }
    }

    fn wallet_stats(active_positions: u32, active_value: u64) -> WalletStats {
        WalletStats {
            pool: Pubkey::default(),
            owner: Pubkey::default(),
            active_positions,
            active_value,
            bump: 7,
        }
    }

    fn terms(depositor: Pubkey, value: u64) -> AcquisitionTerms {
        AcquisitionTerms {
            pool_key: Pubkey::new_unique(),
            depositor,
            nft_mint: Pubkey::new_unique(),
            standard: 1,
            value,
            observed_at: 555,
            now: 999,
        }
    }

    /// One acquisition against a caller-owned index and tier, so a test driving two in a row
    /// keeps them in step with `pool.w_real`; a fresh index under an accumulating pool would
    /// raise a spurious `WeightDesync`.
    fn acquire_with(
        pool: &mut Pool,
        weight_index: &mut WeightIndex,
        top_tier: &mut TopTier,
        position: &mut Position,
        stats: &mut WalletStats,
        terms: AcquisitionTerms,
    ) -> Result<Acquisition> {
        record_acquisition(
            AcquisitionAccounts {
                pool,
                weight_index,
                top_tier,
                position,
                position_key: Pubkey::new_unique(),
                wallet_stats: stats,
                remaining_accounts: &[],
            },
            terms,
        )
    }

    fn acquire(
        pool: &mut Pool,
        position: &mut Position,
        stats: &mut WalletStats,
        terms: AcquisitionTerms,
    ) -> Result<Acquisition> {
        let mut weight_index = boxed_zeroed_weight_index();
        let mut top_tier = empty_top_tier();
        acquire_with(
            pool,
            &mut weight_index,
            &mut top_tier,
            position,
            stats,
            terms,
        )
    }

    // --- the rate math ------------------------------------------------------------

    #[test]
    fn pays_the_rate_in_force_applied_to_the_value() {
        assert_eq!(payout_for(8_500, 1_000_000).unwrap(), 850_000);
    }

    /// The live-rate rule as arithmetic: nothing here reads a stored rate, so two different rates
    /// against one value give two different payouts. A hard-coded 85% would make these equal.
    #[test]
    fn a_different_rate_pays_a_different_amount_for_the_same_value() {
        let value = 1_000_000;
        assert_ne!(
            payout_for(8_500, value).unwrap(),
            payout_for(9_000, value).unwrap()
        );
    }

    #[test]
    fn a_zero_rate_pays_nothing_and_a_full_rate_pays_the_value() {
        assert_eq!(payout_for(0, 1_000_000).unwrap(), 0);
        assert_eq!(payout_for(10_000, 1_000_000).unwrap(), 1_000_000);
    }

    /// Floors, and the direction matters: the payout is compared against Treasury 03's balance
    /// before the transfer, so rounding up could pass the check and fail the CPI.
    #[test]
    fn truncates_towards_the_treasury_rather_than_the_seller() {
        assert_eq!(payout_for(3_333, 10).unwrap(), 3);
    }

    /// `value` is micro-USDC and the rate reaches 10_000, so the product leaves `u64` well
    /// before `value` does. Widening to `u128` is what keeps this a value rather than a wrap.
    #[test]
    fn a_large_value_does_not_wrap_through_the_rate_multiplication() {
        assert_eq!(payout_for(10_000, u64::MAX).unwrap(), u64::MAX);
        assert_eq!(payout_for(5_000, u64::MAX).unwrap(), u64::MAX / 2);
    }

    // --- the Pending / Active branch -----------------------------------------------

    #[test]
    fn no_open_batch_activates_the_position_immediately() {
        let mut pool = configured_pool(0);
        let mut position = zeroed_position();
        let mut stats = wallet_stats(0, 0);
        let t02 = Pubkey::new_unique();
        let value = DEFAULT_ADMISSION_FLOOR * 2;

        let acquired = acquire(&mut pool, &mut position, &mut stats, terms(t02, value)).unwrap();

        assert!(
            acquired.activation.is_some(),
            "no batch open — must activate"
        );
        assert!(position.state == PositionState::Active);
        assert_eq!(pool.n_real, 1, "the draw domain grew");
        assert_eq!(stats.active_positions, 1);
        assert_eq!(stats.active_value, value);
    }

    #[test]
    fn an_open_batch_leaves_the_position_pending_and_moves_no_weight() {
        let mut pool = configured_pool(1);
        let mut position = zeroed_position();
        let mut stats = wallet_stats(0, 0);
        let t02 = Pubkey::new_unique();
        let value = DEFAULT_ADMISSION_FLOOR * 2;

        let acquired = acquire(&mut pool, &mut position, &mut stats, terms(t02, value)).unwrap();

        assert!(
            acquired.activation.is_none(),
            "a batch is open — the caller must emit neither RebalanceEvaluated nor TierChanged"
        );
        assert!(position.state == PositionState::Pending);
        assert_eq!(pool.n_real, 0, "no weight moved");
        assert_eq!(pool.w_real, 0, "no weight moved");
        assert_eq!(
            stats.active_positions, 0,
            "a Pending position is not active"
        );
        assert_eq!(stats.active_value, 0);
    }

    /// The load-bearing half of the branch. `activate_acquired` reads these back rather than
    /// re-attesting, so a `Pending` position that lost them would later have to be activated on
    /// a fresh observation — at a weight Treasury 03 never paid `rate × V` against.
    #[test]
    fn the_attestation_is_persisted_on_both_branches() {
        for open_batches in [0u32, 1] {
            let mut pool = configured_pool(open_batches);
            let mut position = zeroed_position();
            let mut stats = wallet_stats(0, 0);
            let value = DEFAULT_ADMISSION_FLOOR * 2;

            acquire(
                &mut pool,
                &mut position,
                &mut stats,
                terms(Pubkey::new_unique(), value),
            )
            .unwrap();

            assert_eq!(
                (position.recorded_value, position.value_observed_at),
                (value, 555),
                "attestation lost on the open_batches == {open_batches} branch"
            );
        }
    }

    /// A treasury-acquired position earns no Season-0 score, so it carries no lock — on
    /// either branch, and with no argument by which a caller could ask for one.
    #[test]
    fn a_treasury_position_never_carries_a_season_zero_lock() {
        for open_batches in [0u32, 1] {
            let mut pool = configured_pool(open_batches);
            let mut position = zeroed_position();
            let mut stats = wallet_stats(0, 0);
            acquire(
                &mut pool,
                &mut position,
                &mut stats,
                terms(Pubkey::new_unique(), DEFAULT_ADMISSION_FLOOR),
            )
            .unwrap();
            assert_eq!(position.lock_until, 0);
        }
    }

    /// `Position.depositor` is what makes every later instruction treat this as the treasury's:
    /// `approve_deposit` and `reject_deposit` refuse it, leaving `activate_acquired` as the only
    /// activation path. Written from the derived T02 identity, never from the seller.
    #[test]
    fn the_position_is_owned_by_the_t02_identity_and_not_by_the_seller() {
        let mut pool = configured_pool(0);
        let mut position = zeroed_position();
        let mut stats = wallet_stats(0, 0);
        let t02 = Pubkey::new_unique();

        acquire(
            &mut pool,
            &mut position,
            &mut stats,
            terms(t02, DEFAULT_ADMISSION_FLOOR),
        )
        .unwrap();

        assert_eq!(position.depositor, t02);
        assert_eq!(stats.owner, t02, "the rollup is the T02 identity's");
    }

    /// `init_if_needed` is where a second call could silently reset a counter: a second
    /// acquisition must add to an existing rollup's counters, never restate them from zero.
    #[test]
    fn a_second_acquisition_adds_to_the_rollup_instead_of_resetting_it() {
        let mut pool = configured_pool(0);
        let t02 = Pubkey::new_unique();
        let value = DEFAULT_ADMISSION_FLOOR * 2;
        let mut stats = wallet_stats(3, 5_000_000);
        let mut position = zeroed_position();

        acquire(&mut pool, &mut position, &mut stats, terms(t02, value)).unwrap();

        assert_eq!(stats.active_positions, 4, "the existing count was reset");
        assert_eq!(
            stats.active_value,
            5_000_000 + value,
            "the existing value was reset"
        );
        assert_eq!(stats.bump, 7, "an existing account's bump is not rewritten");
    }

    #[test]
    fn each_acquisition_takes_the_next_position_id() {
        let mut pool = configured_pool(0);
        let t02 = Pubkey::new_unique();
        let mut stats = wallet_stats(0, 0);

        let mut weight_index = boxed_zeroed_weight_index();
        let mut top_tier = empty_top_tier();

        let mut first = zeroed_position();
        let a = acquire_with(
            &mut pool,
            &mut weight_index,
            &mut top_tier,
            &mut first,
            &mut stats,
            terms(t02, DEFAULT_ADMISSION_FLOOR),
        )
        .unwrap();
        let mut second = zeroed_position();
        let b = acquire_with(
            &mut pool,
            &mut weight_index,
            &mut top_tier,
            &mut second,
            &mut stats,
            terms(t02, DEFAULT_ADMISSION_FLOOR),
        )
        .unwrap();

        assert_eq!((a.position_id, b.position_id), (41, 42));
        assert_eq!(pool.position_counter, 43);
    }

    // --- the delayed activation -------------------------------------------------------------
    //
    // The integration suite cannot reach this path: a treasury position only becomes `Pending`
    // when a sell lands while `open_batches > 0`, and `commit_rolls` (the only writer of that
    // counter) needs the VRF program the suite's validator does not load.

    fn t02_identity(pool: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(&[TREASURY_02_SEED, pool.as_ref()], &crate::ID).0
    }

    /// A position as `instant_sell` leaves it when a batch is open: `Pending`, owned by T02, and
    /// carrying the attestation the sell made.
    fn pending_treasury_position(t02: Pubkey, value: u64, observed_at: i64) -> Position {
        let mut position = zeroed_position();
        position.depositor = t02;
        position.state = PositionState::Pending;
        position.recorded_value = value;
        position.value_observed_at = observed_at;
        position
    }

    fn activate(
        pool: &mut Pool,
        position: &mut Position,
        stats: &mut WalletStats,
        now: i64,
    ) -> Result<AcquiredActivation> {
        let mut weight_index = boxed_zeroed_weight_index();
        let mut top_tier = empty_top_tier();
        activate_acquired_position(
            ActivationAccounts {
                pool,
                weight_index: &mut weight_index,
                top_tier: &mut top_tier,
                position,
                position_key: Pubkey::new_unique(),
                wallet_stats: stats,
                remaining_accounts: &[],
            },
            now,
        )
    }

    /// Weight, recorded value and observation timestamp all come from the stored attestation;
    /// `activated_at` is the only field the clock reaches.
    #[test]
    fn the_activation_takes_the_stored_attestation_and_the_clock_reaches_only_activated_at() {
        let pool_key = Pubkey::new_unique();
        let mut pool = configured_pool(0);
        let stored_value = DEFAULT_ADMISSION_FLOOR * 3;
        let stored_observed_at = 555;
        let now = 9_000_000;
        let mut position =
            pending_treasury_position(t02_identity(&pool_key), stored_value, stored_observed_at);
        let mut stats = wallet_stats(0, 0);

        let activated = activate(&mut pool, &mut position, &mut stats, now).unwrap();

        assert_eq!(activated.value, stored_value);
        assert_eq!(activated.observed_at, stored_observed_at);
        assert_eq!(position.recorded_value, stored_value);
        assert_eq!(position.deposit_value, stored_value);
        assert_eq!(
            position.value_observed_at, stored_observed_at,
            "the observation timestamp was overwritten with the activation clock"
        );
        assert_eq!(position.activated_at, now);
        assert_eq!(pool.w_real, u128::from(weight_of(stored_value).unwrap()));
    }

    /// Two positions differing only in stored value, activated on the same pool at the same
    /// instant, enter at different weights.
    #[test]
    fn two_positions_stored_at_different_values_enter_at_different_weights() {
        let now = 9_000_000;
        let mut weights = Vec::new();
        for value in [DEFAULT_ADMISSION_FLOOR * 2, DEFAULT_ADMISSION_FLOOR * 7] {
            let pool_key = Pubkey::new_unique();
            let mut pool = configured_pool(0);
            let mut position = pending_treasury_position(t02_identity(&pool_key), value, 555);
            let mut stats = wallet_stats(0, 0);
            activate(&mut pool, &mut position, &mut stats, now).unwrap();
            weights.push(pool.w_real);
        }
        assert_ne!(
            weights[0], weights[1],
            "the two stored values produced one weight — the activation is not reading the \
             stored value at all"
        );
    }

    /// **The limits are configuration and are re-checked, not trusted from acceptance.** Each is
    /// driven from both directions against the *stored* value, on a pool configured after the
    /// fact — which is the case the re-check exists for: a card bought when the pool had no
    /// ceiling must not activate into a pool that has one it exceeds.
    #[test]
    fn the_admission_floor_is_re_checked_against_the_stored_value_from_both_directions() {
        for (floor, expected) in [
            (DEFAULT_ADMISSION_FLOOR, None),
            (DEFAULT_ADMISSION_FLOOR + 1, Some(6104)),
        ] {
            let pool_key = Pubkey::new_unique();
            let mut pool = configured_pool(0);
            pool.admission_floor = floor;
            let mut position =
                pending_treasury_position(t02_identity(&pool_key), DEFAULT_ADMISSION_FLOOR, 555);
            let mut stats = wallet_stats(0, 0);

            let result = activate(&mut pool, &mut position, &mut stats, 9_000_000);
            match expected {
                None => assert!(result.is_ok(), "a stored value at the floor must activate"),
                Some(code) => {
                    assert_eq!(refusal(result), code);
                    assert_eq!(pool.w_real, 0, "weight moved before the refusal");
                }
            }
        }
    }

    #[test]
    fn the_admission_ceiling_is_re_checked_against_the_stored_value_from_both_directions() {
        let stored = DEFAULT_ADMISSION_FLOOR * 4;
        for (ceiling, expected) in [(stored, None), (stored - 1, Some(6105))] {
            let pool_key = Pubkey::new_unique();
            let mut pool = configured_pool(0);
            pool.admission_ceiling = ceiling;
            let mut position = pending_treasury_position(t02_identity(&pool_key), stored, 555);
            let mut stats = wallet_stats(0, 0);

            let result = activate(&mut pool, &mut position, &mut stats, 9_000_000);
            match expected {
                None => assert!(
                    result.is_ok(),
                    "a stored value at the ceiling must activate"
                ),
                Some(code) => {
                    assert_eq!(refusal(result), code);
                    assert_eq!(pool.w_real, 0, "weight moved before the refusal");
                }
            }
        }
    }

    /// The cap reads the **current** rollup, not the one in force at acceptance — which is the
    /// half a stored-value-only re-check would miss: every other treasury acquisition that
    /// activated while this one waited raised `active_value`, and the cap bounds the total.
    #[test]
    fn the_wallet_cap_is_re_checked_against_the_current_rollup_from_both_directions() {
        let stored = DEFAULT_ADMISSION_FLOOR * 2;
        let cap = DEFAULT_ADMISSION_FLOOR * 6;
        for (already_active, expected) in [(cap - stored, None), (cap - stored + 1, Some(6106))] {
            let pool_key = Pubkey::new_unique();
            let mut pool = configured_pool(0);
            pool.wallet_value_cap = cap;
            let mut position = pending_treasury_position(t02_identity(&pool_key), stored, 555);
            let mut stats = wallet_stats(1, already_active);

            let result = activate(&mut pool, &mut position, &mut stats, 9_000_000);
            match expected {
                None => assert!(result.is_ok(), "a total at exactly the cap must activate"),
                Some(code) => {
                    assert_eq!(refusal(result), code);
                    assert_eq!(pool.w_real, 0, "weight moved before the refusal");
                    assert_eq!(
                        (stats.active_positions, stats.active_value),
                        (1, already_active),
                        "the rollup was credited before the refusal"
                    );
                }
            }
        }
    }

    /// A position admissible when bought is refused when the configuration moved under it.
    #[test]
    fn a_position_admissible_at_acceptance_is_refused_after_the_floor_is_raised() {
        let pool_key = Pubkey::new_unique();
        let stored = DEFAULT_ADMISSION_FLOOR;
        let t02 = t02_identity(&pool_key);

        let mut accepted_pool = configured_pool(0);
        let mut probe = pending_treasury_position(t02, stored, 555);
        let mut probe_stats = wallet_stats(0, 0);
        assert!(
            activate(&mut accepted_pool, &mut probe, &mut probe_stats, 9_000_000).is_ok(),
            "the control failed: this value is admissible under the acceptance-time floor"
        );

        let mut raised = configured_pool(0);
        raised.admission_floor = stored * 2;
        let mut position = pending_treasury_position(t02, stored, 555);
        let mut stats = wallet_stats(0, 0);
        assert_eq!(
            refusal(activate(&mut raised, &mut position, &mut stats, 9_000_000)),
            6104
        );
    }

    /// A depositor's own `Pending` position carries no stored value and has its own activation
    /// path, which attests a fresh one. Admitting it here would activate it on a value nobody
    /// attested, skipping the attestation entirely.
    #[test]
    fn a_pending_position_with_no_stored_value_is_refused() {
        let mut pool = configured_pool(0);
        let mut position = pending_treasury_position(Pubkey::new_unique(), 0, 555);
        let mut stats = wallet_stats(0, 0);

        assert_eq!(
            refusal(activate(&mut pool, &mut position, &mut stats, 9_000_000)),
            6311
        );
        assert_eq!(
            pool.w_real, 0,
            "weight moved for a position it then refused"
        );
        assert!(position.state == PositionState::Pending);
    }

    /// An escrow sale leaves the loader's position `Pending` while a batch is open, and this is
    /// its only way out. It activates on the stored value into the loader's own rollup.
    #[test]
    fn a_loaders_pending_position_activates_into_the_loaders_rollup() {
        let mut pool = configured_pool(0);
        let loader = Pubkey::new_unique();
        let value = DEFAULT_ADMISSION_FLOOR * 2;
        let mut position = pending_treasury_position(loader, value, 555);
        let mut stats = wallet_stats(2, 4_000_000);

        activate(&mut pool, &mut position, &mut stats, 9_000_000).unwrap();

        assert!(position.state == PositionState::Active);
        assert_eq!(position.depositor, loader);
        assert_eq!(
            (stats.active_positions, stats.active_value),
            (3, 4_000_000 + value)
        );
    }

    /// Activation adds weight, and an open batch's draw domain must stay fixed until it
    /// resolves. This is the one guard the sell itself deliberately does not carry, so it has to
    /// hold here or an acquisition joins a batch's domain after it opened.
    #[test]
    fn a_treasury_position_is_refused_while_a_batch_is_open() {
        for open_batches in [1u32, 5] {
            let pool_key = Pubkey::new_unique();
            let mut pool = configured_pool(open_batches);
            let mut position =
                pending_treasury_position(t02_identity(&pool_key), DEFAULT_ADMISSION_FLOOR, 555);
            let mut stats = wallet_stats(0, 0);

            assert_eq!(
                refusal(activate(&mut pool, &mut position, &mut stats, 9_000_000)),
                6200
            );
            assert_eq!(pool.w_real, 0);
            assert!(position.state == PositionState::Pending);
        }
    }

    /// Only `Pending` leaves through this door. An already-`Active` position re-activated would
    /// take a second Fenwick leaf and count twice in `n_real` — the pool would draw a card it
    /// holds once as though it held two.
    #[test]
    fn an_already_active_treasury_position_is_refused() {
        let pool_key = Pubkey::new_unique();
        let mut pool = configured_pool(0);
        let mut position =
            pending_treasury_position(t02_identity(&pool_key), DEFAULT_ADMISSION_FLOOR, 555);
        position.state = PositionState::Active;
        let mut stats = wallet_stats(1, DEFAULT_ADMISSION_FLOOR);

        assert_eq!(
            refusal(activate(&mut pool, &mut position, &mut stats, 9_000_000)),
            6400
        );
        assert_eq!(pool.n_real, 0, "the position was inserted a second time");
    }

    /// The rollup is credited, never restated. It was created and paid for at the sell, and it
    /// serves every treasury acquisition in the pool — so an activation that reset it would
    /// erase every earlier one's contribution to the cap it is read against.
    #[test]
    fn the_activation_credits_the_existing_rollup_rather_than_resetting_it() {
        let pool_key = Pubkey::new_unique();
        let mut pool = configured_pool(0);
        let value = DEFAULT_ADMISSION_FLOOR * 2;
        let mut position = pending_treasury_position(t02_identity(&pool_key), value, 555);
        let mut stats = wallet_stats(3, 5_000_000);

        activate(&mut pool, &mut position, &mut stats, 9_000_000).unwrap();

        assert_eq!(stats.active_positions, 4);
        assert_eq!(stats.active_value, 5_000_000 + value);
        assert_eq!(stats.bump, 7, "an existing account's bump is not rewritten");
    }

    /// No lock, and no argument by which a caller could ask for one — the same rule the
    /// immediate-activation branch follows, reached by a different door.
    #[test]
    fn a_delayed_activation_carries_no_season_zero_lock_either() {
        let pool_key = Pubkey::new_unique();
        let mut pool = configured_pool(0);
        let mut position =
            pending_treasury_position(t02_identity(&pool_key), DEFAULT_ADMISSION_FLOOR, 555);
        let mut stats = wallet_stats(0, 0);

        activate(&mut pool, &mut position, &mut stats, 9_000_000).unwrap();

        assert_eq!(position.lock_until, 0);
        assert!(position.state == PositionState::Active);
    }

    // --- the loader accounts --------------------------------------------------------------------

    use crate::common::seeds::{ESCROW_SEED, ESCROW_VAULT_SEED, WALLET_STATS_SEED};
    use anchor_lang::solana_program::program_pack::Pack;
    use anchor_spl::token::spl_token;

    /// One account's storage, so a test can hand out an `AccountInfo` over it.
    struct Stored {
        key: Pubkey,
        owner: Pubkey,
        lamports: u64,
        data: Vec<u8>,
    }

    impl Stored {
        fn info(&mut self) -> AccountInfo<'_> {
            AccountInfo::new(
                &self.key,
                false,
                true,
                &mut self.lamports,
                &mut self.data,
                &self.owner,
                false,
                0,
            )
        }
    }

    fn pda(seeds: &[&[u8]]) -> (Pubkey, u8) {
        Pubkey::find_program_address(seeds, &crate::ID)
    }

    fn stored_escrow(owner: Pubkey, armed: bool) -> Stored {
        let (key, bump) = pda(&[ESCROW_SEED, owner.as_ref()]);
        let (_, vault_bump) = pda(&[ESCROW_VAULT_SEED, key.as_ref()]);
        let mut data = Vec::new();
        Escrow {
            owner,
            armed,
            bump,
            vault_bump,
            reserved: [0; 64],
        }
        .try_serialize(&mut data)
        .unwrap();
        Stored {
            key,
            owner: crate::ID,
            lamports: 1,
            data,
        }
    }

    fn stored_vault(escrow: &Pubkey, amount: u64) -> Stored {
        let (key, _) = pda(&[ESCROW_VAULT_SEED, escrow.as_ref()]);
        let mut data = vec![0u8; spl_token::state::Account::LEN];
        spl_token::state::Account {
            mint: Pubkey::new_unique(),
            owner: *escrow,
            amount,
            state: spl_token::state::AccountState::Initialized,
            ..Default::default()
        }
        .pack_into_slice(&mut data);
        Stored {
            key,
            owner: spl_token::ID,
            lamports: 1,
            data,
        }
    }

    fn stored_stats(pool: &Pubkey, owner: &Pubkey, active_value: u64) -> Stored {
        let (key, bump) = pda(&[WALLET_STATS_SEED, pool.as_ref(), owner.as_ref()]);
        let mut data = Vec::new();
        WalletStats {
            pool: *pool,
            owner: *owner,
            active_positions: u32::from(active_value > 0),
            active_value,
            bump,
        }
        .try_serialize(&mut data)
        .unwrap();
        Stored {
            key,
            owner: crate::ID,
            lamports: 1,
            data,
        }
    }

    /// A loader's three accounts, bound to one owner in one pool.
    struct LoaderSet {
        pool: Pubkey,
        owner: Pubkey,
        escrow: Stored,
        vault: Stored,
        stats: Stored,
    }

    fn loader_set(armed: bool, vault_amount: u64, active_value: u64) -> LoaderSet {
        let pool = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let escrow = stored_escrow(owner, armed);
        let vault = stored_vault(&escrow.key, vault_amount);
        let stats = stored_stats(&pool, &owner, active_value);
        LoaderSet {
            pool,
            owner,
            escrow,
            vault,
            stats,
        }
    }

    fn bind_code(result: Result<Option<Loader>>) -> u32 {
        match result {
            Ok(_) => panic!("expected EscrowAccountsMismatch, but the loader bound"),
            Err(err) => error_code(err),
        }
    }

    #[test]
    fn no_loader_accounts_is_no_escrow_path() {
        let bound = bind_loader(
            &Pubkey::new_unique(),
            LoaderAccounts {
                escrow: None,
                escrow_vault: None,
                wallet_stats: None,
            },
        )
        .unwrap();
        assert!(bound.is_none());
    }

    #[test]
    fn every_partial_set_of_loader_accounts_is_refused() {
        for mask in 1u8..7 {
            let mut set = loader_set(true, 100_000_000, 0);
            let (escrow, vault, stats) = (set.escrow.info(), set.vault.info(), set.stats.info());
            let code = bind_code(bind_loader(
                &set.pool,
                LoaderAccounts {
                    escrow: (mask & 1 != 0).then_some(&escrow),
                    escrow_vault: (mask & 2 != 0).then_some(&vault),
                    wallet_stats: (mask & 4 != 0).then_some(&stats),
                },
            ));
            assert_eq!(code, 6802, "partial set {mask:03b} was not refused");
        }
    }

    /// A loader who never opened an escrow buys nothing, so the other two accounts are not read
    /// at all — garbage there is not an error.
    #[test]
    fn an_escrow_with_no_data_is_no_escrow_path_and_the_rest_is_unread() {
        let mut empty = Stored {
            key: pda(&[ESCROW_SEED, Pubkey::new_unique().as_ref()]).0,
            owner: anchor_lang::solana_program::system_program::ID,
            lamports: 0,
            data: Vec::new(),
        };
        let mut garbage = Stored {
            key: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            lamports: 1,
            data: vec![7; 9],
        };
        let mut other = Stored {
            key: Pubkey::new_unique(),
            owner: Pubkey::new_unique(),
            lamports: 1,
            data: vec![9; 3],
        };
        let (escrow, vault, stats) = (empty.info(), garbage.info(), other.info());
        let bound = bind_loader(
            &Pubkey::new_unique(),
            LoaderAccounts {
                escrow: Some(&escrow),
                escrow_vault: Some(&vault),
                wallet_stats: Some(&stats),
            },
        )
        .unwrap();
        assert!(bound.is_none());
    }

    #[test]
    fn a_complete_set_binds_to_the_escrow_owner() {
        let mut set = loader_set(true, 92_500_000, 3_000_000);
        let (escrow, vault, stats) = (set.escrow.info(), set.vault.info(), set.stats.info());
        let loader = bind_loader(
            &set.pool,
            LoaderAccounts {
                escrow: Some(&escrow),
                escrow_vault: Some(&vault),
                wallet_stats: Some(&stats),
            },
        )
        .unwrap()
        .expect("a complete set binds");
        assert_eq!(loader.owner, set.owner);
        assert!(loader.armed);
        assert_eq!(loader.vault_balance, 92_500_000);
        assert_eq!(loader.wallet_stats.active_value, 3_000_000);
    }

    /// Each account swapped for one of another owner's, another pool's, or an impostor at the
    /// right address: every substitution is refused, never read as a loader that does not
    /// qualify.
    #[test]
    fn a_vault_or_rollup_not_derived_from_the_escrow_owner_is_refused() {
        let mut set = loader_set(true, 100_000_000, 0);
        let mut stranger = loader_set(true, 100_000_000, 0);
        let mut other_pool_stats = stored_stats(&Pubkey::new_unique(), &set.owner, 0);
        let mut not_ours = stored_escrow(set.owner, true);
        not_ours.owner = Pubkey::new_unique();
        let mut vault_not_token = stored_vault(&set.escrow.key, 100_000_000);
        vault_not_token.owner = crate::ID;
        // The stranger's escrow bytes at this owner's address: the stored owner derives another
        // address, so the account is not where it claims to be.
        let mut misplaced = Stored {
            key: set.escrow.key,
            owner: crate::ID,
            lamports: 1,
            data: stranger.escrow.data.clone(),
        };

        let pool = set.pool;
        let check = |label: &str, escrow: &mut Stored, vault: &mut Stored, stats: &mut Stored| {
            let (e, v, s) = (escrow.info(), vault.info(), stats.info());
            let code = bind_code(bind_loader(
                &pool,
                LoaderAccounts {
                    escrow: Some(&e),
                    escrow_vault: Some(&v),
                    wallet_stats: Some(&s),
                },
            ));
            assert_eq!(code, 6802, "{label} was not refused");
        };
        check(
            "another escrow's vault",
            &mut set.escrow,
            &mut stranger.vault,
            &mut set.stats,
        );
        check(
            "another owner's rollup",
            &mut set.escrow,
            &mut set.vault,
            &mut stranger.stats,
        );
        check(
            "another pool's rollup",
            &mut set.escrow,
            &mut set.vault,
            &mut other_pool_stats,
        );
        check(
            "an escrow another program owns",
            &mut not_ours,
            &mut set.vault,
            &mut set.stats,
        );
        check(
            "a vault the token program does not own",
            &mut set.escrow,
            &mut vault_not_token,
            &mut set.stats,
        );
        check(
            "an escrow at another owner's address",
            &mut misplaced,
            &mut set.vault,
            &mut set.stats,
        );

        // Each of the next three is built so that every check but one passes, so a missing check
        // shows as a bind rather than as a refusal some later check happens to make.

        // An `Escrow` claiming this owner, at an address that is not this owner's escrow, with a
        // vault really derived from that address and this owner's real rollup.
        let impostor_key = Pubkey::new_unique();
        let (_, impostor_vault_bump) = pda(&[ESCROW_VAULT_SEED, impostor_key.as_ref()]);
        let mut impostor_data = Vec::new();
        Escrow {
            owner: set.owner,
            armed: true,
            bump: pda(&[ESCROW_SEED, set.owner.as_ref()]).1,
            vault_bump: impostor_vault_bump,
            reserved: [0; 64],
        }
        .try_serialize(&mut impostor_data)
        .unwrap();
        let mut impostor = Stored {
            key: impostor_key,
            owner: crate::ID,
            lamports: 1,
            data: impostor_data,
        };
        let mut impostor_vault = stored_vault(&impostor_key, 100_000_000);
        check(
            "an escrow not at its owner's address",
            &mut impostor,
            &mut impostor_vault,
            &mut set.stats,
        );

        // This owner's rollup bytes under another program.
        let mut stats_not_ours = stored_stats(&pool, &set.owner, 0);
        stats_not_ours.owner = Pubkey::new_unique();
        check(
            "a rollup another program owns",
            &mut set.escrow,
            &mut set.vault,
            &mut stats_not_ours,
        );

        // A program-owned rollup at another address whose stored bump is this owner's, so the
        // address it derives is this owner's rollup and only the comparison can refuse it.
        let mut foreign_stats = stored_stats(&pool, &stranger.owner, 0);
        let own_bump = pda(&[WALLET_STATS_SEED, pool.as_ref(), set.owner.as_ref()]).1;
        *foreign_stats.data.last_mut().unwrap() = own_bump;
        check(
            "a rollup at another owner's address",
            &mut set.escrow,
            &mut set.vault,
            &mut foreign_stats,
        );
    }

    // --- escrow eligibility ---------------------------------------------------------------------

    fn bid_pool(bid_bps: u16, rate_bps: u16, wallet_value_cap: u64) -> Pool {
        let mut pool = configured_pool(0);
        pool.escrow_bid_bps = bid_bps;
        pool.buyback_rate_bps = rate_bps;
        pool.wallet_value_cap = wallet_value_cap;
        pool
    }

    /// Runs `escrow_payment` against a freshly bound loader.
    fn payment_for(
        pool: &Pool,
        value: u64,
        armed: bool,
        vault_amount: u64,
        active_value: u64,
    ) -> Option<u64> {
        let mut set = loader_set(armed, vault_amount, active_value);
        let (escrow, vault, stats) = (set.escrow.info(), set.vault.info(), set.stats.info());
        let loader = bind_loader(
            &set.pool,
            LoaderAccounts {
                escrow: Some(&escrow),
                escrow_vault: Some(&vault),
                wallet_stats: Some(&stats),
            },
        )
        .unwrap();
        escrow_payment(pool, value, loader.as_ref()).unwrap()
    }

    #[test]
    fn a_qualifying_escrow_pays_the_bid_applied_to_the_value() {
        let pool = bid_pool(9_250, 8_500, 0);
        assert_eq!(
            payment_for(&pool, 100_000_000, true, 92_500_000, 0),
            Some(92_500_000)
        );
    }

    /// Each condition on its own sends the sale to Treasury 03 — never an error.
    #[test]
    fn each_ineligibility_falls_through_to_treasury_03() {
        let value = 100_000_000;
        let ok = bid_pool(9_250, 8_500, 0);
        assert_eq!(escrow_payment(&ok, value, None).unwrap(), None, "no loader");
        assert_eq!(
            payment_for(&bid_pool(0, 8_500, 0), value, true, u64::MAX, 0),
            None,
            "a pool with no bid"
        );
        assert_eq!(
            payment_for(&ok, value, false, u64::MAX, 0),
            None,
            "disarmed"
        );
        assert_eq!(
            payment_for(&ok, value, true, 92_500_000 - 1, 0),
            None,
            "one unit short"
        );
        let capped = bid_pool(9_250, 8_500, 5 * value);
        assert_eq!(
            payment_for(&capped, value, true, u64::MAX, 4 * value + 1),
            None,
            "one unit over the loader's cap"
        );
        assert_eq!(
            payment_for(&capped, value, true, 92_500_000, 4 * value),
            Some(92_500_000),
            "exactly covered and exactly at the cap must pay — the control for both refusals"
        );
    }

    /// The seller's amount comes from the rate on both paths, and the escrow's debit splits into
    /// that amount and Treasury 03's spread with nothing lost or invented, for every value and
    /// every bid the configuration allows.
    #[test]
    fn the_escrow_debit_is_the_sellers_amount_plus_the_spread_and_the_spread_is_never_negative() {
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let rate = (next() % 10_001) as u16;
            let bid = rate + (next() % u64::from(10_001 - rate)) as u16;
            let value = next() % 10_000_000_000_000;
            let paid = payout_for(rate, value).unwrap();
            let escrow_paid = payout_for(bid, value).unwrap();
            assert!(
                escrow_paid >= paid,
                "bid {bid} paid less than rate {rate} on {value}"
            );
            let spread = escrow_paid - paid;
            assert_eq!(paid + spread, escrow_paid);
            assert_eq!(
                u128::from(escrow_paid),
                u128::from(value) * u128::from(bid) / 10_000,
                "the debit is not floor(bid × value / 10⁴)"
            );
        }
    }

    // --- who owns the card ----------------------------------------------------------------------

    /// An escrow sale's card is the loader's position, credited to the loader's rollup, and
    /// Treasury 02's rollup keeps its counters.
    #[test]
    fn an_escrow_sale_makes_the_card_the_loaders_and_never_credits_treasury_02() {
        let mut set = loader_set(true, 100_000_000, 2_000_000);
        let (escrow, vault, stats) = (set.escrow.info(), set.vault.info(), set.stats.info());
        let loader = bind_loader(
            &set.pool,
            LoaderAccounts {
                escrow: Some(&escrow),
                escrow_vault: Some(&vault),
                wallet_stats: Some(&stats),
            },
        )
        .unwrap();
        let mut sale = Sale {
            payer: BuybackPayer::Escrow,
            rate_bps: 8_500,
            paid: 85_000_000,
            bid_bps: 9_250,
            escrow_paid: 92_500_000,
            loader,
        };
        let t02 = Pubkey::new_unique();
        let mut t02_stats = wallet_stats(5, 50_000_000);
        let mut pool = configured_pool(0);
        let mut position = zeroed_position();
        let value = DEFAULT_ADMISSION_FLOOR * 2;

        let (depositor, rollup) = sale.acquirer(set.pool, t02, &mut t02_stats);
        acquire(&mut pool, &mut position, rollup, terms(depositor, value)).unwrap();
        sale.store_loader_rollup().unwrap();

        assert_eq!(position.depositor, set.owner);
        assert_eq!(sale.escrow_owner(), set.owner);
        assert_eq!(
            (t02_stats.active_positions, t02_stats.active_value),
            (5, 50_000_000),
            "Treasury 02's rollup was credited for an escrow sale"
        );
        assert_eq!((t02_stats.pool, t02_stats.owner), (set.pool, t02));

        drop((escrow, vault, stats));
        let stored = WalletStats::try_deserialize(&mut &set.stats.data[..]).unwrap();
        assert_eq!(
            (stored.active_positions, stored.active_value),
            (2, 2_000_000 + value),
            "the loader's rollup was not written back to its account"
        );
    }

    #[test]
    fn a_treasury_03_sale_makes_the_card_treasury_02s() {
        let mut sale = Sale {
            payer: BuybackPayer::Treasury03,
            rate_bps: 8_500,
            paid: 85_000_000,
            bid_bps: 0,
            escrow_paid: 0,
            loader: None,
        };
        let t02 = Pubkey::new_unique();
        let mut t02_stats = wallet_stats(0, 0);
        let mut pool = configured_pool(0);
        let mut position = zeroed_position();
        let value = DEFAULT_ADMISSION_FLOOR;

        let (depositor, rollup) = sale.acquirer(Pubkey::new_unique(), t02, &mut t02_stats);
        acquire(&mut pool, &mut position, rollup, terms(depositor, value)).unwrap();
        sale.store_loader_rollup().unwrap();

        assert_eq!(position.depositor, t02);
        assert_eq!(sale.escrow_owner(), Pubkey::default());
        assert_eq!(
            (t02_stats.active_positions, t02_stats.active_value),
            (1, value)
        );
    }
}

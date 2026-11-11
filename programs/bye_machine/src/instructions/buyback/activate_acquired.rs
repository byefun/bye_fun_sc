use anchor_lang::prelude::*;

use crate::common::activation::ActivationAccounts;
use crate::common::buyback::activate_acquired_position;
use crate::common::errors::ByeMachineError;
use crate::common::events::{AcquiredActivated, RebalanceEvaluated, TierChanged};
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED, WALLET_STATS_SEED,
};
use crate::state::{Pool, Position, ProtocolConfig, TopTier, WalletStats, WeightIndex};

/// The treasury's own door into the draw domain: activates a card bought through `instant_sell`
/// while a batch was open, on the attestation it was bought at.
///
/// **No arguments, and the absence is the instruction.** `value` and `observed_at` are read back
/// off the position rather than supplied, because the sell already attested them and Treasury 03
/// already paid `rate × V` against that figure. Taking a fresh observation here — which is what
/// routing the position through `approve_deposit` would do — would let a card whose value moved
/// while it waited enter the pool at a weight the treasury never paid for. That mispricing is
/// silent: no guard fails, no event looks wrong, and the pool's own book is the thing that
/// drifts.
///
/// **No `lock_until` either.** A treasury-acquired position earns no Season-0 score, so it
/// carries no lock and activates at `lock_until = 0` — written as a literal, not taken as an
/// argument, since there is no caller who may choose otherwise. A position below the admission
/// floor is closed by `record_value` regardless of any lock, and `lock_until` reaches voluntary
/// withdrawal only.
///
/// **The refusals and the read-back live in `activate_acquired_position`, not here**: a handler
/// takes a `Context` and cannot be unit-driven, so the shared accounting is where they are
/// testable. What stays here is the account surface, the single call, and the three events.
///
/// **Moves no NFT.** The card entered escrow at `instant_sell` and does not move here, so this
/// instruction has no `*_core` twin and never reads `standard`: weight, tier rank and the blank
/// rebalance know nothing about which token standard the escrow holds.
pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, ActivateAcquired<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let authority = ctx.accounts.operator.key();

    let mut weight_index = ctx.accounts.weight_index.load_mut()?;
    let activated = activate_acquired_position(
        ActivationAccounts {
            pool: &mut ctx.accounts.pool,
            weight_index: &mut weight_index,
            top_tier: &mut ctx.accounts.top_tier,
            position: &mut ctx.accounts.position,
            position_key,
            wallet_stats: &mut ctx.accounts.wallet_stats,
            remaining_accounts: ctx.remaining_accounts,
        },
        now,
    )?;
    drop(weight_index);

    // Deliberately not a `DepositApproved`: no attestation happens here, so an approval event
    // would record a value decision nobody made. `observed_age_seconds` is carried because the
    // gap between acceptance and activation is unbounded — nothing bounds how long a batch stays
    // open — and this field is what makes that gap measurable rather than inferred.
    emit!(AcquiredActivated {
        pool: pool_key,
        slot: clock.slot,
        authority,
        position: position_key,
        value: activated.value,
        observed_at: activated.observed_at,
        observed_age_seconds: now
            .checked_sub(activated.observed_at)
            .ok_or(ByeMachineError::ArithmeticFailure)?,
    });
    emit!(RebalanceEvaluated {
        pool: pool_key,
        slot: clock.slot,
        n_real: ctx.accounts.pool.n_real,
        w_real: ctx.accounts.pool.w_real,
        blanks_before: activated.activation.blanks_before,
        blanks_after: ctx.accounts.pool.blanks,
    });
    if activated.activation.entered_tier {
        emit!(TierChanged {
            pool: pool_key,
            slot: clock.slot,
            left: activated.activation.displaced,
            entered: Some(position_key),
        });
    }

    Ok(())
}

/// One signer and no payer. Every account this touches already exists — the position and its
/// vault were created at `instant_sell`, and so was the T02 rollup, rent paid by the Operator
/// there because the treasury PDA never calls `deposit` and no other instruction creates it.
#[derive(Accounts)]
pub struct ActivateAcquired<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump
    )]
    pub position: Box<Account<'info, Position>>,

    #[account(mut, address = pool.weight_index)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    #[account(
        mut,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    /// The T02 rollup, taken as an existing writable account exactly as `approve_deposit` takes
    /// a depositor's own. Seeded on `position.depositor`, which the treasury inclusion above
    /// pins to this pool's T02 identity — so the account whose `active_value` the wallet cap is
    /// checked against is the same one the activation then credits.
    #[account(
        mut,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()],
        bump = wallet_stats.bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{calls, production, writes};

    const SRC: &str = include_str!("activate_acquired.rs");
    const APPROVE_SRC: &str = include_str!("../deposit/approve_deposit.rs");
    const INSTANT_SELL_SRC: &str = include_str!("instant_sell.rs");
    const BUYBACK_SRC: &str = include_str!("../../common/buyback.rs");

    /// **No arguments at all**, and the absence is the instruction. A `value: u64` argument
    /// would compile, encode against an IDL generated from this same file, and silently re-price
    /// the treasury's book the first time a caller passed a different number.
    #[test]
    fn the_handler_takes_no_argument_beyond_its_context() {
        let signature = production(SRC)
            .split("pub fn handler")
            .nth(1)
            .expect("the handler must be declared")
            .split(") -> Result<()> {")
            .next()
            .expect("the handler signature must be terminated")
            .to_string();
        // Positive control: a split that caught the wrong span would report an absence for any
        // needle.
        assert!(
            signature.contains("ActivateAcquired<'info>"),
            "the extracted span is not the handler signature — the split markers moved"
        );
        for parameter in ["value: u64", "observed_at: i64", "lock_until: i64"] {
            assert!(
                !signature.contains(parameter),
                "activate_acquired gained a `{parameter}` parameter — this instruction consumes \
                 the attestation the sell already made and must not accept a second one"
            );
        }
    }

    /// It reaches the shared half exactly once and never restates a guard beside it: a copied
    /// guard is a second, unreviewed refusal, and a guard added only here would run on this path
    /// only.
    #[test]
    fn it_reaches_the_shared_activation_half_exactly_once_and_guards_nothing_itself() {
        assert_eq!(calls(SRC, "activate_acquired_position"), 1);
        for guard in [
            "assert_acquired_position",
            "assert_weight_open",
            "validate_admission_limits",
            "activate_position",
        ] {
            assert_eq!(
                calls(SRC, guard),
                0,
                "{guard} is called here as well as in the shared half"
            );
        }
        // The control: the shared half really does carry those four.
        for guard in [
            "assert_acquired_position",
            "assert_weight_open",
            "validate_admission_limits",
        ] {
            assert_eq!(
                calls(BUYBACK_SRC, guard),
                1,
                "{guard} is absent from both the handler and the shared half"
            );
        }
    }

    /// The event fields come off the shared half's return value, not a second read: re-reading
    /// `ctx.accounts.position.recorded_value` would publish whatever the activation left there
    /// rather than the value it admitted.
    #[test]
    fn the_event_reports_the_values_the_activation_admitted() {
        let prod = production(SRC);
        assert!(prod.contains("value: activated.value,"));
        assert!(prod.contains("observed_at: activated.observed_at,"));
        assert!(prod.contains(".checked_sub(activated.observed_at)"));
        assert_eq!(
            prod.matches("ctx.accounts.position.recorded_value").count(),
            0
        );
        assert_eq!(
            prod.matches("ctx.accounts.position.value_observed_at")
                .count(),
            0
        );
    }

    /// One activation publishes one rebalance, and the tier event is conditional on the
    /// activation's own outcome rather than on a local the handler recomputes.
    #[test]
    fn the_two_conditional_events_come_off_the_activations_outcome() {
        let prod = production(SRC);
        assert_eq!(prod.matches("emit!(RebalanceEvaluated").count(), 1);
        assert!(prod.contains("if activated.activation.entered_tier {"));
        assert!(prod.contains("blanks_before: activated.activation.blanks_before"));
        assert!(prod.contains("left: activated.activation.displaced"));
    }

    /// `AcquiredActivated` and not `DepositApproved`: the events share field names, so a swap
    /// compiles, but the wire shape distinguishes "the Operator attested this value" from "the
    /// pool read back a value attested elsewhere".
    #[test]
    fn it_publishes_an_activation_and_never_an_approval() {
        let prod = production(SRC);
        assert_eq!(prod.matches("emit!(AcquiredActivated").count(), 1);
        assert_eq!(prod.matches("emit!(DepositApproved").count(), 0);
        assert_eq!(
            production(APPROVE_SRC)
                .matches("emit!(AcquiredActivated")
                .count(),
            0
        );
    }

    /// It writes no position identity field. Every one of them was written at `instant_sell`,
    /// and re-writing `depositor` or `nft_mint` here would let an activation redirect the card
    /// or its ownership after Treasury 03 had already paid for it.
    #[test]
    fn it_re_states_no_field_the_acquisition_already_wrote() {
        for field in [
            "position.pool",
            "position.depositor",
            "position.nft_mint",
            "position.position_id",
            "position.standard",
            "position.bump",
            "position.vault_bump",
        ] {
            assert_eq!(
                writes(SRC, field),
                0,
                "{field} is written here — the acquisition already wrote it"
            );
        }
        // The control: the acquisition really is the writer of these fields.
        assert_eq!(calls(INSTANT_SELL_SRC, "record_acquisition"), 1);
    }

    /// Moves no NFT, so it forms no custody CPI and never reads the stored standard. A transfer
    /// added here would move a card out of an escrow the pool still counts as a real position.
    #[test]
    fn it_forms_no_custody_cpi_and_never_reads_the_standard() {
        let prod = production(SRC);
        for needle in [
            "transfer_pnft",
            "transfer_spl",
            "release_from_vault",
            "release_core_from_vault",
            "position.standard",
        ] {
            assert_eq!(
                prod.matches(needle).count(),
                0,
                "activate_acquired references `{needle}` — it moves no asset"
            );
        }
    }

    /// One signer, and no payer among the accounts. Every account here already exists, so an
    /// `init`/`init_if_needed` would be an account this instruction created behind a name that
    /// says it only activates — and `payer =` is what a second, unreviewed rent charge looks
    /// like on the Operator's key.
    #[test]
    fn it_creates_no_account_and_charges_no_rent() {
        let prod = production(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        for needle in ["init,", "init_if_needed", "payer =", "space ="] {
            assert_eq!(
                prod.matches(needle).count(),
                0,
                "activate_acquired declares `{needle}` — it creates nothing"
            );
        }
    }

    /// Every PDA declares its seeds. A dropped `seeds =` turns a derived account into one the
    /// caller chooses freely, and Anchor reports nothing.
    #[test]
    fn every_pda_declares_its_seeds() {
        let prod = production(SRC);
        assert_eq!(prod.matches("seeds = [").count(), 5);
        assert!(prod.contains("seeds = [PROTOCOL_CONFIG_SEED]"));
        assert!(prod.contains("seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()]"));
        assert!(prod
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()]"));
        assert!(prod.contains("seeds = [TOP_TIER_SEED, pool.key().as_ref()]"));
        // Seeded on `position.depositor` and not on a supplied key: the account the wallet cap
        // reads must be the one the activation then credits, and the treasury inclusion is what
        // pins that depositor to this pool's T02 identity.
        assert!(prod.contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()]"
        ));
    }

    /// The `WeightIndex` is a bare keypair account, not a PDA: `AccountLoader` checks only owner
    /// and discriminator, so another pool's index would load cleanly. Only this address binding
    /// stops it, and `insert`'s desync guard cannot — it validates against the caller's own
    /// `expected_total`.
    #[test]
    fn the_weight_index_is_pinned_to_the_pools_own_account() {
        let prod = production(SRC);
        assert!(prod.contains("#[account(mut, address = pool.weight_index)]"));
        assert!(prod.contains("pub weight_index: AccountLoader<'info, WeightIndex>"));
    }

    /// The pause flag gates entry only, and this instruction is not entry — the card was
    /// accepted at `instant_sell`, which carries that gate. A pool that stops accepting deposits
    /// must still be able to finish activating what it already bought and paid for, or the
    /// position sits `Pending` over an escrow with no way out.
    #[test]
    fn it_is_not_gated_on_the_deposits_paused_flag() {
        let prod = production(SRC);
        assert_eq!(prod.matches("deposits_paused").count(), 0);
        // The control: the gate exists on the sell, so its absence here is deliberate.
        assert!(production(INSTANT_SELL_SRC).contains("!ctx.accounts.pool.deposits_paused"));
    }
}

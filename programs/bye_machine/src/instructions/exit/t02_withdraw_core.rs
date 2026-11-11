use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};
use mpl_core::ID as MPL_CORE_ID;

use crate::common::core_asset::{release_core_from_vault, require_collateral_settleable};
use crate::common::errors::ByeMachineError;
use crate::common::events::{RebalanceEvaluated, TierChanged, Withdrawn};
use crate::common::exit_accounting::apply_withdraw;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED,
    TOP_TIER_SEED, TREASURY_02_SEED, WALLET_STATS_SEED,
};
use crate::common::standards::STANDARD_CORE;
use crate::common::tier;
use crate::state::{Pool, Position, ProtocolConfig, TopTier, WalletStats, WeightIndex};

/// `withdraw`'s MPL-Core twin.
///
/// **The accounting is called, not restated.** `apply_withdraw` in `common/exit_accounting.rs`
/// (roll gate, lock, two-leg fee settle, four checked counter decrements, rebalance evaluation)
/// is the same function `withdraw` calls, so a second copy of the arithmetic would be two places
/// for the answer to what a depositor is paid. This file owns the custody leg and the account
/// set. The weight-freeze guard is carried through the shared accounting, where
/// `assert_weight_open` runs first of all.
///
/// **The collateral gate runs before `apply_withdraw`, one statement into the handler.** The fee
/// is paid out of the principal vault before the card is touched, so a gate next to the release
/// would return its named code after a USDC transfer had been built, and the depositor of a
/// burned card would get an opaque MPL Core revert instead of `CollateralAbsent` (6307) or
/// `CollateralFrozen` (6309) naming `close_seized`.
pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, T02WithdrawCore<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let t02_identity = ctx.accounts.t02_identity.key();
    let tier_size = ctx.accounts.pool.tier_size;
    let acc_tier = ctx.accounts.top_tier.acc_tier;
    let vault_bump = ctx.accounts.position.vault_bump;

    // First, before both of this handler's CPIs: the fee payout moves USDC before the card is
    // touched. The collection goes in because the freeze can live in its plugin registry rather
    // than the asset's; the gate pins it to the collection the asset declares.
    let collection = require_collateral_settleable(
        &ctx.accounts.asset,
        &ctx.accounts.collection,
        &ctx.accounts.position_vault.key(),
    )?;

    let effect = apply_withdraw(
        &mut ctx.accounts.pool,
        &mut ctx.accounts.position,
        &mut ctx.accounts.wallet_stats,
        acc_tier,
        now,
    )?;

    ctx.accounts.weight_index.load_mut()?.remove(
        effect.slot_index,
        effect.recorded_value,
        effect.expected_total,
    )?;

    let mut tier_change: Option<(Option<Pubkey>, Option<Pubkey>)> = None;
    if effect.was_member {
        tier::vacate(
            &mut ctx.accounts.top_tier,
            position_key,
            &mut ctx.accounts.position,
            acc_tier,
        )?;
        let entered = tier::fill_vacancy_from_candidate(
            &mut ctx.accounts.top_tier,
            tier_size,
            ctx.remaining_accounts,
            pool_key,
            acc_tier,
        )?;
        tier_change = Some((Some(position_key), entered));
    }

    let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();
    let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), &[ctx.accounts.pool.bump]];
    let pool_signer: &[&[&[u8]]] = &[pool_seeds];

    if effect.fees_paid > 0 {
        transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.principal_vault.to_account_info(),
                    to: ctx.accounts.treasury_03.to_account_info(),
                    authority: ctx.accounts.pool.to_account_info(),
                },
                pool_signer,
            ),
            effect.fees_paid,
        )?;
    }

    release_core_from_vault(
        &ctx.accounts.asset,
        collection,
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.position_vault.to_account_info(),
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &position_key,
        vault_bump,
    )?;

    emit!(Withdrawn {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        depositor: t02_identity,
        fees_paid: effect.fees_paid,
    });

    emit!(RebalanceEvaluated {
        pool: pool_key,
        slot: clock.slot,
        n_real: ctx.accounts.pool.n_real,
        w_real: ctx.accounts.pool.w_real,
        blanks_before: effect.blanks_before,
        blanks_after: ctx.accounts.pool.blanks,
    });

    if let Some((left, entered)) = tier_change {
        emit!(TierChanged {
            pool: pool_key,
            slot: clock.slot,
            left,
            entered,
        });
    }

    Ok(())
}

/// Signed by the T02 authority alone: the asset and the position's rent go to it, while the fee
/// payout goes to Treasury 03. An optional tier successor travels in `remaining_accounts` and is
/// read only when the closing position was a member. Account-for-account the principal's Core
/// twin, with the depositor wallet replaced by the authority and its USDC account by Treasury 03.
#[derive(Accounts)]
pub struct T02WithdrawCore<'info> {
    #[account(mut)]
    pub t02_authority: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    /// CHECK: the Treasury 02 identity PDA — an address, not an account.
    #[account(seeds = [TREASURY_02_SEED, pool.key().as_ref()], bump)]
    pub t02_identity: UncheckedAccount<'info>,

    #[account(
        mut,
        close = t02_authority,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == t02_identity.key() @ ByeMachineError::NotATreasuryPosition,
        // MPL Core only; on the accounts struct so it runs before any CPI (the fee payout moves
        // USDC before the card is touched).
        constraint = position.standard == STANDARD_CORE
            @ ByeMachineError::WrongStandardForInstruction
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

    #[account(
        mut,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), t02_identity.key().as_ref()],
        bump = wallet_stats.bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    /// Where the fee lands, pinned to the single address `Pool` records for Treasury 03.
    #[account(
        mut,
        address = pool.treasury_03,
        constraint = treasury_03.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted
    )]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    /// CHECK: the escrow identity, and **not an account** — its only on-chain expression is
    /// `AssetV1.owner`. Derived here so Anchor pins the address the collateral gate reads against
    /// and so the seeds the release signs under are this position's own. Never `init` and never
    /// `mut`: there is nothing to allocate and nothing to write.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    /// CHECK: **the position's own card, and this pin is the one the collateral gate cannot
    /// make.** `read_collateral` reads whatever account it is handed, so without this address a
    /// depositor could present a settleable asset of their own and withdraw a position whose
    /// card has been seized — taking the weight out and leaving the pool's own record of the
    /// seizure unwritten. Writable because `TransferV1` rewrites `AssetV1.owner`.
    #[account(mut, address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: pinned in the handler to the collection the asset itself declares. Required because
    /// MPL Core needs the collection account present to run its plugins over the transfer.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, production};

    const SRC: &str = include_str!("t02_withdraw_core.rs");

    #[test]
    fn the_t02_authority_is_the_only_signer_and_is_checked_against_config() {
        let prod = production(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains(
            "constraint = t02_authority.key() == protocol_config.t02_authority @ ByeMachineError::Unauthorized"
        ));
    }

    #[test]
    fn the_position_must_be_owned_by_the_t02_identity_and_its_rollup_is_the_identitys() {
        let prod = production(SRC);
        assert!(prod.contains(
            "constraint = position.depositor == t02_identity.key() @ ByeMachineError::NotATreasuryPosition"
        ));
        assert!(prod.contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), t02_identity.key().as_ref()]"
        ));
    }

    #[test]
    fn the_fee_goes_to_treasury_03_and_the_rent_to_the_authority() {
        let prod = production(SRC);
        assert_eq!(calls(SRC, "transfer"), 1);
        assert!(prod.contains("to: ctx.accounts.treasury_03.to_account_info(),"));
        assert!(prod.contains("close = t02_authority"));
        assert!(!accounts_body(SRC).contains("depositor_usdc"));
    }

    #[test]
    fn the_collateral_gate_runs_before_the_shared_accounting_and_the_asset_goes_to_the_authority() {
        let prod = production(SRC);
        let gate = prod.find("require_collateral_settleable(").unwrap();
        let accounting = prod.find("apply_withdraw(").unwrap();
        assert!(
            gate < accounting,
            "the fee payout must not precede the collateral gate"
        );
        assert_eq!(calls(SRC, "release_core_from_vault"), 1);
        assert!(prod.contains("position.standard == STANDARD_CORE"));
    }
}

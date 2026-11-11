use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions::ID as SYSVAR_INSTRUCTIONS_ID;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{
    close_account, transfer, CloseAccount, Mint, Token, TokenAccount, Transfer,
};
use mpl_token_metadata::ID as TOKEN_METADATA_ID;

use crate::common::errors::ByeMachineError;
use crate::common::escrow::{release_from_vault, PnftRelease};
use crate::common::events::{RebalanceEvaluated, TierChanged, Withdrawn};
use crate::common::exit_accounting::apply_withdraw;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PRINCIPAL_VAULT_SEED, PROTOCOL_CONFIG_SEED,
    TOP_TIER_SEED, TREASURY_02_SEED, WALLET_STATS_SEED,
};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::common::tier;
use crate::state::{Pool, Position, ProtocolConfig, TopTier, WalletStats, WeightIndex};

pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, T02Withdraw<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let t02_identity = ctx.accounts.t02_identity.key();
    let nft_mint = ctx.accounts.position.nft_mint;
    let tier_size = ctx.accounts.pool.tier_size;
    let acc_tier = ctx.accounts.top_tier.acc_tier;

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

    let position_seeds: &[&[u8]] = &[
        POSITION_SEED,
        pool_key.as_ref(),
        nft_mint.as_ref(),
        &[ctx.accounts.position.bump],
    ];
    let position_signer: &[&[&[u8]]] = &[position_seeds];

    release_from_vault(
        ctx.accounts.position.standard,
        &ctx.accounts.position_vault.to_account_info(),
        &ctx.accounts.position.to_account_info(),
        &ctx.accounts.authority_token,
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.nft_mint.to_account_info(),
        &ctx.accounts.metadata,
        &ctx.accounts.master_edition,
        &ctx.accounts.t02_authority.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.associated_token_program.to_account_info(),
        &ctx.accounts.token_metadata_program,
        PnftRelease {
            position_token_record: ctx.accounts.position_token_record.as_deref(),
            depositor_token_record: ctx.accounts.authority_token_record.as_deref(),
            sysvar_instructions: ctx.accounts.sysvar_instructions.as_deref(),
            authorization_rules_program: ctx.accounts.authorization_rules_program.as_deref(),
            authorization_rules: ctx.accounts.authorization_rules.as_deref(),
        },
        position_signer,
    )?;

    // The vault PDA is derived from the position, which the same seeds re-create on a
    // re-deposit — leaving an emptied token account behind would make that re-deposit
    // permanently unsatisfiable.
    close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        CloseAccount {
            account: ctx.accounts.position_vault.to_account_info(),
            destination: ctx.accounts.t02_authority.to_account_info(),
            authority: ctx.accounts.position.to_account_info(),
        },
        position_signer,
    ))?;

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

/// Signed by the T02 authority alone: it receives the card and both rent refunds, while the fee
/// payout goes to Treasury 03. An optional tier successor travels in `remaining_accounts` and is
/// read only when the closing position was a member.
#[derive(Accounts)]
pub struct T02Withdraw<'info> {
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
        // Token Metadata family only. On the accounts struct so it runs before any CPI: the fee
        // payout moves USDC out of the principal vault before the card is touched.
        constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)
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

    #[account(
        mut,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub nft_mint: Box<Account<'info, Mint>>,

    /// CHECK: created as the T02 authority's associated token account by `TransferV1` when empty,
    /// and checked against the T02 authority by that same processor when it already exists.
    #[account(mut)]
    pub authority_token: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    #[account(mut)]
    pub metadata: UncheckedAccount<'info>,

    /// CHECK: read only by Token Metadata's own `TransferV1` processor.
    pub master_edition: UncheckedAccount<'info>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Required
    /// present on standard 0 and absent on standard 1.
    #[account(mut)]
    pub position_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: derivation is asserted by Token Metadata's own `TransferV1` processor. Optional on
    /// the same terms as `position_token_record`.
    #[account(mut)]
    pub authority_token_record: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the Token Metadata program this pool's pNFTs are governed by.
    #[account(address = TOKEN_METADATA_ID)]
    pub token_metadata_program: UncheckedAccount<'info>,

    /// CHECK: optional, forwarded to `TransferV1`, which pins it to Token Auth Rules.
    pub authorization_rules_program: Option<UncheckedAccount<'info>>,

    /// CHECK: optional, forwarded to `TransferV1`, which asserts it matches the mint's rule set.
    pub authorization_rules: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned to the instructions sysvar `TransferV1` reads for its CPI guard. Optional on
    /// the same terms as the two token records — SPL `Transfer` has no CPI guard to read it.
    #[account(address = SYSVAR_INSTRUCTIONS_ID)]
    pub sysvar_instructions: Option<UncheckedAccount<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, calls, production};

    const SRC: &str = include_str!("t02_withdraw.rs");

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
    fn settles_through_the_shared_withdraw_accounting_and_reads_no_pause_flag() {
        let prod = production(SRC);
        assert_eq!(calls(SRC, "apply_withdraw"), 1);
        assert_eq!(prod.matches("deposits_paused").count(), 0);
        assert_eq!(prod.matches("rolls_paused").count(), 0);
    }

    #[test]
    fn restricts_the_position_to_the_token_metadata_family_before_any_cpi() {
        assert!(production(SRC)
            .contains("constraint = matches!(position.standard, STANDARD_PNFT | STANDARD_LEGACY)"));
    }
}

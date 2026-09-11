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
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PRINCIPAL_VAULT_SEED, TOP_TIER_SEED,
    WALLET_STATS_SEED,
};
use crate::common::standards::{STANDARD_LEGACY, STANDARD_PNFT};
use crate::common::tier;
use crate::state::{Pool, Position, TopTier, WalletStats, WeightIndex};

pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, Withdraw<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let depositor = ctx.accounts.depositor.key();
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
        let destination = ctx
            .accounts
            .depositor_usdc
            .as_ref()
            .ok_or(ErrorCode::AccountNotEnoughKeys)?;
        transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.principal_vault.to_account_info(),
                    to: destination.to_account_info(),
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
        &ctx.accounts.depositor_token,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.nft_mint.to_account_info(),
        &ctx.accounts.metadata,
        &ctx.accounts.master_edition,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.token_program.to_account_info(),
        &ctx.accounts.associated_token_program.to_account_info(),
        &ctx.accounts.token_metadata_program,
        PnftRelease {
            position_token_record: ctx.accounts.position_token_record.as_deref(),
            depositor_token_record: ctx.accounts.depositor_token_record.as_deref(),
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
            destination: ctx.accounts.depositor.to_account_info(),
            authority: ctx.accounts.position.to_account_info(),
        },
        position_signer,
    ))?;

    emit!(Withdrawn {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        depositor,
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

/// Depositor-signed: the NFT, the fee payout and both rent refunds all go
/// to the position's own depositor, who is also the only signer. An optional tier successor
/// travels in `remaining_accounts` and is read only when the closing position was a member.
#[derive(Accounts)]
pub struct Withdraw<'info> {
    #[account(mut)]
    pub depositor: Signer<'info>,

    #[account(
        mut,
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Box<Account<'info, Pool>>,

    #[account(
        mut,
        close = depositor,
        seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()],
        bump = position.bump,
        constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner,
        // This instruction serves the Token Metadata family only, and the assert is on the
        // accounts struct so it runs **before any CPI** — the fee payout moves USDC out of the
        // principal vault before this handler touches the card, so a handler-side check would
        // come after a transfer that had already happened.
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
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()],
        bump = wallet_stats.bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    #[account(
        mut,
        seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()],
        bump
    )]
    pub principal_vault: Box<Account<'info, TokenAccount>>,

    /// Optional so that withdrawal availability never depends on the depositor holding a USDC
    /// account: the payout is structurally zero today, and requiring the destination to exist
    /// for a transfer that never runs would gate an exit on something that has no permission to
    /// gate it. Required the moment the payout is non-zero.
    #[account(
        mut,
        constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner
    )]
    pub depositor_usdc: Option<Box<Account<'info, TokenAccount>>>,

    #[account(
        mut,
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump = position.vault_bump
    )]
    pub position_vault: Box<Account<'info, TokenAccount>>,

    #[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]
    pub nft_mint: Box<Account<'info, Mint>>,

    /// CHECK: created as the depositor's associated token account by `TransferV1` when empty,
    /// and checked against `depositor` by that same processor when it already exists.
    #[account(mut)]
    pub depositor_token: UncheckedAccount<'info>,

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
    pub depositor_token_record: Option<UncheckedAccount<'info>>,

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
    use super::*;
    use crate::common::constants::{DEFAULT_ADMISSION_FLOOR, FENWICK_CAPACITY};
    use crate::common::math::weight_of;
    use crate::common::test_support::{
        accounts_body, calls, field_writes, flat, is_assignment, production, writes,
    };

    const WITHDRAW_SRC: &str = include_str!("withdraw.rs");
    const EXIT_ACCOUNTING_SRC: &str = include_str!("../../common/exit_accounting.rs");

    // --- the account surface -------------------------------------------------------------------

    #[test]
    fn the_depositor_is_a_signer_bound_to_the_positions_own_depositor() {
        let prod = production(WITHDRAW_SRC);
        assert!(prod.contains("pub depositor: Signer<'info>"));
        // A pin reading `matches("UncheckedAccount<'info>> ").count() == 0` would be a
        // double `>` that can only ever match inside `Option<UncheckedAccount<'info>>`, so it
        // would constrain nothing about `depositor` and stay green under a `Signer` ->
        // `UncheckedAccount` substitution. `: Signer<'info>` occurring exactly once crate-wide
        // is `exactly_one_signer_per_handler` in `instructions/mod.rs`; this is that same form,
        // scoped to this file.
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(prod.contains(
            "constraint = position.depositor == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
    }

    #[test]
    fn reads_no_pause_flag() {
        assert_eq!(
            production(WITHDRAW_SRC).matches("deposits_paused").count(),
            0
        );
        assert_eq!(production(WITHDRAW_SRC).matches("rolls_paused").count(), 0);
    }

    #[test]
    fn every_state_account_is_boxed_and_every_pda_declares_its_seeds() {
        let prod = production(WITHDRAW_SRC);
        assert_eq!(prod.matches("Box<Account<").count(), 8);
        assert_eq!(prod.matches("seeds = [").count(), 6);
        assert!(prod.contains("#[account(mut, address = pool.weight_index)]"));
        assert!(prod.contains("pub weight_index: AccountLoader<'info, WeightIndex>"));
        assert!(prod.contains("seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()]"));
        assert!(prod
            .contains("seeds = [POSITION_SEED, pool.key().as_ref(), position.nft_mint.as_ref()]"));
        assert!(prod.contains("seeds = [TOP_TIER_SEED, pool.key().as_ref()]"));
        assert!(prod.contains(
            "seeds = [WALLET_STATS_SEED, pool.key().as_ref(), position.depositor.as_ref()]"
        ));
        assert!(prod.contains("seeds = [PRINCIPAL_VAULT_SEED, pool.key().as_ref()]"));
        assert!(prod.contains("seeds = [POSITION_VAULT_SEED, position.key().as_ref()]"));
    }

    /// `close = payer` in place of `close = depositor` on a sibling file was missed
    /// at baseline. The rent goes to the position's own depositor, nowhere else.
    #[test]
    fn the_position_closes_to_the_depositor_and_to_nobody_else() {
        let prod = production(WITHDRAW_SRC);
        assert_eq!(prod.matches("close = ").count(), 1);
        assert!(prod.contains("close = depositor,"));
        assert_eq!(prod.matches("close = payer").count(), 0);
        assert_eq!(prod.matches("close = signer").count(), 0);
    }

    // --- tier transitions, gated on membership ------------------------------------------------

    #[test]
    fn every_tier_transition_delegates_to_the_checked_tier_operations() {
        assert_eq!(calls(WITHDRAW_SRC, "tier::demote"), 0);
        assert_eq!(calls(WITHDRAW_SRC, "tier::promote"), 0);
        assert_eq!(calls(WITHDRAW_SRC, "tier::vacate"), 1);
        assert_eq!(calls(WITHDRAW_SRC, "tier::fill_vacancy_from_candidate"), 1);
        assert_eq!(calls(WITHDRAW_SRC, "tier::refresh_member"), 0);
        assert_eq!(calls(WITHDRAW_SRC, "tier::cross_into_tier"), 0);
        assert_eq!(calls(WITHDRAW_SRC, "settle_displaced_member"), 0);
    }

    /// A non-member exit vacates nothing, so a supplied candidate must be
    /// ignored rather than promoted into a `len < tier_size` gap — which would publish
    /// `TierChanged{left: None, entered: Some}` from an instruction that removed no member.
    /// The whole tier block, candidate included, sits inside the membership branch.
    #[test]
    fn the_optional_candidate_is_read_only_inside_the_membership_branch() {
        let prod = production(WITHDRAW_SRC);
        assert_eq!(prod.matches("ctx.remaining_accounts").count(), 1);
        assert_eq!(prod.matches("if effect.was_member {").count(), 1);
        let branch = prod.find("if effect.was_member {").unwrap();
        let candidate = prod.find("tier::fill_vacancy_from_candidate(").unwrap();
        let vacate = prod.find("tier::vacate(").unwrap();
        assert!(branch < vacate && vacate < candidate);
    }

    #[test]
    fn tier_changed_is_emitted_from_a_single_call_site_carrying_both_sides() {
        let prod = production(WITHDRAW_SRC);
        assert_eq!(prod.matches("emit!(TierChanged").count(), 1);
        assert_eq!(prod.matches("tier_change = Some(").count(), 1);
        assert!(flat(WITHDRAW_SRC).contains("tier_change = Some((Some(position_key), entered));"));
        assert!(flat(WITHDRAW_SRC).contains("if let Some((left, entered)) = tier_change { emit!(TierChanged { pool: pool_key, slot: clock.slot, left, entered, });"));
    }

    // --- the WeightIndex removal ---------------------------------------------------------------

    /// `remove` derives the leaf weight itself, but the caller still chooses which value and
    /// which aggregate it verifies against. Passing anything but the pre-close
    /// `recorded_value`, or the pre-decrement `w_real`, is a `WeightDesync` at best and a
    /// silent verification against the wrong leaf at worst.
    #[test]
    fn remove_receives_the_recorded_value_and_the_post_decrement_expected_total() {
        assert!(flat(WITHDRAW_SRC).contains(
            "ctx.accounts.weight_index.load_mut()?.remove( effect.slot_index, \
             effect.recorded_value, effect.expected_total, )?;"
        ));
        assert_eq!(calls(WITHDRAW_SRC, ".remove"), 1);
        assert_eq!(calls(WITHDRAW_SRC, ".insert"), 0);
        assert_eq!(calls(WITHDRAW_SRC, ".update"), 0);
    }

    // --- the Fenwick oracle and the allocator bound ---------------------------------------------

    fn boxed_zeroed_weight_index() -> Box<WeightIndex> {
        unsafe {
            let layout = std::alloc::Layout::new::<WeightIndex>();
            let ptr = std::alloc::alloc_zeroed(layout).cast::<WeightIndex>();
            assert!(!ptr.is_null(), "allocation failed");
            Box::from_raw(ptr)
        }
    }

    /// Independent of `fenwick_prefix` on purpose, so a fault in the production reader cannot
    /// hide behind the oracle meant to catch it.
    fn sum_of_leaves(tree: &[u64]) -> u64 {
        fn prefix(tree: &[u64], mut i: usize) -> u64 {
            let mut sum = 0u64;
            while i > 0 {
                sum += tree[i - 1];
                i &= i - 1;
            }
            sum
        }
        (1..=tree.len())
            .map(|i| prefix(tree, i) - prefix(tree, i - 1))
            .sum()
    }

    fn assert_allocator_bound(index: &WeightIndex) {
        assert!(
            index.free_count <= index.high_water,
            "free_count {} exceeds high_water {}",
            index.free_count,
            index.high_water
        );
        assert!(
            (index.high_water as usize) <= FENWICK_CAPACITY,
            "high_water {} exceeds capacity",
            index.high_water
        );
    }

    /// The exit path is the first production caller to reach `slot_release`, which moves
    /// `free_stack`/`free_count` — two fields with no writer and no bound otherwise stated
    /// anywhere in this crate. The bound is asserted after every op.
    #[test]
    fn removing_a_middle_leaf_recycles_one_slot_and_keeps_the_allocator_bound() {
        let mut index = boxed_zeroed_weight_index();
        let values = [
            DEFAULT_ADMISSION_FLOOR,
            DEFAULT_ADMISSION_FLOOR * 2,
            DEFAULT_ADMISSION_FLOOR * 3,
        ];
        let mut total = 0u64;
        let mut slots = Vec::new();
        for value in values {
            total += weight_of(value).unwrap();
            slots.push(index.insert(value, total).unwrap());
            assert_eq!(sum_of_leaves(&index.tree), total);
            assert_allocator_bound(&index);
        }
        assert_eq!(index.high_water, 3);
        assert_eq!(index.free_count, 0);

        total -= weight_of(values[1]).unwrap();
        index.remove(slots[1], values[1], total).unwrap();
        assert_eq!(index.total_weight, total);
        assert_eq!(sum_of_leaves(&index.tree), total);
        assert_eq!(index.free_count, 1);
        assert_eq!(index.high_water, 3, "a release never moves the frontier");
        assert_allocator_bound(&index);

        let recycled_value = DEFAULT_ADMISSION_FLOOR * 5;
        total += weight_of(recycled_value).unwrap();
        let recycled = index.insert(recycled_value, total).unwrap();
        assert_eq!(recycled, slots[1], "the released slot is reused");
        assert_eq!(index.total_weight, total);
        assert_eq!(sum_of_leaves(&index.tree), total);
        assert_eq!(index.free_count, 0);
        assert_eq!(index.high_water, 3);
        assert_allocator_bound(&index);
    }

    // --- the CPI surfaces ------------------------------------------------------------------------

    /// `release_from_vault` takes 13 positional `&AccountInfo` of identical type, so any swap
    /// compiles and nothing else in the tree distinguishes a correct call from a permuted one.
    /// The direction is vault → depositor: the first account is the position vault and the
    /// `depositor` slot is where the card lands. The five pNFT accounts are not in that count —
    /// `PnftRelease`'s named fields cannot be transposed.
    #[test]
    fn the_release_arguments_are_pinned_in_order_and_move_vault_to_depositor() {
        assert!(flat(WITHDRAW_SRC).contains(
            "release_from_vault( ctx.accounts.position.standard, \
             &ctx.accounts.position_vault.to_account_info(), \
             &ctx.accounts.position.to_account_info(), &ctx.accounts.depositor_token, \
             &ctx.accounts.depositor.to_account_info(), &ctx.accounts.nft_mint.to_account_info(), \
             &ctx.accounts.metadata, &ctx.accounts.master_edition, \
             &ctx.accounts.depositor.to_account_info(), \
             &ctx.accounts.system_program.to_account_info(), \
             &ctx.accounts.token_program.to_account_info(), \
             &ctx.accounts.associated_token_program.to_account_info(), \
             &ctx.accounts.token_metadata_program, PnftRelease { \
             position_token_record: ctx.accounts.position_token_record.as_deref(), \
             depositor_token_record: ctx.accounts.depositor_token_record.as_deref(), \
             sysvar_instructions: ctx.accounts.sysvar_instructions.as_deref(), \
             authorization_rules_program: ctx.accounts.authorization_rules_program.as_deref(), \
             authorization_rules: ctx.accounts.authorization_rules.as_deref(), }, \
             position_signer, )?;"
        ));
        assert_eq!(calls(WITHDRAW_SRC, "release_from_vault"), 1);
        assert_eq!(
            calls(WITHDRAW_SRC, "transfer_pnft"),
            0,
            "the branch is the only route to a transfer; a direct call would skip the standard"
        );
    }

    #[test]
    fn the_position_signs_the_escrow_return_under_its_own_exact_seeds() {
        assert!(flat(WITHDRAW_SRC).contains(
            "let position_seeds: &[&[u8]] = &[ POSITION_SEED, pool_key.as_ref(), \
             nft_mint.as_ref(), &[ctx.accounts.position.bump], ];"
        ));
        assert!(
            flat(WITHDRAW_SRC).contains("let position_signer: &[&[&[u8]]] = &[position_seeds];")
        );
    }

    /// The vault PDA is re-derived by a later re-deposit, so an emptied account left
    /// behind makes that re-deposit permanently unsatisfiable.
    #[test]
    fn the_nft_vault_is_closed_to_the_depositor_under_the_position_authority() {
        assert!(flat(WITHDRAW_SRC).contains(
            "CloseAccount { account: ctx.accounts.position_vault.to_account_info(), \
             destination: ctx.accounts.depositor.to_account_info(), \
             authority: ctx.accounts.position.to_account_info(), }, position_signer, ))?;"
        ));
        assert_eq!(calls(WITHDRAW_SRC, "close_account"), 1);
    }

    /// The payout account surface is declared and the delta computed on every
    /// path, but the CPI is skipped at zero — a zero-amount SPL transfer still requires the
    /// destination to exist, and nothing may gate withdrawal availability.
    #[test]
    fn the_fee_payout_moves_principal_vault_to_the_depositor_and_is_skipped_at_zero() {
        assert!(flat(WITHDRAW_SRC).contains(
            "if effect.fees_paid > 0 { let destination = ctx .accounts .depositor_usdc .as_ref() \
             .ok_or(ErrorCode::AccountNotEnoughKeys)?; transfer( CpiContext::new_with_signer( \
             ctx.accounts.token_program.to_account_info(), Transfer { \
             from: ctx.accounts.principal_vault.to_account_info(), \
             to: destination.to_account_info(), \
             authority: ctx.accounts.pool.to_account_info(), }, pool_signer, ), \
             effect.fees_paid, )?; }"
        ));
        assert_eq!(
            accounts_body(WITHDRAW_SRC).matches("Option<").count(),
            6,
            "the payout destination, the two authorization-rules accounts, and three more: \
             both token records and sysvar_instructions, required-absent on a legacy release"
        );
        assert!(accounts_body(WITHDRAW_SRC)
            .contains("pub depositor_usdc: Option<Box<Account<'info, TokenAccount>>>"));
        assert!(flat(WITHDRAW_SRC).contains(
            "let pool_seeds: &[&[u8]] = &[POOL_SEED, pool_id_bytes.as_ref(), \
             &[ctx.accounts.pool.bump]];"
        ));
        assert_eq!(calls(WITHDRAW_SRC, "transfer"), 1);
    }

    // --- single exit, obligations, absences -----------------------------------------------------

    fn handler_body(source: &str) -> &str {
        let prod = production(source);
        let start = prod.find("pub fn handler").unwrap();
        let rest = &prod[start..];
        let end = rest.find("\n/// Depositor-signed").unwrap();
        &rest[..end]
    }

    #[test]
    fn handler_has_exactly_one_ok_and_zero_return_statements() {
        let body = handler_body(WITHDRAW_SRC);
        assert_eq!(body.matches("Ok(())").count(), 1);
        assert_eq!(body.matches("return ").count(), 0);
    }

    /// The counter and fee obligations live in `common/exit_accounting.rs`, where
    /// `every_path_obligation_appears_exactly_once` now asserts them. What is left here is the
    /// half this file still owns — the two emits and the single call that reaches the
    /// accounting — plus the one claim the move creates: a **second** `apply_withdraw` call
    /// would settle the fees and decrement every counter twice, which no pin in the accounting
    /// module can see.
    #[test]
    fn every_path_obligation_appears_exactly_once_at_the_top_level() {
        let prod = production(WITHDRAW_SRC);
        assert_eq!(calls(WITHDRAW_SRC, "apply_withdraw"), 1);
        assert_eq!(prod.matches("emit!(Withdrawn").count(), 1);
        assert_eq!(prod.matches("emit!(RebalanceEvaluated").count(), 1);
        assert_eq!(calls(EXIT_ACCOUNTING_SRC, "position_fee_payout"), 2);
        assert_eq!(calls(EXIT_ACCOUNTING_SRC, "rebalance::evaluate"), 1);
    }

    /// `fees_paid` is published from the value the settle returned, not re-read off a field the
    /// close is about to delete. Both mutations the two payloads report — the settle and the
    /// blank write — happen inside `apply_withdraw`, so the ordering is anchored on that call
    /// rather than on separate statements in this file.
    #[test]
    fn withdrawn_publishes_the_returned_payout_and_every_emit_follows_its_mutation() {
        let prod = production(WITHDRAW_SRC);
        assert!(prod.contains("fees_paid: effect.fees_paid,"));
        let apply = prod.find("apply_withdraw(").unwrap();
        let rebalance_emit = prod.find("emit!(RebalanceEvaluated").unwrap();
        let withdrawn_emit = prod.find("emit!(Withdrawn").unwrap();
        assert!(apply < withdrawn_emit);
        assert!(withdrawn_emit < rebalance_emit, "Withdrawn leads, matching its declared order");
        // And the accounting module's own case orders the two mutations against each other:
        // the blank write precedes the aggregate the removal verifies against.
        let acc = production(EXIT_ACCOUNTING_SRC);
        assert!(
            acc.find("let fees_paid = position_fee_payout").unwrap()
                < acc.find("pool.blanks = [targets.b1").unwrap()
        );
    }

    /// `recorded_value` and `slot_index` have their writers elsewhere. This handler
    /// reads both and writes neither — a write here would silently re-home a live leaf.
    #[test]
    fn neither_recorded_value_nor_slot_index_is_written_here() {
        assert_eq!(writes(WITHDRAW_SRC, "position.recorded_value"), 0);
        assert_eq!(writes(WITHDRAW_SRC, "position.slot_index"), 0);
        assert_eq!(writes(WITHDRAW_SRC, "position.state"), 0);
    }

    #[test]
    fn no_sweep_state_is_touched() {
        let prod = production(WITHDRAW_SRC);
        for field in [
            "sweep_pending",
            "sweep_epoch",
            "sweep_updates",
            "last_sweep_at",
        ] {
            assert_eq!(
                prod.matches(field).count(),
                0,
                "{field} is not this handler's"
            );
        }
    }

    /// The crate-wide `mut`-declaration enumerator cannot reach one sub-class: an account
    /// written **only by a callee** through `.to_account_info()` is never mutated Rust-side, so
    /// dropping its `mut` leaves that test green — `position_vault`, `principal_vault`,
    /// `depositor_usdc`, `depositor_token` and both token records are all that shape here. The
    /// five Rust-side accounts are covered by the enumerator (and `position` louder still:
    /// Anchor's macro rejects `close` without `mut` at compile time). This pins the rest by
    /// exact count, so deleting any single one fails.
    #[test]
    fn every_writable_account_declaration_is_pinned_by_count() {
        let accounts = accounts_body(WITHDRAW_SRC);
        assert_eq!(accounts.matches("mut").count(), 13);
        for declaration in [
            "#[account(mut)]\n    pub depositor: Signer<'info>",
            "#[account(mut, address = pool.weight_index)]",
            "#[account(mut)]\n    pub depositor_token: UncheckedAccount<'info>",
            "#[account(mut)]\n    pub metadata: UncheckedAccount<'info>",
            "#[account(mut)]\n    pub position_token_record: Option<UncheckedAccount<'info>>",
            "#[account(mut)]\n    pub depositor_token_record: Option<UncheckedAccount<'info>>",
        ] {
            assert!(
                accounts.contains(declaration),
                "missing writable declaration: {declaration}"
            );
        }
        for pda in [
            "mut,\n        seeds = [PRINCIPAL_VAULT_SEED",
            "mut,\n        seeds = [POSITION_VAULT_SEED",
            "mut,\n        seeds = [TOP_TIER_SEED",
            "mut,\n        seeds = [WALLET_STATS_SEED",
            "mut,\n        seeds = [POOL_SEED",
            "mut,\n        close = depositor,",
            "mut,\n        constraint = depositor_usdc.mint",
        ] {
            assert!(accounts.contains(pda), "missing writable PDA: {pda}");
        }
    }
    // --- the seam between the handler and `apply_withdraw` -----------------------------------

    /// `apply_withdraw` takes `acc_tier` and `now` as *parameters*, so a value read out of the
    /// wrong account here is invisible to every pure-unit case, and all three fee accumulators
    /// are zero today — a blind spot reached through the seam rather than through the
    /// arithmetic, which a behavioural test alone would not catch (`acc_tier` sourced from
    /// `pool.acc_equal` instead of `top_tier.acc_tier` among the ways it can go wrong).
    #[test]
    fn every_handler_prologue_binding_is_sourced_from_its_own_account_and_field() {
        let flat = flat(WITHDRAW_SRC);
        for binding in [
            "let clock = Clock::get()?;",
            "let now = clock.unix_timestamp;",
            "let pool_key = ctx.accounts.pool.key();",
            "let position_key = ctx.accounts.position.key();",
            "let depositor = ctx.accounts.depositor.key();",
            "let nft_mint = ctx.accounts.position.nft_mint;",
            "let tier_size = ctx.accounts.pool.tier_size;",
            "let acc_tier = ctx.accounts.top_tier.acc_tier;",
        ] {
            assert!(flat.contains(binding), "mis-sourced or missing: {binding}");
        }

        let body = handler_body(WITHDRAW_SRC);
        let prologue = &body[..body.find("let effect = apply_withdraw").unwrap()];
        assert_eq!(
            prologue.matches("    let ").count(),
            8,
            "a binding was added to or removed from the prologue"
        );
        assert!(flat.contains("let pool_id_bytes = ctx.accounts.pool.pool_id.to_le_bytes();"));
    }

    /// `TierChanged` was pinned field by field and `Withdrawn`/`RebalanceEvaluated` were not,
    /// which left several payload mis-sourcings undetected — including `blanks_before ==
    /// blanks_after`, which leaves the event structurally well-formed and semantically empty for
    /// a decoder reading it.
    #[test]
    fn the_withdrawn_and_rebalance_payloads_are_pinned_field_by_field() {
        let flat = flat(WITHDRAW_SRC);
        assert!(flat.contains(
            "emit!(Withdrawn { pool: pool_key, slot: clock.slot, position: position_key, \
             depositor, fees_paid: effect.fees_paid, });"
        ));
        assert!(flat.contains(
            "emit!(RebalanceEvaluated { pool: pool_key, slot: clock.slot, \
             n_real: ctx.accounts.pool.n_real, w_real: ctx.accounts.pool.w_real, \
             blanks_before: effect.blanks_before, blanks_after: ctx.accounts.pool.blanks, });"
        ));
    }

    /// Ruling 11 makes the destination optional so nothing gates exit availability; the two
    /// constraints are what stop an optional account from being *any* token account. Deleting
    /// the ownership half alone was missed at baseline.
    #[test]
    fn the_payout_destination_is_bound_to_the_depositor_and_to_the_vaults_mint() {
        let accounts = accounts_body(WITHDRAW_SRC);
        assert!(accounts.contains(
            "constraint = depositor_usdc.mint == principal_vault.mint @ ByeMachineError::StandardNotAdmitted"
        ));
        assert!(accounts.contains(
            "constraint = depositor_usdc.owner == depositor.key() @ ByeMachineError::NotPositionOwner"
        ));
        // Four, not three: the `standard in {0,1}` guard is an accounts-struct constraint so
        // that it runs before the fee payout above, which is a CPI this handler makes before it
        // ever touches the card.
        assert_eq!(accounts.matches("constraint = ").count(), 4);
    }

    /// `token_metadata_program` is the program `transfer_pnft` builds its CPI against while the
    /// position PDA signs, so an unpinned address hands a caller-supplied program the position's
    /// signature. All six of these deletions were missed at baseline; the deposit domain pins
    /// the same two programs and the exit domain did not.
    #[test]
    fn every_address_pinned_account_carries_its_exact_address() {
        let accounts = accounts_body(WITHDRAW_SRC);
        for pin in [
            "#[account(mut, address = pool.weight_index)]",
            "#[account(address = position.nft_mint @ ByeMachineError::StandardNotAdmitted)]",
            "#[account(address = TOKEN_METADATA_ID)]",
            "#[account(address = SYSVAR_INSTRUCTIONS_ID)]",
        ] {
            assert!(accounts.contains(pin), "missing address pin: {pin}");
        }
        assert_eq!(accounts.matches("address = ").count(), 4);
    }

    /// The vault is closed once it is empty. Closing it while it still holds the pNFT
    /// fails the SPL non-empty check, so the order is load-bearing and nothing asserted it.
    #[test]
    fn the_custody_release_precedes_the_vault_close() {
        let body = handler_body(WITHDRAW_SRC);
        assert!(body.find("release_from_vault(").unwrap() < body.find("close_account(").unwrap());
    }

    /// **This handler writes no state field at all.** Every write — `pool` × 4, `position` × 3,
    /// `wallet_stats` × 2 — lives in `common/exit_accounting.rs` beside the writes
    /// themselves. What cannot be seen from there is a write this file adds *outside* the
    /// shared accounting: `Pool.open_batches = 0` (releasing the weight freeze),
    /// `Pool.acc_equal = 0` (wiping the fee accumulator) and `WalletStats.bump = 0` (bricking
    /// the PDA) would each be exactly this kind of stray write. An empty set
    /// refuses all three by construction rather than by enumeration.
    #[test]
    fn the_handler_writes_exactly_the_fields_it_owns_and_no_others() {
        for receiver in [
            "pool",
            "position",
            "wallet_stats",
            "ctx.accounts.pool",
            "ctx.accounts.position",
            "ctx.accounts.wallet_stats",
            "ctx.accounts.top_tier",
        ] {
            assert!(
                field_writes(WITHDRAW_SRC, receiver).is_empty(),
                "{receiver} is written directly; every state write is apply_withdraw's"
            );
        }
        // The control: the set this file no longer writes is written in exactly one place.
        assert_eq!(
            field_writes(EXIT_ACCOUNTING_SRC, "pool"),
            ["blanks", "n_real", "owed_fees", "w_real"]
        );
    }
    /// The tokenisation boundaries [`is_assignment`] has to get right. Its count control — the
    /// figures a widened predicate would move — followed the writes to
    /// `common/exit_accounting.rs`'s `every_pinned_write_count_survives_the_move`; a control
    /// over this file's remaining zeros would be a control over nothing. `<<=`/`>>=` share a
    /// prefix with `<`/`>` and `<=`/`>=` sit between them, so a prefix-matched predicate either
    /// misses the shifts or counts the comparisons. `position.state == ...` reaching this as a
    /// write is exactly the fault a prefix-matched predicate produces.
    #[test]
    fn is_assignment_separates_every_assignment_operator_from_every_comparison() {
        for assignment in [
            " = 0;", "= 0;", " += 1", " -= 1", " *= 2", " /= 2", " %= 2", " |= 1", " &= 1",
            " ^= 1", " <<= 1", " >>= 1",
        ] {
            assert!(
                is_assignment(assignment),
                "missed an assignment: {assignment:?}"
            );
        }
        for comparison in [
            " == 0",
            "== 0",
            " != 0",
            " <= 0",
            " >= 0",
            " < 0",
            " > 0",
            ", 0",
            "; ",
            ")?",
            ".key()",
            " => Ok(())",
            "=> 6300,",
        ] {
            assert!(
                !is_assignment(comparison),
                "counted a non-write: {comparison:?}"
            );
        }
    }
}

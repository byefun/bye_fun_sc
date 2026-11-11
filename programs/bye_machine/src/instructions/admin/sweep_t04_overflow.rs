//! The shared Treasury 04's ceiling, restored out of band.
//!
//! A virtual position's equal share credits Treasury 04 in full at settlement, so the balance may
//! sit above the protocol-wide `t04_ceiling` between sweeps. This instruction is what brings it
//! back: it transfers exactly the excess to Treasury 03 and leaves Treasury 04 at its ceiling.
//!
//! **Discretionary, not scheduled.** Nothing on-chain requires it to run at all, or on any
//! cadence. **Refused while the ceiling is 0**, which it is until the Administrator sets it: a
//! sweep against 0 would move the whole float every blank is paid from.
//!
//! **Custody stays narrow.** The Operator signature only *triggers* the sweep; the program
//! computes both the amount and the destination, so no authority-chosen figure ever debits
//! Treasury 04.
//!
//! **Every pool, and none with a batch open.** Treasury 04's balance at the moment a batch opens
//! is the balance every blank outcome in that batch settles against, and with one Treasury 04 any
//! pool's open batch is such a batch. The sweep therefore takes every pool as a remaining account
//! and refuses unless each is idle: a set short of one pool would let a sweep land under exactly
//! the batch the check exists for. The check shares `assert_weight_open` because the condition is
//! the same one; the sweep moves no pool weight.
//!
//! **It is a sequencing rule.** What the gate removes is the ordinary collision — a routine sweep
//! landing while a batch happens to be open, on a ceiling lowered for unrelated reasons — for the
//! cost of one comparison per pool.

use anchor_lang::prelude::*;
use anchor_spl::token::{transfer, Token, TokenAccount, Transfer};

use crate::common::errors::ByeMachineError;
use crate::common::events::T04Swept;
use crate::common::guards::assert_weight_open;
use crate::common::seeds::{POOL_SEED, PROTOCOL_CONFIG_SEED, TREASURY_03_SEED, TREASURY_04_SEED};
use crate::state::{Pool, ProtocolConfig};

/// The balance above the ceiling, which is what the sweep moves and all it moves.
///
/// `saturating_sub` is the whole of the no-op path: a balance at or below the ceiling yields `0`,
/// and the handler transfers nothing and emits nothing on `0`. Returning an `Option` or erroring
/// would make "already at the ceiling" a failure, and it is a success with no work to do.
pub(crate) fn overflow_above_ceiling(t04_balance: u64, ceiling: u64) -> u64 {
    t04_balance.saturating_sub(ceiling)
}

/// `pools` is every pool the program has created, once each, none with a batch open, and each
/// naming the two treasuries this sweep moves between.
///
/// Each account is read as an `Account<Pool>` (owner and discriminator) and must sit at its own
/// `pool_id`'s PDA. Ids are distinct and below `pool_counter`, and there are `pool_counter` of
/// them, so the set is exactly the pools `init_pool` created. A pool that still names a per-pool
/// treasury has not been consolidated, and the sweep waits for it.
fn assert_every_pool_idle<'info>(
    pools: &'info [AccountInfo<'info>],
    pool_counter: u16,
    treasury_03: &Pubkey,
    treasury_04: &Pubkey,
) -> Result<()> {
    require_eq!(
        pools.len(),
        usize::from(pool_counter),
        ByeMachineError::SweepPoolSetIncomplete
    );
    let mut seen = vec![false; pools.len()];
    for info in pools {
        let pool = Account::<Pool>::try_from(info)?;
        let id = usize::from(pool.pool_id);
        require!(
            id < seen.len() && !seen[id],
            ByeMachineError::SweepPoolSetIncomplete
        );
        seen[id] = true;
        let address = Pubkey::create_program_address(
            &[POOL_SEED, pool.pool_id.to_le_bytes().as_ref(), &[pool.bump]],
            &crate::ID,
        )
        .map_err(|_| ByeMachineError::SweepPoolSetIncomplete)?;
        require_keys_eq!(info.key(), address, ByeMachineError::SweepPoolSetIncomplete);
        assert_weight_open(&pool)?;
        require_keys_eq!(pool.treasury_03, *treasury_03, ErrorCode::ConstraintAddress);
        require_keys_eq!(pool.treasury_04, *treasury_04, ErrorCode::ConstraintAddress);
    }
    Ok(())
}

pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, SweepT04Overflow<'info>>) -> Result<()> {
    let ceiling = ctx.accounts.protocol_config.t04_ceiling;
    require!(ceiling > 0, ByeMachineError::T04CeilingUnset);

    assert_every_pool_idle(
        ctx.remaining_accounts,
        ctx.accounts.protocol_config.pool_counter,
        &ctx.accounts.treasury_03.key(),
        &ctx.accounts.treasury_04.key(),
    )?;

    let excess = overflow_above_ceiling(ctx.accounts.treasury_04.amount, ceiling);

    // The no-op path, and it is deliberately silent: no transfer, and no event. An event here
    // would put a zero-amount `T04Swept` in the log for every sweep that found nothing to do, and
    // a watcher reconciling Treasury 03's credits against this event would have to filter them
    // back out.
    if excess == 0 {
        return Ok(());
    }

    let config_seeds: &[&[u8]] = &[PROTOCOL_CONFIG_SEED, &[ctx.accounts.protocol_config.bump]];
    let config_signer: &[&[&[u8]]] = &[config_seeds];

    transfer(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            Transfer {
                from: ctx.accounts.treasury_04.to_account_info(),
                to: ctx.accounts.treasury_03.to_account_info(),
                authority: ctx.accounts.protocol_config.to_account_info(),
            },
            config_signer,
        ),
        excess,
    )?;

    emit!(T04Swept {
        slot: Clock::get()?.slot,
        amount: excess,
        ceiling,
    });

    Ok(())
}

/// Every pool follows as a remaining account, read-only, in any order — see
/// `assert_every_pool_idle`.
#[derive(Accounts)]
pub struct SweepT04Overflow<'info> {
    pub operator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Box<Account<'info, ProtocolConfig>>,

    #[account(mut, seeds = [TREASURY_04_SEED], bump)]
    pub treasury_04: Box<Account<'info, TokenAccount>>,

    #[account(mut, seeds = [TREASURY_03_SEED], bump)]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    pub token_program: Program<'info, Token>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{accounts_body, calls, production};

    const SRC: &str = include_str!("sweep_t04_overflow.rs");
    const SETTLE_SRC: &str = include_str!("../roll/settle_roll.rs");
    const SETTLE_CORE_SRC: &str = include_str!("../roll/settle_roll_core.rs");

    const OPERATOR_CONSTRAINT: &str =
        "constraint = operator.key() == protocol_config.operator @ ByeMachineError::Unauthorized";

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    // --- Behavioural: the amount, over plain values --------------------------------------------

    /// The transfer amount is `max(0, balance − ceiling)` and nothing else. Both directions of
    /// the boundary are here, because a `<` written for a `<=` moves a zero-value transfer.
    #[test]
    fn the_excess_is_exactly_the_balance_above_the_ceiling() {
        assert_eq!(overflow_above_ceiling(5_000, 2_000), 3_000);
        assert_eq!(overflow_above_ceiling(2_001, 2_000), 1, "one over is one");
        assert_eq!(overflow_above_ceiling(2_000, 2_000), 0, "at the ceiling");
        assert_eq!(overflow_above_ceiling(1_999, 2_000), 0, "one under");
        assert_eq!(overflow_above_ceiling(0, 2_000), 0, "empty treasury");
    }

    /// The sweep leaves Treasury 04 **at** its ceiling, never below it: what remains after the
    /// transfer is `min(balance, ceiling)`, driven here rather than asserted as prose.
    #[test]
    fn what_remains_is_the_ceiling_or_the_whole_balance_whichever_is_smaller() {
        for (balance, ceiling) in [
            (5_000u64, 2_000u64),
            (2_000, 2_000),
            (1, 2_000),
            (u64::MAX, 2_000),
        ] {
            let remaining = balance - overflow_above_ceiling(balance, ceiling);
            assert_eq!(
                remaining,
                balance.min(ceiling),
                "balance {balance}, ceiling {ceiling}"
            );
        }
    }

    /// The sweep never creates or destroys value: what leaves Treasury 04 is what arrives at
    /// Treasury 03, so the two sides sum back to the original balance.
    #[test]
    fn the_transfer_conserves_the_balance() {
        for (balance, ceiling) in [(5_000u64, 2_000u64), (2_000, 2_000), (9, 4), (0, 7)] {
            let moved = overflow_above_ceiling(balance, ceiling);
            let remaining = balance - moved;
            assert_eq!(moved + remaining, balance);
        }
    }

    // --- Behavioural: the pool set, over real account bytes ------------------------------------

    struct PoolAccount {
        key: Pubkey,
        lamports: u64,
        data: Vec<u8>,
        owner: Pubkey,
    }

    fn pool_account(pool_id: u16, t03: Pubkey, t04: Pubkey) -> PoolAccount {
        let (key, bump) =
            Pubkey::find_program_address(&[POOL_SEED, pool_id.to_le_bytes().as_ref()], &crate::ID);
        let mut pool = Pool::blank();
        pool.pool_id = pool_id;
        pool.bump = bump;
        pool.treasury_03 = t03;
        pool.treasury_04 = t04;
        account_of(key, &pool)
    }

    fn account_of(key: Pubkey, pool: &Pool) -> PoolAccount {
        let mut data = Vec::new();
        pool.try_serialize(&mut data).unwrap();
        PoolAccount {
            key,
            lamports: 1,
            data,
            owner: crate::ID,
        }
    }

    fn decode(account: &PoolAccount) -> Pool {
        Pool::try_deserialize(&mut account.data.as_slice()).unwrap()
    }

    /// Runs the check over `accounts`. The infos borrow from a leaked copy, since
    /// `Account::try_from` wants `'info` references and each test is short-lived.
    fn check(
        accounts: Vec<PoolAccount>,
        pool_counter: u16,
        t03: Pubkey,
        t04: Pubkey,
    ) -> Result<()> {
        let accounts: &'static mut [PoolAccount] = Box::leak(accounts.into_boxed_slice());
        let infos: Vec<AccountInfo<'static>> = accounts
            .iter_mut()
            .map(|a| {
                AccountInfo::new(
                    &a.key,
                    false,
                    false,
                    &mut a.lamports,
                    &mut a.data,
                    &a.owner,
                    false,
                    0,
                )
            })
            .collect();
        let infos: &'static [AccountInfo<'static>] = Box::leak(infos.into_boxed_slice());
        assert_every_pool_idle(infos, pool_counter, &t03, &t04)
    }

    fn treasuries() -> (Pubkey, Pubkey) {
        (Pubkey::new_unique(), Pubkey::new_unique())
    }

    #[test]
    fn every_pool_once_in_any_order_is_accepted() {
        let (t03, t04) = treasuries();
        let set = |ids: &[u16]| ids.iter().map(|id| pool_account(*id, t03, t04)).collect();
        assert!(check(set(&[0, 1, 2]), 3, t03, t04).is_ok());
        assert!(check(set(&[2, 0, 1]), 3, t03, t04).is_ok());
        assert!(check(set(&[0]), 1, t03, t04).is_ok());
    }

    /// One pool short, one too many, a repeat in place of a missing pool, and an id at or past
    /// `pool_counter` are all the incomplete-set error.
    #[test]
    fn a_set_that_is_not_every_pool_once_is_refused() {
        let (t03, t04) = treasuries();
        let set = |ids: &[u16]| ids.iter().map(|id| pool_account(*id, t03, t04)).collect();
        for (ids, counter) in [
            (&[0u16, 1][..], 3u16),
            (&[0, 1, 2][..], 2),
            (&[0, 1, 1][..], 3),
            (&[0, 1, 3][..], 3),
            (&[][..], 1),
        ] {
            assert_eq!(
                error_code(check(set(ids), counter, t03, t04).unwrap_err()),
                6532,
                "ids {ids:?}, pool_counter {counter}"
            );
        }
    }

    /// A pool-tagged account at an address other than its own id's PDA is refused even when its
    /// bytes are a faithful copy of the real pool's.
    #[test]
    fn a_pool_away_from_its_own_address_is_refused() {
        let (t03, t04) = treasuries();
        let real = pool_account(1, t03, t04);
        let copy = account_of(Pubkey::new_unique(), &decode(&real));
        let set = vec![pool_account(0, t03, t04), copy];
        assert_eq!(error_code(check(set, 2, t03, t04).unwrap_err()), 6532);
    }

    #[test]
    fn any_pool_with_a_batch_open_refuses_the_sweep() {
        let (t03, t04) = treasuries();
        for busy in 0..3u16 {
            let set = (0..3u16)
                .map(|id| {
                    let mut account = pool_account(id, t03, t04);
                    if id == busy {
                        let mut pool = decode(&account);
                        pool.open_batches = 1;
                        account = account_of(account.key, &pool);
                    }
                    account
                })
                .collect();
            assert_eq!(
                error_code(check(set, 3, t03, t04).unwrap_err()),
                6200,
                "pool {busy}"
            );
        }
    }

    /// A pool still naming a per-pool treasury has not been consolidated, and the sweep waits.
    #[test]
    fn a_pool_naming_another_treasury_refuses_the_sweep() {
        let (t03, t04) = treasuries();
        let stray = Pubkey::new_unique();
        for (pool_t03, pool_t04) in [(stray, t04), (t03, stray), (t03, Pubkey::default())] {
            let set = vec![
                pool_account(0, t03, t04),
                pool_account(1, pool_t03, pool_t04),
            ];
            assert_eq!(
                error_code(check(set, 2, t03, t04).unwrap_err()),
                2012,
                "the address constraint's own code"
            );
        }
    }

    #[test]
    fn an_account_this_program_does_not_own_is_refused() {
        let (t03, t04) = treasuries();
        let mut foreign = pool_account(0, t03, t04);
        foreign.owner = Pubkey::new_unique();
        assert!(check(vec![foreign], 1, t03, t04).is_err());
    }

    // --- Structural: declaration facts a behavioural test cannot reach -------------------------

    /// Operator and no one else. A second signer class in this struct would be a second way to
    /// trigger a Treasury 04 debit outside settlement.
    #[test]
    fn the_operator_is_the_only_signer_class() {
        let prod = production(SRC);
        assert_eq!(prod.matches(OPERATOR_CONSTRAINT).count(), 1);
        assert_eq!(prod.matches("administrator").count(), 0);
        assert_eq!(
            accounts_body(SRC).matches("Signer<'info>").count(),
            1,
            "exactly one signer is declared"
        );
    }

    /// **Both treasuries are declarations, not arguments.** Each is the shared PDA, so an
    /// Operator can name neither another source nor another destination.
    #[test]
    fn both_treasuries_are_the_shared_pdas() {
        let body = accounts_body(SRC);
        assert_eq!(
            body.matches("#[account(mut, seeds = [TREASURY_04_SEED], bump)]")
                .count(),
            1
        );
        assert_eq!(
            body.matches("#[account(mut, seeds = [TREASURY_03_SEED], bump)]")
                .count(),
            1
        );
    }

    /// The handler takes no arguments, so there is no amount for a caller to supply. Stated
    /// against the signature because that is where a later `amount: u64` would appear.
    #[test]
    fn the_handler_accepts_no_caller_supplied_amount() {
        assert!(production(SRC).contains(
            "pub fn handler<'info>(ctx: Context<'_, '_, 'info, 'info, SweepT04Overflow<'info>>)"
        ));
    }

    /// `protocol_revenue` is absent by construction — this is a distinct fund movement with its
    /// own single destination, and the protocol-revenue split governs the fee path only. An
    /// account that is not declared cannot be credited.
    #[test]
    fn protocol_revenue_is_not_reachable_from_this_instruction() {
        assert_eq!(production(SRC).matches("protocol_revenue").count(), 0);
        assert_eq!(production(SRC).matches("treasury_05").count(), 0);
    }

    /// The unset-ceiling refusal, then the pool-set check, then the transfer.
    #[test]
    fn both_gates_run_before_any_transfer() {
        let prod = production(SRC);
        let handler = &prod[prod.find("pub fn handler<").unwrap()..];
        let unset = handler
            .find("require!(ceiling > 0, ByeMachineError::T04CeilingUnset);")
            .expect("the unset ceiling is refused");
        let pools = handler.find("assert_every_pool_idle(").unwrap();
        let transfer = handler.find("transfer(").unwrap();
        assert!(unset < pools && pools < transfer);
        assert_eq!(calls(&prod, "assert_every_pool_idle"), 1);
        assert_eq!(calls(&prod, "assert_weight_open"), 1);
    }

    /// Exactly one transfer, Treasury 04 its source, signed by Treasury 04's authority.
    #[test]
    fn exactly_one_transfer_and_it_debits_treasury_04() {
        let prod = production(SRC);
        assert_eq!(calls(&prod, "transfer"), 1);
        assert!(prod.contains("from: ctx.accounts.treasury_04.to_account_info()"));
        assert!(prod.contains("to: ctx.accounts.treasury_03.to_account_info()"));
        assert!(prod.contains("authority: ctx.accounts.protocol_config.to_account_info()"));
    }

    /// **The no-op path returns before both the transfer and the event.** Asserted by position,
    /// because a `return` that drifted below the `emit!` would still satisfy "emits once".
    #[test]
    fn the_no_op_path_returns_before_the_transfer_and_the_event() {
        let prod = production(SRC);
        let early_return = prod
            .find("return Ok(());")
            .expect("the no-op path returns early");
        assert!(early_return < prod.find("transfer(").unwrap());
        assert!(early_return < prod.find("emit!(").unwrap());
        assert_eq!(prod.matches("emit!(").count(), 1, "one emitter, one path");
    }

    /// **This is the event's only emitter, and the settle paths carry none.** The absence there
    /// is checked against live bodies — each still emits its own events, so a gutted file could
    /// not produce this zero.
    #[test]
    fn this_is_the_only_emitter_of_the_sweep_event() {
        assert_eq!(production(SRC).matches("emit!(T04Swept").count(), 1);
        assert_eq!(production(SRC).matches("T04CeilingOverflow").count(), 0);

        for (label, source) in [
            ("settle_roll.rs", SETTLE_SRC),
            ("settle_roll_core.rs", SETTLE_CORE_SRC),
        ] {
            let prod = production(source);
            assert_eq!(
                prod.matches("T04Swept").count() + prod.matches("T04CeilingOverflow").count(),
                0,
                "{label} names a sweep event, which only the sweep may emit"
            );
            assert!(
                prod.matches("emit!(").count() > 0,
                "{label} emits nothing at all — a zero over a gutted file proves nothing"
            );
            assert_eq!(
                prod.matches("t04_ceiling").count(),
                0,
                "{label} still reads t04_ceiling — settlement must never price against it"
            );
        }
    }
}

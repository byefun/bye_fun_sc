//! Switches whether an escrow buys back its owner's cards.
//!
//! **Only the owner can change it.** The escrow is seeded on the signer, so any other wallet —
//! the Operator and the Administrator included — derives a different address and is refused by
//! the seeds. Arming reads no balance: an empty escrow can be armed, and buys nothing until it is
//! funded. Writing the current value again is accepted and changes nothing.

use anchor_lang::prelude::*;

use crate::common::events::EscrowArmed;
use crate::common::seeds::ESCROW_SEED;
use crate::state::Escrow;

/// Writes `armed` and reports whether it changed.
pub(crate) fn apply_armed(escrow: &mut Escrow, armed: bool) -> bool {
    if escrow.armed == armed {
        return false;
    }
    escrow.armed = armed;
    true
}

pub fn handler(ctx: Context<SetEscrowArmed>, armed: bool) -> Result<()> {
    let clock = Clock::get()?;

    if apply_armed(&mut ctx.accounts.escrow, armed) {
        emit!(EscrowArmed {
            escrow: ctx.accounts.escrow.key(),
            slot: clock.slot,
            owner: ctx.accounts.owner.key(),
            armed,
        });
    }

    Ok(())
}

#[derive(Accounts)]
pub struct SetEscrowArmed<'info> {
    pub owner: Signer<'info>,

    #[account(
        mut,
        seeds = [ESCROW_SEED, owner.key().as_ref()],
        bump = escrow.bump
    )]
    pub escrow: Box<Account<'info, Escrow>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{accounts_body, field_writes, production};

    const SRC: &str = include_str!("set_escrow_armed.rs");

    fn escrow(armed: bool) -> Escrow {
        Escrow {
            owner: Pubkey::new_unique(),
            armed,
            bump: 254,
            vault_bump: 253,
            reserved: [0; 64],
        }
    }

    #[test]
    fn every_transition_lands_on_the_requested_value_and_reports_only_a_change() {
        for (from, to) in [(false, true), (true, false), (false, false), (true, true)] {
            let mut account = escrow(from);
            let before = account.clone();
            let changed = apply_armed(&mut account, to);
            assert_eq!(account.armed, to);
            assert_eq!(changed, from != to, "{from} -> {to}");
            assert_eq!(account.owner, before.owner);
            assert_eq!(account.bump, before.bump);
            assert_eq!(account.vault_bump, before.vault_bump);
            assert_eq!(account.reserved, before.reserved);
        }
    }

    #[test]
    fn the_escrow_is_seeded_on_the_signer_and_is_the_only_account_besides_it() {
        let prod = production(SRC);
        let body = accounts_body(SRC);
        assert_eq!(prod.matches(": Signer<'info>").count(), 1);
        assert!(body.contains("seeds = [ESCROW_SEED, owner.key().as_ref()]"));
        assert!(body.contains("bump = escrow.bump"));
        let slots: Vec<&str> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub "))
            .filter_map(|r| r.split_once(':'))
            .map(|(name, _)| name.trim())
            .collect();
        assert_eq!(slots, ["owner", "escrow"]);
    }

    #[test]
    fn writes_armed_alone_reads_no_balance_and_emits_only_on_a_change() {
        assert_eq!(field_writes(SRC, "escrow"), ["armed"]);
        let prod = production(SRC);
        assert!(!prod.contains("escrow_vault"));
        assert!(!prod.contains(".amount"));
        let gate = prod
            .find("if apply_armed(&mut ctx.accounts.escrow, armed) {")
            .expect("the event is no longer gated on a change");
        let emit = prod.find("emit!(EscrowArmed").expect("EscrowArmed is gone");
        assert!(gate < emit);
    }
}

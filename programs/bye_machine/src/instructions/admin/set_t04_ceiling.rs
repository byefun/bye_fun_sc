use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::T04CeilingSet;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::ProtocolConfig;

/// Sets the balance `sweep_t04_overflow` returns the shared Treasury 04 to, in micro-USDC. 0 is
/// accepted and leaves the sweep refused.
pub fn handler(ctx: Context<SetT04Ceiling>, ceiling: u64) -> Result<()> {
    let config = &mut ctx.accounts.protocol_config;
    let old = config.t04_ceiling;
    config.t04_ceiling = ceiling;

    emit!(T04CeilingSet {
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        old,
        new: ceiling,
    });
    Ok(())
}

#[derive(Accounts)]
pub struct SetT04Ceiling<'info> {
    pub administrator: Signer<'info>,

    #[account(
        mut,
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{accounts_body, field_writes, production, writes};

    const SRC: &str = include_str!("set_t04_ceiling.rs");

    #[test]
    fn the_administrator_is_the_only_signer_class() {
        let prod = production(SRC);
        assert_eq!(
            prod.matches(
                "constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized"
            )
            .count(),
            1
        );
        assert_eq!(prod.matches("operator").count(), 0);
        assert_eq!(accounts_body(SRC).matches("Signer<'info>").count(), 1);
    }

    /// The ceiling is the one field written, and nothing else on `ProtocolConfig` moves.
    #[test]
    fn writes_the_ceiling_and_nothing_else() {
        assert_eq!(writes(SRC, "t04_ceiling"), 1);
        assert_eq!(field_writes(SRC, "config"), vec!["t04_ceiling".to_string()]);
    }
}

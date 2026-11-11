use anchor_lang::prelude::*;

use crate::common::damm_v2::assert_immutable_identity;
use crate::common::errors::ByeMachineError;
use crate::common::events::VrfConfigChanged;
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::ProtocolConfig;

/// Rotates the external-dependency configuration — the VRF provider, its queue, the pinned swap
/// venue, and the slippage bound spent on that venue — together, with no partial-update form.
///
/// This handler is the only writer of the VRF addresses and bound outside `init_protocol`.
/// `update_swap_pool` is deliberately the separate third writer of the venue field. Without these
/// rotation paths a value fixed at launch behind a provider not yet chosen, a queue address a
/// later migration retires, or a mistyped slippage bound would have no way back to a correct one
/// for the life of the deployment — and a wrong bound surfaces only when the first blank is drawn.
///
/// The venue is validated on the way in, on the same terms `init_protocol` validates it. Rotation
/// that skipped the check would be a way around the gate at launch: pin a valid pool, then move
/// to anything.
pub fn handler(
    ctx: Context<UpdateVrfConfig>,
    vrf_program: Pubkey,
    oracle_queue: Pubkey,
    swap_pool: Pubkey,
    max_swap_slippage_bps: u16,
) -> Result<()> {
    require_keys_eq!(
        ctx.accounts.swap_pool.key(),
        swap_pool,
        ByeMachineError::SwapPoolInvalid
    );
    assert_immutable_identity(
        &ctx.accounts.swap_pool.key(),
        ctx.accounts.swap_pool.owner,
        &ctx.accounts.swap_pool.try_borrow_data()?,
        &ctx.accounts.protocol_config.bye_mint,
        &ctx.accounts.protocol_config.usdc_mint,
    )?;
    assert_slippage_bound(max_swap_slippage_bps)?;

    let config = &mut ctx.accounts.protocol_config;
    let old_vrf_program = config.vrf_program;
    let old_oracle_queue = config.oracle_queue;
    let old_swap_pool = config.swap_pool;
    let old_max_swap_slippage_bps = config.max_swap_slippage_bps;

    config.vrf_program = vrf_program;
    config.oracle_queue = oracle_queue;
    config.swap_pool = swap_pool;
    config.max_swap_slippage_bps = max_swap_slippage_bps;

    emit!(VrfConfigChanged {
        slot: Clock::get()?.slot,
        authority: ctx.accounts.administrator.key(),
        old_vrf_program,
        old_oracle_queue,
        old_swap_pool,
        vrf_program,
        oracle_queue,
        swap_pool,
        old_max_swap_slippage_bps,
        max_swap_slippage_bps,
    });

    Ok(())
}

/// At 10,000 the bound collapses to zero and the swap carries no slippage protection at all; above
/// it the arithmetic that derives the minimum underflows. Refused where the value is written, so a
/// mistyped bound fails at configuration rather than at the first blank settlement.
pub(crate) fn assert_slippage_bound(max_swap_slippage_bps: u16) -> Result<()> {
    require!(
        max_swap_slippage_bps < 10_000,
        ByeMachineError::SwapSlippageOutOfBounds
    );
    Ok(())
}

#[derive(Accounts)]
pub struct UpdateVrfConfig<'info> {
    pub administrator: Signer<'info>,

    #[account(
        mut,
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    /// CHECK: the venue being rotated in, validated by the adapter — owner, pool discriminator,
    /// the mint pair as a set, and both of its own token vaults. Checked against the mints the
    /// config already records, so a rotation cannot quietly re-point the protocol at a pool for
    /// a different pair.
    pub swap_pool: UncheckedAccount<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::{calls, crate_sources, writes};

    /// This handler is the only writer of the three rotated addresses outside `init_protocol`;
    /// a third writer fails here rather than leaving the doc comment false. Keyed on the field
    /// rather than a receiver name, so a handler reaching the config under any binding is seen.
    #[test]
    fn each_external_dependency_field_has_its_declared_writers() {
        let sources = crate_sources();

        for (field, expected) in [
            (
                ".vrf_program",
                vec!["init_protocol.rs", "update_vrf_config.rs"],
            ),
            (
                ".oracle_queue",
                vec!["init_protocol.rs", "update_vrf_config.rs"],
            ),
            (
                ".max_swap_slippage_bps",
                vec!["init_protocol.rs", "update_vrf_config.rs"],
            ),
            (
                ".swap_pool",
                vec![
                    "init_protocol.rs",
                    "update_swap_pool.rs",
                    "update_vrf_config.rs",
                ],
            ),
        ] {
            let mut writers: Vec<String> = sources
                .iter()
                .filter(|path| writes(&std::fs::read_to_string(path).unwrap(), field) > 0)
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            writers.sort();

            assert_eq!(
                writers,
                expected.into_iter().map(str::to_string).collect::<Vec<_>>(),
                "{field} is written somewhere the rotation contract does not account for"
            );
        }
    }

    /// The bound is refused where it is written, so a mistyped value fails at configuration
    /// rather than at the first blank drawn — which is the only other place it would surface,
    /// and by then the pool is open and tickets are sold.
    #[test]
    fn the_slippage_bound_admits_everything_below_ten_thousand_and_nothing_above() {
        for bps in [0u16, 1, 100, 250, 9_998, 9_999] {
            assert!(
                assert_slippage_bound(bps).is_ok(),
                "{bps} bps must be accepted"
            );
        }
        for bps in [10_000u16, 10_001, u16::MAX] {
            assert_eq!(
                assert_slippage_bound(bps).unwrap_err(),
                ByeMachineError::SwapSlippageOutOfBounds.into(),
                "{bps} bps must be refused"
            );
        }
    }

    /// Both writers of the bound run the same predicate. A rotation that validated and an
    /// initialisation that did not — or the reverse — would leave one door open.
    #[test]
    fn both_writers_of_the_bound_call_the_same_predicate() {
        for (label, path) in [
            (
                "update_vrf_config",
                "instructions/admin/update_vrf_config.rs",
            ),
            ("init_protocol", "instructions/admin/init_protocol.rs"),
        ] {
            let src = std::fs::read_to_string(format!("{}/src/{path}", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
            assert!(
                calls(&src, "assert_slippage_bound") > 0,
                "{label} writes the bound without checking it"
            );
        }
    }

    /// Same argument for the venue itself: rotation that skipped the identity check would be a
    /// way around the gate at launch — pin a valid pool, then move to anything.
    #[test]
    fn both_writers_of_the_venue_validate_it() {
        for (label, path) in [
            (
                "update_vrf_config",
                "instructions/admin/update_vrf_config.rs",
            ),
            ("init_protocol", "instructions/admin/init_protocol.rs"),
        ] {
            let src = std::fs::read_to_string(format!("{}/src/{path}", env!("CARGO_MANIFEST_DIR")))
                .unwrap();
            assert!(
                calls(&src, "assert_immutable_identity") > 0,
                "{label} records the venue without validating it"
            );
        }
    }
}

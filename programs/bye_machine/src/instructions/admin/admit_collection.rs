use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::CollectionAdmitted;
use crate::common::seeds::{COLLECTION_SEED, POOL_SEED, PROTOCOL_CONFIG_SEED};
use crate::common::standards::{clears_a_bit, mask_is_valid};
use crate::state::{Pool, PoolCollection, ProtocolConfig};

/// The three argument guards, in the order a caller hits them: the mask must be usable, the
/// update must change something, and `reason` must be present **exactly** when the update clears a
/// bit.
///
/// A free function rather than inline `require!`s in `handler`, because the handler needs a
/// `Context` and the exhaustive test below does not — and a test that re-implemented these three
/// conditions would pass while the handler had them wired backwards. That is not hypothetical: it
/// is what this module's test did first, and an injection negating the narrowing predicate in the
/// handler left all 128 of its cases green.
pub fn check_update(old_standards: u8, standards: u8, reason: Option<u16>) -> Result<()> {
    require!(
        mask_is_valid(standards),
        ByeMachineError::StandardsMaskInvalid
    );
    require!(
        standards != old_standards,
        ByeMachineError::StandardsUpdateNoOp
    );

    let narrowing = clears_a_bit(old_standards, standards);
    require!(
        narrowing == reason.is_some(),
        if narrowing {
            ByeMachineError::NarrowingReasonRequired
        } else {
            ByeMachineError::ReasonNotApplicable
        }
    );
    Ok(())
}

pub fn handler(
    ctx: Context<AdmitCollection>,
    collection: Pubkey,
    standards: u8,
    reason: Option<u16>,
) -> Result<()> {
    let record = &mut ctx.accounts.pool_collection;
    // `init_if_needed` zeroes a fresh account, so `pool` is the identity field that distinguishes
    // a creation from an update. It is read once into `is_new` rather than re-tested, because
    // `old_standards == 0` is equivalent only while `mask_is_valid` forbids storing a zero mask —
    // branching on one and computing from the other would couple this handler to that guard
    // silently.
    let is_new = record.pool == Pubkey::default();
    let old_standards = if is_new { 0 } else { record.standards };

    check_update(old_standards, standards, reason)?;

    let pool_key = ctx.accounts.pool.key();
    let now = Clock::get()?.unix_timestamp;

    if is_new {
        record.pool = pool_key;
        record.collection = collection;
        record.admitted_at = now;
        record.bump = ctx.bumps.pool_collection;
    }
    record.standards = standards;

    emit!(CollectionAdmitted {
        pool: pool_key,
        slot: Clock::get()?.slot,
        collection,
        standards,
        old_standards,
        reason,
        authority: ctx.accounts.administrator.key(),
    });

    Ok(())
}

#[derive(Accounts)]
#[instruction(collection: Pubkey)]
pub struct AdmitCollection<'info> {
    #[account(mut)]
    pub administrator: Signer<'info>,

    #[account(
        seeds = [PROTOCOL_CONFIG_SEED],
        bump = protocol_config.bump,
        constraint = administrator.key() == protocol_config.administrator @ ByeMachineError::Unauthorized
    )]
    pub protocol_config: Account<'info, ProtocolConfig>,

    #[account(
        seeds = [POOL_SEED, pool.pool_id.to_le_bytes().as_ref()],
        bump = pool.bump
    )]
    pub pool: Account<'info, Pool>,

    #[account(
        init_if_needed,
        payer = administrator,
        space = 8 + PoolCollection::INIT_SPACE,
        seeds = [COLLECTION_SEED, pool.key().as_ref(), collection.as_ref()],
        bump
    )]
    pub pool_collection: Account<'info, PoolCollection>,

    pub system_program: Program<'info, System>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::standards::{clears_a_bit, mask_is_valid};
    use crate::common::test_support::production;

    const SRC: &str = include_str!("admit_collection.rs");

    fn error_code(err: anchor_lang::error::Error) -> u32 {
        match err {
            anchor_lang::error::Error::AnchorError(ae) => ae.error_code_number,
            other => panic!("expected AnchorError, got {other:?}"),
        }
    }

    /// The full `old × new × reason-presence` domain: 8 × 8 × 2 = 128 cases, every one asserted
    /// against an independently-computed expectation. A sampled test passes even when the
    /// narrowing predicate and the reason requirement are wired to each other backwards, which
    /// is the fault that would make `withdraw_collection`'s mandatory motive avoidable by
    /// clearing bits one at a time.
    #[test]
    fn the_three_guards_partition_the_whole_old_new_reason_domain() {
        for old in 0u8..=0b111 {
            for new in 0u8..=0b111 {
                for reason in [None, Some(7u16)] {
                    let expected = if !mask_is_valid(new) {
                        Some(6507)
                    } else if new == old {
                        Some(6508)
                    } else if clears_a_bit(old, new) != reason.is_some() {
                        Some(if clears_a_bit(old, new) { 6509 } else { 6510 })
                    } else {
                        None
                    };
                    let actual = check_update(old, new, reason).err().map(error_code);
                    assert_eq!(
                        actual, expected,
                        "old {old:#05b} -> new {new:#05b}, reason {reason:?}"
                    );
                }
            }
        }
    }

    /// A creation is `old == 0`, so it can never be a narrowing — which is why a `reason` on one
    /// is rejected as meaningless rather than merely unused.
    #[test]
    fn a_creation_rejects_a_reason_and_accepts_its_absence() {
        assert_eq!(
            error_code(check_update(0, 0b011, Some(7)).unwrap_err()),
            6510
        );
        assert!(check_update(0, 0b011, None).is_ok());
    }

    /// `admitted_at` dates the FIRST admission, so the update path must not touch it. Asserted on
    /// the handler's source because the write is inside an `if is_new` block and there is no
    /// return value to observe it through.
    #[test]
    fn admitted_at_and_the_identity_fields_are_written_only_on_the_creation_branch() {
        let prod = production(SRC);
        // Anchored on the STATEMENT form, `if is_new {` followed by a newline. The bare string
        // also matches the inline `let old_standards = if is_new { 0 } else { ... }` earlier in
        // the handler, and that occurrence comes first — so a `.split(..).nth(1)` on it silently
        // pinned the wrong block and reported the writes as missing. Found by this test failing
        // for the wrong reason.
        const BRANCH: &str = "if is_new {\n";
        assert_eq!(
            prod.matches(BRANCH).count(),
            1,
            "the statement-form branch must be unique, or this pin picks an arbitrary one"
        );
        let guarded = prod.split(BRANCH).nth(1).unwrap();
        let creation_only = guarded.split("    }").next().unwrap();
        for field in [
            "record.pool = pool_key;",
            "record.collection = collection;",
            "record.admitted_at = now;",
            "record.bump = ctx.bumps.pool_collection;",
        ] {
            assert!(
                creation_only.contains(field),
                "`{field}` must sit inside the is_new branch"
            );
            assert_eq!(
                prod.matches(field).count(),
                1,
                "`{field}` has a second writer outside the creation branch"
            );
        }
        assert!(
            !creation_only.contains("record.standards ="),
            "standards must be written on BOTH branches, so it cannot sit inside is_new"
        );
    }
}

use anchor_lang::prelude::*;
use anchor_spl::token::{Token, TokenAccount};
use mpl_core::ID as MPL_CORE_ID;

use crate::common::buyback::{
    pay_for_sale, record_acquisition, AcquisitionAccounts, AcquisitionTerms, LoaderAccounts,
    SaleAccounts,
};
use crate::common::core_asset::{declared_core_collection, read_core_asset, transfer_core};
use crate::common::errors::ByeMachineError;
use crate::common::events::{InstantSold, RebalanceEvaluated, TierChanged};
use crate::common::guards::validate_admission_value;
use crate::common::seeds::{
    POOL_SEED, POSITION_SEED, POSITION_VAULT_SEED, PROTOCOL_CONFIG_SEED, TOP_TIER_SEED,
    TREASURY_02_SEED, WALLET_STATS_SEED,
};
use crate::common::standards::STANDARD_CORE;
use crate::state::{
    Pool, PoolCollection, Position, ProtocolConfig, TopTier, WalletStats, WeightIndex,
};

/// `instant_sell`'s MPL-Core twin.
///
/// **It differs from its principal in the custody leg and nowhere else.** Same two signers, same
/// guards in the same order, same payer selection and payment legs, same `Pending`/`Active`
/// branch, same event. Both halves around the custody leg are shared implementations —
/// `pay_for_sale` and `record_acquisition` — so the claim is structural rather than a promise
/// two copies keep.
///
/// **Three things the custody leg does differently**, each inherited from `deposit_core`. There is
/// no vault **token account** — `PDA(["vault", position])` holds the asset directly as
/// `AssetV1.owner`, which makes this twin cheaper than its principal in both accounts and rent.
/// There is no master-edition guard, because MPL Core has no edition mechanism for a print attack
/// to use. And the standard is not resolved from a metadata field: the owner program *is* the
/// standard on this family, which is what `read_core_asset` asserts before any CPI and what
/// `require_admits` is given as `STANDARD_CORE`.
///
/// **The new owner's book inherits the Core seizure exposure**. A Core card bought at a sale
/// sits in custody under the same
/// `permanent_transfer_delegate` and `permanent_burn_delegate` as any depositor's. T02 has full
/// exit parity, so it has full parity in this too. This handler implements that rule;
/// it adds **no** per-standard switch — one `PoolCollection.standards` bitmask governs deposits and
/// instant sells alike.
pub fn handler<'info>(
    ctx: Context<'_, '_, 'info, 'info, InstantSellCore<'info>>,
    value: u64,
    observed_at: i64,
) -> Result<()> {
    require!(
        !ctx.accounts.pool.deposits_paused,
        ByeMachineError::DepositsPaused
    );

    let asset_key = ctx.accounts.asset.key();
    let asset = read_core_asset(&ctx.accounts.asset)?;

    // The seller must actually hold the card. MPL Core would refuse the CPI anyway — with its own
    // `NoApprovals` (26), which says nothing a client can act on — and this is the exact analogue
    // of the principal's `seller_token.owner == seller.key()` constraint.
    require_keys_eq!(
        asset.owner,
        ctx.accounts.seller.key(),
        ByeMachineError::NotPositionOwner
    );

    // The collection comes from `UpdateAuthority::Collection`, which MPL Core makes the
    // collection's own authority sign for at mint. Open to any eligible-card holder.
    let collection = declared_core_collection(&asset)?;
    PoolCollection::require_admits(
        &ctx.accounts.pool.key(),
        &collection,
        &ctx.accounts.pool_collection.key(),
        &ctx.accounts.pool_collection,
        STANDARD_CORE,
    )?;

    // The record decision above is derived from what the *asset* declares, never from this
    // account, so this pins the CPI's collection to the same key rather than the admission. MPL
    // Core rejects a mismatch too, with `InvalidCollection` (19); a named code beats depending on
    // the dependency to catch what this program can check itself.
    require_keys_eq!(
        ctx.accounts.collection.key(),
        collection,
        ByeMachineError::CollectionNotAdmitted
    );

    // The value limits, on the principal's reasoning; the wallet cap is read inside
    // `pay_for_sale`, against whichever owner the position will belong to.
    validate_admission_value(&ctx.accounts.pool, value)?;

    let mut sale = pay_for_sale(
        SaleAccounts {
            pool: &ctx.accounts.pool,
            protocol_config: &ctx.accounts.protocol_config,
            treasury_03: &ctx.accounts.treasury_03,
            treasury_02_stats: &ctx.accounts.wallet_stats,
            seller_usdc: ctx.accounts.seller_usdc.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
            loader: LoaderAccounts {
                escrow: ctx.accounts.loader_escrow.as_deref(),
                escrow_vault: ctx.accounts.loader_escrow_vault.as_deref(),
                wallet_stats: ctx.accounts.loader_wallet_stats.as_deref(),
            },
        },
        value,
    )?;

    // The custody leg, and the only half that differs from the principal. `[3] authority` is the
    // seller and `[4] newOwner` the new vault PDA — an account that does not exist and is not
    // allocated by `TransferV1`, so the vault stays accountless and nobody can close it.
    //
    // `Some` unconditionally, and this is the right call site for that: the handler has just
    // required the asset to declare this collection, so there is no arm on which the intake
    // transfer must be given `None`.
    transfer_core(
        &ctx.accounts.asset,
        Some(&ctx.accounts.collection),
        &ctx.accounts.position_vault,
        &ctx.accounts.seller.to_account_info(),
        // The Operator pays, not the seller — the rent-bearing accounts here are the treasury's.
        &ctx.accounts.operator.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &[],
    )?;

    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    let pool_key = ctx.accounts.pool.key();
    let position_key = ctx.accounts.position.key();
    let t02_identity = ctx.accounts.t02_identity.key();
    let seller = ctx.accounts.seller.key();

    let mut weight_index = ctx.accounts.weight_index.load_mut()?;
    let (depositor, rollup) = sale.acquirer(pool_key, t02_identity, &mut ctx.accounts.wallet_stats);
    let acquired = record_acquisition(
        AcquisitionAccounts {
            pool: &mut ctx.accounts.pool,
            weight_index: &mut weight_index,
            top_tier: &mut ctx.accounts.top_tier,
            position: &mut ctx.accounts.position,
            position_key,
            wallet_stats: rollup,
            remaining_accounts: ctx.remaining_accounts,
        },
        AcquisitionTerms {
            pool_key,
            depositor,
            // `nft_mint` names the escrowed asset. On this family there is no mint — the
            // `AssetV1` account *is* the instrument — and the field is not renamed because it is
            // a shipped layout read by three TS decoders and by `tier-resolve`'s memcmp offsets.
            nft_mint: asset_key,
            // Written from the branch the caller reached, not from a field read: the owner
            // program is the standard on this family.
            standard: STANDARD_CORE,
            value,
            observed_at,
            now,
        },
    )?;
    drop(weight_index);
    sale.store_loader_rollup()?;

    ctx.accounts.wallet_stats.bump = ctx.bumps.wallet_stats;
    ctx.accounts.position.bump = ctx.bumps.position;
    // Stored on all three standards. What is absent here is the token account, not the bump: the
    // exit CPI signs as `PDA(["vault", position])` under `invoke_signed`, and program signing does
    // not require the signing account to exist.
    ctx.accounts.position.vault_bump = ctx.bumps.position_vault;

    emit!(InstantSold {
        pool: pool_key,
        slot: clock.slot,
        position: position_key,
        seller,
        value,
        observed_at,
        observed_age_seconds: now
            .checked_sub(observed_at)
            .ok_or(ByeMachineError::ArithmeticFailure)?,
        rate_bps: sale.rate_bps,
        paid: sale.paid,
        standard: ctx.accounts.position.standard,
        payer: sale.payer,
        escrow_owner: sale.escrow_owner(),
        escrow_paid: sale.escrow_paid,
        bid_bps: sale.bid_bps,
    });

    if let Some(activation) = acquired.activation {
        emit!(RebalanceEvaluated {
            pool: pool_key,
            slot: clock.slot,
            n_real: ctx.accounts.pool.n_real,
            w_real: ctx.accounts.pool.w_real,
            blanks_before: activation.blanks_before,
            blanks_after: ctx.accounts.pool.blanks,
        });
        if activation.entered_tier {
            emit!(TierChanged {
                pool: pool_key,
                slot: clock.slot,
                left: activation.displaced,
                entered: Some(position_key),
            });
        }
    }

    Ok(())
}

/// Two signers, as on the principal. The custody surface is MPL Core's — an `asset`, a
/// `collection` and the MPL Core program — where the principal's is Token Metadata's. The token
/// program and the three loader accounts are the principal's, because the USDC legs are shared.
#[derive(Accounts)]
pub struct InstantSellCore<'info> {
    pub seller: Signer<'info>,

    #[account(mut)]
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

    /// The admission record for the collection the presented asset declares. Its address is
    /// re-derived in the handler from this pool's key and the collection the *asset* names.
    pub pool_collection: Box<Account<'info, PoolCollection>>,

    /// CHECK: the T02 identity PDA — an address, not an account. It holds no balance; its role
    /// is to become `Position.depositor` on a Treasury 03 sale.
    #[account(seeds = [TREASURY_02_SEED, pool.key().as_ref()], bump)]
    pub t02_identity: UncheckedAccount<'info>,

    #[account(
        init,
        payer = operator,
        space = 8 + Position::INIT_SPACE,
        seeds = [POSITION_SEED, pool.key().as_ref(), asset.key().as_ref()],
        bump
    )]
    pub position: Box<Account<'info, Position>>,

    /// CHECK: the escrow identity, and **not an account**. Derived here so Anchor pins the address
    /// and yields the `vault_bump` the exit path signs with; deliberately never `init`, because a
    /// Core asset needs no token account and allocating one would create rent nobody can reclaim.
    /// Its only on-chain expression is `AssetV1.owner`.
    #[account(
        seeds = [POSITION_VAULT_SEED, position.key().as_ref()],
        bump
    )]
    pub position_vault: UncheckedAccount<'info>,

    /// The T02 rollup, created here with the Operator's rent because nothing else ever creates it.
    /// An escrow sale creates it when absent and leaves its counters alone.
    #[account(
        init_if_needed,
        payer = operator,
        space = 8 + WalletStats::INIT_SPACE,
        seeds = [WALLET_STATS_SEED, pool.key().as_ref(), t02_identity.key().as_ref()],
        bump
    )]
    pub wallet_stats: Box<Account<'info, WalletStats>>,

    #[account(mut, address = pool.weight_index)]
    pub weight_index: AccountLoader<'info, WeightIndex>,

    #[account(
        mut,
        seeds = [TOP_TIER_SEED, pool.key().as_ref()],
        bump = top_tier.bump
    )]
    pub top_tier: Box<Account<'info, TopTier>>,

    #[account(mut, address = pool.treasury_03)]
    pub treasury_03: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = seller_usdc.mint == treasury_03.mint @ ByeMachineError::StandardNotAdmitted,
        constraint = seller_usdc.owner == seller.key() @ ByeMachineError::NotPositionOwner
    )]
    pub seller_usdc: Box<Account<'info, TokenAccount>>,

    /// CHECK: owner program, discriminator and contents are asserted by `read_core_asset`; the
    /// handler then pins its owner to the seller. Writable because `TransferV1` rewrites
    /// `AssetV1.owner`.
    #[account(mut)]
    pub asset: UncheckedAccount<'info>,

    /// CHECK: pinned in the handler to the collection the asset itself declares, which is also the
    /// collection the admission record was derived for.
    pub collection: UncheckedAccount<'info>,

    /// CHECK: pinned to the MPL Core program this family's assets live under.
    #[account(address = MPL_CORE_ID)]
    pub mpl_core_program: UncheckedAccount<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,

    /// CHECK: the loader's escrow, named by the Operator; bound in `bind_loader`, as on the
    /// principal.
    pub loader_escrow: Option<UncheckedAccount<'info>>,

    /// CHECK: bound in `bind_loader` to `[ESCROW_VAULT_SEED, loader_escrow]`.
    #[account(mut)]
    pub loader_escrow_vault: Option<UncheckedAccount<'info>>,

    /// CHECK: bound in `bind_loader` to `[WALLET_STATS_SEED, pool, escrow.owner]`.
    #[account(mut)]
    pub loader_wallet_stats: Option<UncheckedAccount<'info>>,
}

#[cfg(test)]
mod tests {
    use crate::common::test_support::{calls, flat, production};

    const CORE_SRC: &str = include_str!("instant_sell_core.rs");
    const PRINCIPAL_SRC: &str = include_str!("instant_sell.rs");

    /// Executable lines only: every `//`, `///` and `//!` line dropped, so prose may differ
    /// between the twins but code may not.
    fn code_only(source: &str) -> String {
        production(source)
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .map(str::trim_end)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The span between two anchors, inclusive. Panics rather than returning an empty slice if
    /// either anchor moved — a silently empty span would make every comparison below vacuous.
    fn span(source: &str, start: &str, end: &str) -> String {
        let prod = production(source);
        let i = prod
            .find(start)
            .unwrap_or_else(|| panic!("span start anchor missing: {start}"));
        let j = prod[i..]
            .find(end)
            .unwrap_or_else(|| panic!("span end anchor missing: {end}"))
            + i;
        prod[i..j + end.len()].to_string()
    }

    const PROLOGUE: (&str, &str) = ("    validate_admission_value(", "        value,\n    )?;\n");
    const EPILOGUE: (&str, &str) = ("    emit!(InstantSold {", "    Ok(())\n}");

    /// **The twin differs from its principal in the custody leg and nowhere else.** Compares
    /// whole spans with `assert_eq!` rather than `contains()`, which is field-agnostic and
    /// passes on a swapped pair. Two spans, because the custody leg sits between them: the
    /// admission, payout, T03-solvency and USDC prologue, and the event epilogue.
    ///
    /// Everything between them is `record_acquisition` and the payer's ownership hand-off, shared
    /// implementations whose single calls `both_twins_pay_and_assign_through_the_shared_half` pins,
    /// so comparing `field_writes` would be vacuous (both handlers write no position fields); the
    /// line-count floors below guard against that.
    #[test]
    fn the_twin_differs_from_its_principal_in_the_custody_leg_and_nowhere_else() {
        for (label, (start, end), floor) in
            [("prologue", PROLOGUE, 15usize), ("epilogue", EPILOGUE, 25)]
        {
            let principal = code_only(&span(PRINCIPAL_SRC, start, end));
            let twin = code_only(&span(CORE_SRC, start, end));
            assert!(
                principal.lines().count() >= floor,
                "{label}: the principal's span is {} lines — too small for the comparison below \
                 to be worth anything; an anchor probably moved",
                principal.lines().count()
            );
            assert_eq!(
                twin, principal,
                "{label}: the twin's non-custody code diverged from its principal's"
            );
        }
    }

    /// **Payer selection and both payment legs live in `common/buyback.rs`, not in either
    /// handler.** Each twin calls the payment, the ownership hand-off and the rollup write-back
    /// exactly once, and neither moves USDC itself — a `transfer(` here would be a payment leg
    /// one twin has and the other may not.
    #[test]
    fn both_twins_pay_and_assign_through_the_shared_half() {
        for (label, source) in [("principal", PRINCIPAL_SRC), ("twin", CORE_SRC)] {
            for callee in ["pay_for_sale", "sale.acquirer", "sale.store_loader_rollup"] {
                assert_eq!(
                    calls(source, callee),
                    1,
                    "{label} calls {callee} other than once"
                );
            }
            assert_eq!(calls(source, "transfer"), 0, "{label} moves USDC itself");
            assert_eq!(
                calls(source, "payout_for"),
                0,
                "{label} prices the sale itself"
            );
        }
    }

    /// **The control the comparison above needs.** If `instant_sell_core` were a copy of
    /// `instant_sell`, every span would match and the test would pass while the twin did the
    /// wrong thing entirely. So the custody legs are asserted to *differ*: the twin transfers
    /// through MPL Core and never through Token Metadata, and the principal the reverse.
    #[test]
    fn the_custody_legs_are_not_the_same_code() {
        assert_eq!(calls(CORE_SRC, "transfer_core"), 1);
        assert_eq!(calls(CORE_SRC, "transfer_pnft"), 0);
        assert_eq!(calls(CORE_SRC, "transfer_spl"), 0);

        assert_eq!(calls(PRINCIPAL_SRC, "transfer_core"), 0);
        assert_eq!(calls(PRINCIPAL_SRC, "transfer_pnft"), 1);
        assert_eq!(calls(PRINCIPAL_SRC, "transfer_spl"), 1);
    }

    /// `standard == 2` is asserted before any CPI — not as a field read, because on this family
    /// the owner program *is* the standard. `read_core_asset` pins that owner program, and the
    /// admission predicate is given `STANDARD_CORE` rather than a resolved value, so a pNFT or a
    /// legacy mint cannot reach this handler's transfer.
    #[test]
    fn the_core_standard_is_asserted_before_any_cpi() {
        assert_eq!(calls(CORE_SRC, "read_core_asset"), 1);
        assert!(flat(CORE_SRC).contains(
            "PoolCollection::require_admits( &ctx.accounts.pool.key(), &collection, \
             &ctx.accounts.pool_collection.key(), &ctx.accounts.pool_collection, STANDARD_CORE, )?;"
        ));

        let prod = production(CORE_SRC);
        let gate = prod
            .find("read_core_asset")
            .expect("the asset read is the standard assertion");
        for cpi in ["transfer_core(", "transfer("] {
            assert!(
                prod.find(cpi).map(|at| at > gate).unwrap_or(true),
                "{cpi} precedes the standard assertion"
            );
        }
    }

    /// **No vault token account, and that is the twin's cost advantage** — `init` would allocate
    /// a token account for a mint that does not exist, and rent nobody can reclaim. Two claims,
    /// because "not initialised" and "not a token account" are different mistakes.
    #[test]
    fn the_vault_is_an_address_and_never_an_allocation() {
        assert!(flat(CORE_SRC).contains(
            "#[account( seeds = [POSITION_VAULT_SEED, position.key().as_ref()], bump )] \
             pub position_vault: UncheckedAccount<'info>,"
        ));
        assert_eq!(
            crate::common::test_support::accounts_body(CORE_SRC)
                .matches("token::")
                .count(),
            0,
            "a token-account constraint here would allocate the vault the Core path exists without"
        );
    }

    /// The protocol keeps the Core seizure loss, and this handler implements that
    /// rule without adding a switch. One `PoolCollection.standards` bitmask governs
    /// deposits and instant sells alike, so there is no per-standard gate here to find.
    #[test]
    fn the_sell_carries_no_per_standard_switch_of_its_own() {
        // `code_only`, not `production`: the doc comment names `PoolCollection.standards` to
        // explain the rule.
        let code = code_only(CORE_SRC);
        // `.standards` is the shape a switch would take — a field read off `Pool` or off the
        // admission record. The bare token appears once, in `use crate::common::standards::…`,
        // which is the discriminant constant and not a gate.
        assert_eq!(
            code.matches(".standards").count(),
            0,
            "the handler reads an admitted-standards value of its own; the one bitmask is \
             `PoolCollection`'s and it is reached only through the shared predicate"
        );
        assert_eq!(calls(CORE_SRC, "PoolCollection::require_admits"), 1);
    }
}

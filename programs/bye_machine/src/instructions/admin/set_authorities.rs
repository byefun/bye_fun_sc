use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::{AuthorityRole, AuthorityRotated, RotationPhase};
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::ProtocolConfig;

pub fn handler(
    ctx: Context<SetAuthorities>,
    role: AuthorityRole,
    new_authority: Option<Pubkey>,
) -> Result<()> {
    let config = &mut ctx.accounts.protocol_config;
    let signer = ctx.accounts.signer.key();

    let (old, new, phase) = match new_authority {
        Some(proposed) => {
            require_keys_eq!(signer, config.administrator, ByeMachineError::Unauthorized);
            let old = current_authority(config, role);
            *pending_slot(config, role) = proposed;
            (old, proposed, RotationPhase::Proposed)
        }
        None => {
            let pending = *pending_slot(config, role);
            require_keys_neq!(pending, Pubkey::default(), ByeMachineError::Unauthorized);
            require_keys_eq!(signer, pending, ByeMachineError::Unauthorized);
            let old = current_authority(config, role);
            *live_slot(config, role) = signer;
            *pending_slot(config, role) = Pubkey::default();
            (old, signer, RotationPhase::Accepted)
        }
    };

    emit!(AuthorityRotated {
        slot: Clock::get()?.slot,
        authority: signer,
        role,
        old,
        new,
        phase
    });

    Ok(())
}

fn current_authority(config: &ProtocolConfig, role: AuthorityRole) -> Pubkey {
    match role {
        AuthorityRole::Administrator => config.administrator,
        AuthorityRole::Operator => config.operator,
        AuthorityRole::T03 => config.t03_authority,
        AuthorityRole::T02 => config.t02_authority,
    }
}

fn live_slot(config: &mut ProtocolConfig, role: AuthorityRole) -> &mut Pubkey {
    match role {
        AuthorityRole::Administrator => &mut config.administrator,
        AuthorityRole::Operator => &mut config.operator,
        AuthorityRole::T03 => &mut config.t03_authority,
        AuthorityRole::T02 => &mut config.t02_authority,
    }
}

fn pending_slot(config: &mut ProtocolConfig, role: AuthorityRole) -> &mut Pubkey {
    match role {
        AuthorityRole::Administrator => &mut config.pending_administrator,
        AuthorityRole::Operator => &mut config.pending_operator,
        AuthorityRole::T03 => &mut config.pending_t03_authority,
        AuthorityRole::T02 => &mut config.pending_t02_authority,
    }
}

#[derive(Accounts)]
pub struct SetAuthorities<'info> {
    pub signer: Signer<'info>,

    #[account(mut, seeds = [PROTOCOL_CONFIG_SEED], bump = protocol_config.bump)]
    pub protocol_config: Account<'info, ProtocolConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::test_support::production;

    const SET_AUTHORITIES_SRC: &str = include_str!("set_authorities.rs");

    /// The rotation-guard `match` expression (`handler`, `:16-32`), verbatim — from `let (old,
    /// new, phase) = match new_authority {` through its closing `};`, depth-counted on the
    /// braces so a statement inserted anywhere inside either arm extends the span rather than
    /// falling outside it. This is what the whole-block `assert_eq!` below compares against.
    /// The four narrow pins in that test see only the two guard call sites by name — they are
    /// position- and arm-blind, so a shadowed rebinding placed between the guards (`let pending =
    /// signer;` ahead of the accept-path `require_keys_eq!`, making it compare `signer` to
    /// itself) leaves all four green while making rotation acceptance permissionless. Equality
    /// over the whole arm closes that: it sees text added anywhere inside either arm, not just at
    /// the two pinned call sites.
    fn rotation_guard_block(source: &str) -> &str {
        let prod = production(source);
        let anchor = "let (old, new, phase) = match new_authority {";
        assert_eq!(
            prod.matches(anchor).count(),
            1,
            "expected exactly one rotation-guard match expression"
        );
        let start = prod.find(anchor).unwrap();
        let mut depth = 0usize;
        let mut close = None;
        for (i, c) in prod[start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(start + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.expect("rotation-guard match has no matching closing brace");
        let semicolon = prod[close..]
            .find(';')
            .expect("rotation-guard match has no trailing semicolon");
        &prod[start..close + semicolon + 1]
    }

    /// The two prologue statements ahead of the rotation-guard match — `let config =
    /// &mut ctx.accounts.protocol_config;` and `let signer = ctx.accounts.signer.key();` — sit
    /// outside `rotation_guard_block`'s pin, which anchors on the match expression and starts
    /// reading two lines below them. Substituting `let signer = config.administrator;` for the
    /// second prologue line turns both `require_keys_eq!` guards into
    /// `require_keys_eq!(config.administrator, config.administrator)` — always true, byte-
    /// identical guard text, `emit!` still fires and falsely attributes the call to the
    /// administrator. `cargo test` and clippy both pass with the substitution in place; only a
    /// pin over the whole body, prologue included, sees it — the `update_tier.rs`
    /// whole-body-pin shape (`the_handler_body_is_pinned_verbatim_leaving_no_room_for_an_inserted_statement`)
    /// applied here.
    fn handler_body(source: &str) -> &str {
        let prod = production(source);
        let sig = "pub fn handler(";
        assert_eq!(
            prod.matches(sig).count(),
            1,
            "expected exactly one pub fn handler in this file"
        );
        let sig_start = prod.find(sig).unwrap();
        let open = sig_start
            + prod[sig_start..]
                .find('{')
                .expect("handler signature has no opening brace");
        let mut depth = 0usize;
        let mut close = None;
        for (i, c) in prod[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.expect("handler body has no matching closing brace");
        &prod[open..close]
    }

    /// Closes the seam `rotation_guard_block` leaves above its own anchor — a substituted
    /// prologue operand (`config.administrator` in place of `ctx.accounts.signer.key()`) makes
    /// the rotation-phase guards compare a value to itself without changing a single character
    /// the match-block pin or the four `contains`/count guards below can see. This pin has no
    /// such seam: it is the entire handler body, prologue and `Ok(())` included, so any
    /// statement added, removed, reordered or operand-substituted anywhere in the body breaks
    /// it. Kept alongside — not in place of — the narrower pins in the test below: those name
    /// which guard moved when only one changes; this one has no room for any of them to slip
    /// past.
    ///
    /// **When this fails:** update the literal to match the handler's new body — never delete or
    /// weaken this assertion. A failure here means the handler body changed somewhere the
    /// narrower pins did not already catch; confirm the change is intended before updating the
    /// literal.
    #[test]
    fn the_handler_body_is_pinned_verbatim_prologue_included() {
        assert_eq!(
            handler_body(SET_AUTHORITIES_SRC),
            r#"{
    let config = &mut ctx.accounts.protocol_config;
    let signer = ctx.accounts.signer.key();

    let (old, new, phase) = match new_authority {
        Some(proposed) => {
            require_keys_eq!(signer, config.administrator, ByeMachineError::Unauthorized);
            let old = current_authority(config, role);
            *pending_slot(config, role) = proposed;
            (old, proposed, RotationPhase::Proposed)
        }
        None => {
            let pending = *pending_slot(config, role);
            require_keys_neq!(pending, Pubkey::default(), ByeMachineError::Unauthorized);
            require_keys_eq!(signer, pending, ByeMachineError::Unauthorized);
            let old = current_authority(config, role);
            *live_slot(config, role) = signer;
            *pending_slot(config, role) = Pubkey::default();
            (old, signer, RotationPhase::Accepted)
        }
    };

    emit!(AuthorityRotated {
        slot: Clock::get()?.slot,
        authority: signer,
        role,
        old,
        new,
        phase
    });

    Ok(())
}"#,
            "the handler body changed; if intended, update the literal, do not delete or weaken \
             this assertion"
        );
    }

    /// `set_authorities` is the crate's one body-authorized handler — its signer identity is
    /// checked by `require_keys_eq!` in the
    /// handler body, not by an accounts-struct `constraint =`, so the whole-body accounts-struct
    /// pin in `instructions/mod.rs` cannot see either guard by construction. Without this test,
    /// dropping the accept-path guard (`:26`) makes rotation acceptance permissionless, and
    /// substituting the propose-path guard's authority (`config.administrator` →
    /// `config.operator`) lets the operator propose an administrator rotation — both compile,
    /// pass clippy and pass every other test in the crate.
    ///
    /// The four `contains`/count assertions below are position- and arm-blind: they cannot see a
    /// statement inserted *between* the two guards, such as a shadowed `pending` that makes the
    /// accept-path guard compare a value to itself. The final `assert_eq!` against
    /// `rotation_guard_block`'s whole-arm literal closes that — kept last so the narrow
    /// assertions still name *which* guard moved when only one of them is at fault.
    ///
    /// **When this fails:** update the literals to match the handler's new guards — never delete
    /// or weaken this assertion. A failure here means one of the two rotation guards changed;
    /// confirm the change is intended before updating the literal.
    #[test]
    fn both_rotation_phase_guards_are_pinned_verbatim_and_the_pair_is_exhaustive() {
        let prod = production(SET_AUTHORITIES_SRC);
        assert!(
            prod.contains(
                "require_keys_eq!(signer, config.administrator, ByeMachineError::Unauthorized);"
            ),
            "the propose-path authority guard changed"
        );
        assert!(
            prod.contains("require_keys_eq!(signer, pending, ByeMachineError::Unauthorized);"),
            "the accept-path authority guard changed"
        );
        assert_eq!(
            prod.matches("require_keys_eq!").count(),
            2,
            "exactly two require_keys_eq! guards: propose and accept — a third phase or a \
             deleted guard must change this count"
        );
        assert_eq!(
            prod.matches("require_keys_neq!").count(),
            1,
            "the accept path's not-pending guard"
        );
        assert_eq!(
            rotation_guard_block(SET_AUTHORITIES_SRC),
            r#"let (old, new, phase) = match new_authority {
        Some(proposed) => {
            require_keys_eq!(signer, config.administrator, ByeMachineError::Unauthorized);
            let old = current_authority(config, role);
            *pending_slot(config, role) = proposed;
            (old, proposed, RotationPhase::Proposed)
        }
        None => {
            let pending = *pending_slot(config, role);
            require_keys_neq!(pending, Pubkey::default(), ByeMachineError::Unauthorized);
            require_keys_eq!(signer, pending, ByeMachineError::Unauthorized);
            let old = current_authority(config, role);
            *live_slot(config, role) = signer;
            *pending_slot(config, role) = Pubkey::default();
            (old, signer, RotationPhase::Accepted)
        }
    };"#,
            "the rotation guard block changed; if intended, update the literal, do not delete or \
             weaken this assertion"
        );
    }

    // --- the three role-dispatch helpers, enumerated behaviourally over AuthorityRole -------

    /// `current_authority`, `live_slot` and `pending_slot` are the only place `role` selects a
    /// `ProtocolConfig` field — 21 references across this file, all call sites or declarations,
    /// and until this test none of them were exercised directly. Every pin above reads the
    /// call-site text (`*live_slot(config, role) = signer;`), which is unchanged by a cross-wired
    /// arm inside `live_slot` itself — `T02 => &mut config.operator` in place of `&mut
    /// config.t02_authority` makes a T02 rotation acceptance silently overwrite the operator
    /// authority instead. `cargo test` and clippy both pass with that substitution in place;
    /// nothing above sees it, because nothing above calls the helper. The two tests below do.
    ///
    /// The role list is drawn from `AuthorityRole`'s own variants, and the index maps below are
    /// exhaustive `match`es with no wildcard arm, so a fifth `AuthorityRole` variant added later
    /// fails this file to compile rather than silently dropping out of the sweep — the
    /// `claim_nft.rs` `closed_below_floor_is_the_only_admissible_state_and_the_rest_reject_at_6301`
    /// pattern, applied to a three-way field dispatch instead of a single guard.
    ///
    /// Every one of the eight `ProtocolConfig` fields a role can address gets its own
    /// `Pubkey::new_unique()` value, not a shared placeholder — a cross-wired arm returns a
    /// provably wrong value only if the two candidate fields can hold different things. A test
    /// where every field held the same sentinel would pass under any mapping between roles and
    /// slots, a vacuous-assertion shape that has surfaced elsewhere in this crate.
    const ROLES: [AuthorityRole; 4] = [
        AuthorityRole::Administrator,
        AuthorityRole::Operator,
        AuthorityRole::T03,
        AuthorityRole::T02,
    ];

    /// A `ProtocolConfig` with all eight role-addressable fields set to distinct sentinels and
    /// every other field zeroed — the `Pool::blank()` shape, inlined here since `ProtocolConfig`
    /// has no test constructor of its own and this file must not add one to production code.
    fn distinct_config() -> ProtocolConfig {
        ProtocolConfig {
            administrator: Pubkey::new_unique(),
            pending_administrator: Pubkey::new_unique(),
            pending_operator: Pubkey::new_unique(),
            pending_t03_authority: Pubkey::new_unique(),
            pending_t02_authority: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            t03_authority: Pubkey::new_unique(),
            t02_authority: Pubkey::new_unique(),
            protocol_revenue: Pubkey::default(),
            usdc_mint: Pubkey::default(),
            bye_mint: Pubkey::default(),
            vrf_program: Pubkey::default(),
            oracle_queue: Pubkey::default(),
            swap_pool: Pubkey::default(),
            max_swap_slippage_bps: 0,
            pool_counter: 0,
            bump: 0,
        }
    }

    /// The eight role-addressable fields, live slots first then pending, in the fixed order
    /// `live_index`/`pending_index` below index into — the whole-state fingerprint the
    /// write-through test compares before and after, so a write that lands in the wrong field
    /// shows up as a mismatch at that field's position while every other position stays equal.
    fn snapshot(config: &ProtocolConfig) -> [Pubkey; 8] {
        [
            config.administrator,
            config.operator,
            config.t03_authority,
            config.t02_authority,
            config.pending_administrator,
            config.pending_operator,
            config.pending_t03_authority,
            config.pending_t02_authority,
        ]
    }

    /// Independent of `current_authority`/`live_slot`'s own `match` — written by hand against the
    /// field list in [`snapshot`], not by calling the helper under test, so the oracle and its
    /// subject can't share the same bug.
    fn live_index(role: AuthorityRole) -> usize {
        match role {
            AuthorityRole::Administrator => 0,
            AuthorityRole::Operator => 1,
            AuthorityRole::T03 => 2,
            AuthorityRole::T02 => 3,
        }
    }

    fn pending_index(role: AuthorityRole) -> usize {
        match role {
            AuthorityRole::Administrator => 4,
            AuthorityRole::Operator => 5,
            AuthorityRole::T03 => 6,
            AuthorityRole::T02 => 7,
        }
    }

    /// For failure messages only — `AuthorityRole` derives no `Debug`, so `{role:?}` doesn't
    /// compile; exhaustive for the same reason as the two index maps above.
    fn role_name(role: AuthorityRole) -> &'static str {
        match role {
            AuthorityRole::Administrator => "Administrator",
            AuthorityRole::Operator => "Operator",
            AuthorityRole::T03 => "T03",
            AuthorityRole::T02 => "T02",
        }
    }

    #[test]
    fn current_authority_live_slot_and_pending_slot_each_address_the_matching_role() {
        for role in ROLES {
            let mut config = distinct_config();
            let expect = snapshot(&config);

            assert_eq!(
                current_authority(&config, role),
                expect[live_index(role)],
                "current_authority returned the wrong field for {}",
                role_name(role)
            );
            assert_eq!(
                *live_slot(&mut config, role),
                expect[live_index(role)],
                "live_slot addressed the wrong field for {}",
                role_name(role)
            );
            assert_eq!(
                *pending_slot(&mut config, role),
                expect[pending_index(role)],
                "pending_slot addressed the wrong field for {}",
                role_name(role)
            );
        }
    }

    /// The read-only mismatches above already catch a cross-wired mapping, but
    /// `live_slot`/`pending_slot` exist to be written through
    /// (`handler`), so this asserts the write side directly — a sentinel written
    /// through the helper must land in the intended field and change no other of the eight.
    #[test]
    fn live_slot_and_pending_slot_write_through_to_only_the_matching_field() {
        for role in ROLES {
            let mut live_config = distinct_config();
            let before_live = snapshot(&live_config);
            let new_live = Pubkey::new_unique();
            *live_slot(&mut live_config, role) = new_live;
            let mut want_live = before_live;
            want_live[live_index(role)] = new_live;
            assert_eq!(
                snapshot(&live_config),
                want_live,
                "live_slot's write for {} landed in the wrong field, or touched another",
                role_name(role)
            );

            let mut pending_config = distinct_config();
            let before_pending = snapshot(&pending_config);
            let new_pending = Pubkey::new_unique();
            *pending_slot(&mut pending_config, role) = new_pending;
            let mut want_pending = before_pending;
            want_pending[pending_index(role)] = new_pending;
            assert_eq!(
                snapshot(&pending_config),
                want_pending,
                "pending_slot's write for {} landed in the wrong field, or touched another",
                role_name(role)
            );
        }
    }
}

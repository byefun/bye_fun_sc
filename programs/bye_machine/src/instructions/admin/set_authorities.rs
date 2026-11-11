use anchor_lang::prelude::*;

use crate::common::errors::ByeMachineError;
use crate::common::events::{AuthorityRole, AuthorityRotated};
use crate::common::seeds::PROTOCOL_CONFIG_SEED;
use crate::state::ProtocolConfig;

/// Replaces the live authority for `role` in one Administrator-signed step. A key typo takes
/// effect immediately, so the default key and a no-op rotation are refused.
pub fn handler(
    ctx: Context<SetAuthorities>,
    role: AuthorityRole,
    new_authority: Pubkey,
) -> Result<()> {
    let config = &mut ctx.accounts.protocol_config;
    let signer = ctx.accounts.signer.key();

    require_keys_eq!(signer, config.administrator, ByeMachineError::Unauthorized);
    require_keys_neq!(
        new_authority,
        Pubkey::default(),
        ByeMachineError::AuthorityUnset
    );
    let old = current_authority(config, role);
    require_keys_neq!(new_authority, old, ByeMachineError::AuthorityUnchanged);
    *live_slot(config, role) = new_authority;

    emit!(AuthorityRotated {
        slot: Clock::get()?.slot,
        authority: signer,
        role,
        old,
        new: new_authority
    });

    Ok(())
}

/// Every `AuthorityRole` addresses a live slot, so these helpers return plain values rather than
/// `Option`s.
fn current_authority(config: &ProtocolConfig, role: AuthorityRole) -> Pubkey {
    match role {
        AuthorityRole::Administrator => config.administrator,
        AuthorityRole::Operator => config.operator,
        AuthorityRole::T05 => config.t05_authority,
        AuthorityRole::T02 => config.t02_authority,
    }
}

fn live_slot(config: &mut ProtocolConfig, role: AuthorityRole) -> &mut Pubkey {
    match role {
        AuthorityRole::Administrator => &mut config.administrator,
        AuthorityRole::Operator => &mut config.operator,
        AuthorityRole::T05 => &mut config.t05_authority,
        AuthorityRole::T02 => &mut config.t02_authority,
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

    /// The handler body, prologue included.
    fn handler_body(source: &str) -> String {
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
        prod[open..close].to_string()
    }

    /// The whole handler body, prologue included, verbatim: catches a substituted prologue
    /// operand (e.g. `signer` bound to `config.administrator`) that makes the guard trivially
    /// true. Update the literal when the body intentionally changes.
    #[test]
    fn the_handler_body_is_pinned_verbatim_prologue_included() {
        assert_eq!(
            handler_body(SET_AUTHORITIES_SRC),
            r#"{
    let config = &mut ctx.accounts.protocol_config;
    let signer = ctx.accounts.signer.key();

    require_keys_eq!(signer, config.administrator, ByeMachineError::Unauthorized);
    require_keys_neq!(
        new_authority,
        Pubkey::default(),
        ByeMachineError::AuthorityUnset
    );
    let old = current_authority(config, role);
    require_keys_neq!(new_authority, old, ByeMachineError::AuthorityUnchanged);
    *live_slot(config, role) = new_authority;

    emit!(AuthorityRotated {
        slot: Clock::get()?.slot,
        authority: signer,
        role,
        old,
        new: new_authority
    });

    Ok(())
}"#,
            "the handler body changed; if intended, update the literal, do not delete or weaken \
             this assertion"
        );
    }

    /// `set_authorities` authorizes in the handler body (`require_keys_eq!`), not in an
    /// accounts-struct `constraint =`. The single administrator guard must precede every other
    /// check and the write, so a non-administrator learns nothing from the other errors.
    #[test]
    fn the_administrator_guard_is_unique_and_precedes_the_checks_and_the_write() {
        let body = handler_body(SET_AUTHORITIES_SRC);
        let guard =
            "require_keys_eq!(signer, config.administrator, ByeMachineError::Unauthorized);";
        assert_eq!(
            body.matches(guard).count(),
            1,
            "exactly one administrator guard"
        );
        assert_eq!(body.matches("require_keys_eq!").count(), 1);
        let guard_at = body.find(guard).unwrap();
        for later in [
            "ByeMachineError::AuthorityUnset",
            "ByeMachineError::AuthorityUnchanged",
            "*live_slot(config, role) = new_authority;",
        ] {
            assert!(
                guard_at < body.find(later).expect(later),
                "{later} must come after the administrator guard"
            );
        }
    }

    /// The retired two-phase fields are never touched by the handler or its helpers.
    #[test]
    fn the_reserved_slots_are_never_named() {
        assert!(!production(SET_AUTHORITIES_SRC).contains("reserved_"));
    }

    // --- the two role-dispatch helpers, enumerated behaviourally over AuthorityRole ----------

    /// The index maps below are exhaustive `match`es with no wildcard arm, so a new
    /// `AuthorityRole` variant fails to compile here rather than dropping out of the sweep.
    /// Every slot gets its own `Pubkey::new_unique()` so a cross-wired arm returns a provably
    /// wrong value. The array is sized `4` so retiring a role must delete an element.
    const ROTATABLE_ROLES: [AuthorityRole; 4] = [
        AuthorityRole::Administrator,
        AuthorityRole::Operator,
        AuthorityRole::T05,
        AuthorityRole::T02,
    ];

    /// A `ProtocolConfig` with all eight slot fields set to distinct sentinels and
    /// every other field zeroed — the `Pool::blank()` shape, inlined here since `ProtocolConfig`
    /// has no test constructor of its own and this file must not add one to production code.
    fn distinct_config() -> ProtocolConfig {
        ProtocolConfig {
            administrator: Pubkey::new_unique(),
            t04_ceiling: u64::from_le_bytes(
                Pubkey::new_unique().to_bytes()[..8].try_into().unwrap(),
            ),
            reserved_0: Pubkey::new_unique().to_bytes()[8..].try_into().unwrap(),
            reserved_1: Pubkey::new_unique(),
            reserved_2: Pubkey::new_unique(),
            reserved_3: Pubkey::new_unique(),
            operator: Pubkey::new_unique(),
            t05_authority: Pubkey::new_unique(),
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

    /// The four live slots, then the four reserved ones, in the order `live_index` indexes.
    fn snapshot(config: &ProtocolConfig) -> [Pubkey; 8] {
        [
            config.administrator,
            config.operator,
            config.t05_authority,
            config.t02_authority,
            ceiling_slot(config),
            config.reserved_1,
            config.reserved_2,
            config.reserved_3,
        ]
    }

    /// The 32 bytes `t04_ceiling` and `reserved_0` share, read as one slot so a role written into
    /// either half shows up here.
    fn ceiling_slot(config: &ProtocolConfig) -> Pubkey {
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&config.t04_ceiling.to_le_bytes());
        bytes[8..].copy_from_slice(&config.reserved_0);
        Pubkey::new_from_array(bytes)
    }

    /// Hand-written oracle, independent of the helpers' own `match`, so oracle and subject cannot
    /// share a bug.
    fn live_index(role: AuthorityRole) -> usize {
        match role {
            AuthorityRole::Administrator => 0,
            AuthorityRole::Operator => 1,
            AuthorityRole::T05 => 2,
            AuthorityRole::T02 => 3,
        }
    }

    /// For failure messages only — `AuthorityRole` derives no `Debug`.
    fn role_name(role: AuthorityRole) -> &'static str {
        match role {
            AuthorityRole::Administrator => "Administrator",
            AuthorityRole::Operator => "Operator",
            AuthorityRole::T05 => "T05",
            AuthorityRole::T02 => "T02",
        }
    }

    #[test]
    fn current_authority_and_live_slot_each_address_the_matching_role() {
        for role in ROTATABLE_ROLES {
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
        }
    }

    /// The four roles address four distinct slots: no two roles resolve to the same field.
    #[test]
    fn no_two_roles_address_the_same_slot() {
        let mut config = distinct_config();
        let mut live: Vec<Pubkey> = Vec::new();
        for role in ROTATABLE_ROLES {
            let current = current_authority(&config, role);
            assert_eq!(
                *live_slot(&mut config, role),
                current,
                "live_slot and current_authority disagree for {}",
                role_name(role)
            );
            live.push(current);
        }
        live.sort();
        live.dedup();
        assert_eq!(
            live.len(),
            ROTATABLE_ROLES.len(),
            "two roles share a live slot"
        );
    }

    /// A sentinel written through `live_slot` lands in the intended field and changes no other of
    /// the eight, the four reserved fields included.
    #[test]
    fn live_slot_writes_through_to_only_the_matching_field() {
        for role in ROTATABLE_ROLES {
            let mut config = distinct_config();
            let before = snapshot(&config);
            let new_live = Pubkey::new_unique();
            *live_slot(&mut config, role) = new_live;
            let mut want = before;
            want[live_index(role)] = new_live;
            assert_eq!(
                snapshot(&config),
                want,
                "live_slot's write for {} landed in the wrong field, or touched another",
                role_name(role)
            );
        }
    }
}

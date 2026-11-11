pub mod claim_fee_to_escrow;
pub mod escrow_deposit;
pub mod escrow_withdraw;
pub mod open_escrow;
pub mod set_escrow_armed;

// Only the `Accounts` context types are re-exported; handlers are called by full module path
// in lib.rs, since every one of them is named `handler`.
pub use claim_fee_to_escrow::ClaimFeeToEscrow;
pub use escrow_deposit::EscrowDeposit;
pub use escrow_withdraw::EscrowWithdraw;
pub use open_escrow::OpenEscrow;
pub use set_escrow_armed::SetEscrowArmed;

/// Seeded random sequences over the pure halves of the escrow instructions. The on-chain half of
/// each property — the vault read back after a real transfer, the seeds refusing another signer —
/// is driven by the escrow integration suite.
#[cfg(test)]
mod properties {
    use super::escrow_withdraw::withdrawal_balance_after;
    use super::set_escrow_armed::apply_armed;
    use crate::state::Escrow;
    use anchor_lang::prelude::Pubkey;

    const SEEDS: u64 = 64;
    const STEPS: usize = 500;

    /// SplitMix64: deterministic, so a failing seed reproduces exactly.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
        fn below(&mut self, bound: u64) -> u64 {
            self.next() % bound
        }
    }

    /// The balance never goes below zero, a refused withdrawal leaves it exactly where it was,
    /// and an accepted one lowers it by exactly the amount. Amounts straddle the balance on
    /// purpose, so roughly half the withdrawals are refused.
    #[test]
    fn random_deposits_and_withdrawals_never_take_the_balance_below_zero() {
        let mut refused = 0usize;
        let mut accepted = 0usize;
        for seed in 0..SEEDS {
            let mut rng = Rng(seed);
            let mut balance: u64 = 0;
            for _ in 0..STEPS {
                if rng.below(2) == 0 {
                    balance += rng.below(50_000_000);
                } else {
                    let amount = rng.below(balance.saturating_mul(2).max(2));
                    match withdrawal_balance_after(amount, balance) {
                        Ok(after) => {
                            assert!(amount > 0 && amount <= balance, "seed {seed}");
                            assert_eq!(after, balance - amount, "seed {seed}");
                            balance = after;
                            accepted += 1;
                        }
                        Err(_) => {
                            assert!(amount == 0 || amount > balance, "seed {seed}");
                            refused += 1;
                        }
                    }
                }
            }
        }
        assert!(
            accepted > 1_000 && refused > 1_000,
            "{accepted} accepted, {refused} refused"
        );
    }

    /// Whatever sequence led to it, a non-zero balance is always withdrawable in full and leaves
    /// exactly zero.
    #[test]
    fn the_full_balance_is_always_withdrawable_after_any_sequence() {
        for seed in 0..SEEDS {
            let mut rng = Rng(seed);
            let mut balance: u64 = 0;
            for _ in 0..STEPS {
                balance += rng.below(50_000_000);
                if let Ok(after) = withdrawal_balance_after(rng.below(balance.max(1)), balance) {
                    balance = after;
                }
                if balance > 0 {
                    assert_eq!(withdrawal_balance_after(balance, balance).unwrap(), 0);
                }
            }
        }
    }

    /// `armed` always equals the last value requested, and a change is reported exactly when the
    /// value differs — the count of reported changes is the count of `EscrowArmed` events.
    #[test]
    fn armed_follows_the_last_request_and_reports_exactly_the_changes() {
        for seed in 0..SEEDS {
            let mut rng = Rng(seed);
            let mut escrow = Escrow {
                owner: Pubkey::new_unique(),
                armed: false,
                bump: 254,
                vault_bump: 253,
                reserved: [0; 64],
            };
            let mut expected_changes = 0usize;
            let mut reported_changes = 0usize;
            for _ in 0..STEPS {
                let requested = rng.below(2) == 1;
                if requested != escrow.armed {
                    expected_changes += 1;
                }
                if apply_armed(&mut escrow, requested) {
                    reported_changes += 1;
                }
                assert_eq!(escrow.armed, requested, "seed {seed}");
            }
            assert_eq!(reported_changes, expected_changes, "seed {seed}");
        }
    }
}

use anchor_lang::prelude::{pubkey, Pubkey};

// Program ID bound to accounts/dev/bye_machine-keypair.json, docker-compose.yml,
// scripts/constants.ts, and Anchor.toml [programs.*]. Keep all of these in sync.
cfg_if::cfg_if! {
    if #[cfg(feature = "prod")] {
        pub const PROGRAM_ID: Pubkey = pubkey!("4exRPJN7N8MVZh37efYAkwnGGxoJKMVUARQqf7awmYkJ");
    } else if #[cfg(feature = "dev")] {
        pub const PROGRAM_ID: Pubkey = pubkey!("4exRPJN7N8MVZh37efYAkwnGGxoJKMVUARQqf7awmYkJ");
    } else {
        pub const PROGRAM_ID: Pubkey = pubkey!("4exRPJN7N8MVZh37efYAkwnGGxoJKMVUARQqf7awmYkJ");
    }
}

// The only key `init_protocol` accepts as `payer`, and therefore the only key that can become
// the first Administrator. Deploy and init are two transactions; without this pin, `init_protocol`
// is permissionless between them and the first signer takes the root of the whole privilege set
// (`update_config`, `set_pause`, `admit_collection`/`withdraw_collection`, and — via
// `set_authorities` — Operator, T03 and T02 as well). Enforced in `init_protocol.rs` as a
// `constraint`, not an `address`: `address` on an ALL-CAPS const path is emitted into the IDL
// (anchor-syn's `idl::accounts::get_address`) and this constant varies per feature, which would
// make the committed `target/idl/bye_machine.json` environment-specific. See D-156.
//
// This is only the INITIAL value. `set_authorities` rotates Administrator in two phases
// (current admin proposes, the proposed key accepts), so a pinned-wrong bootstrap is recoverable
// without a redeploy — and a pinned-wrong build fails CLOSED: `init_protocol` refuses rather than
// installing the wrong administrator.
// BRANCH ORDER IS LOAD-BEARING, and it is not the same shape as `PROGRAM_ID`'s above — that one
// carries the same value in every arm, so its arm order has never mattered. Here `dev` alone
// cannot select the key, because `dev` names TWO environments: `docker-compose.yml`'s `build`
// service compiles the artifact the localnet integration suite runs against with
// `--features dev,testing`, while `deploy-devnet` compiles the real devnet artifact with
// `--features dev`. `testing` is what separates them — it is the suite marker, and README's
// "Never build the devnet/mainnet artifact with the `testing` feature" is what keeps it so — so
// it must be tested BEFORE `dev`. Resolving order and the builds that reach each arm:
//
//   prod              -> mainnet            (`cargo build-sbf --features prod`)
//   testing           -> localnet suite     (`--features dev,testing`, docker `build` service)
//   dev               -> devnet             (`--features dev`, `deploy-devnet`)
//   (no features)     -> localnet suite     (`anchor build`, the host path in README)
cfg_if::cfg_if! {
    if #[cfg(feature = "prod")] {
        // PLACEHOLDER — generated, nobody holds the secret on purpose. D-156 must replace this
        // with the real mainnet Administrator before the first `--features prod` build; v1.0
        // carries no multisig, so this is a single key and its custody is the decision's subject.
        pub const INITIAL_ADMINISTRATOR: Pubkey =
            pubkey!("2XaggZw959STu3UaCGtWDvTq6htgEBxKevysuEAJ6m4n");
    } else if #[cfg(feature = "testing")] {
        // `accounts/dev/deployer.json` — the committed throwaway the localnet suite signs with
        // (`tests/helpers/env.ts`'s `loadSuiteAdmin`, airdropped by
        // `scripts/run-integration-tests.sh`). Must equal the no-features arm below, so the
        // docker and host test paths bootstrap with the same key.
        pub const INITIAL_ADMINISTRATOR: Pubkey =
            pubkey!("DCe2NVuNGg8yqNPZq5PChJtjJV7DRPVxhQr6eSVy11np");
    } else if #[cfg(feature = "dev")] {
        // Same placeholder as `prod`, and deliberately NOT the committed throwaway: devnet init
        // stays blocked until this is replaced with a key whose secret the devnet operator holds.
        // A fail-closed gap, not an oversight — `init_protocol` refuses rather than installing an
        // administrator anybody could sign as.
        pub const INITIAL_ADMINISTRATOR: Pubkey =
            pubkey!("2XaggZw959STu3UaCGtWDvTq6htgEBxKevysuEAJ6m4n");
    } else {
        // `anchor build` — the host localnet path. `tests/unit/initial-administrator.test.ts`
        // pins this literal against `accounts/dev/deployer.json`, so rotating that keypair fails
        // as one named fixture error instead of as every integration file at once.
        pub const INITIAL_ADMINISTRATOR: Pubkey =
            pubkey!("DCe2NVuNGg8yqNPZq5PChJtjJV7DRPVxhQr6eSVy11np");
    }
}

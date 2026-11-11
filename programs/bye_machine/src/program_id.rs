use anchor_lang::prelude::{pubkey, Pubkey};

// Program IDs per environment. The localnet ID (the `testing` and no-features arms) is bound to
// the local throwaway accounts/dev/bye_machine-keypair.json; keep it in sync with
// docker-compose.yml, scripts/constants.ts, scripts/lib/legacy-pool-fixtures.ts and
// scripts/run-integration-tests.sh. `testing` is tested before `dev` because the localnet suite
// artifact is built with `--features dev,testing`.
cfg_if::cfg_if! {
    if #[cfg(feature = "prod")] {
        pub const PROGRAM_ID: Pubkey = pubkey!("ByeM1a8XdZb6dCcUh664cDNnfnVtAwrSf9fjncygMkxU");
    } else if #[cfg(feature = "testing")] {
        pub const PROGRAM_ID: Pubkey = pubkey!("szKvUQJx6USMBe6J2yNrd16jEAF98Ys82GdeBP93yqG");
    } else if #[cfg(feature = "dev")] {
        pub const PROGRAM_ID: Pubkey = pubkey!("ZDrZoJpGA7w5JkEnDUf2AG8KRLg1fEXdqCCGwVmVSh1");
    } else {
        pub const PROGRAM_ID: Pubkey = pubkey!("szKvUQJx6USMBe6J2yNrd16jEAF98Ys82GdeBP93yqG");
    }
}

// The only key `init_protocol` accepts as `payer`, and therefore the only key that can become
// the first Administrator. Deploy and init are two transactions; without this restriction,
// `init_protocol` is permissionless between them and the first signer takes the root of the
// whole privilege set. Enforced in `init_protocol.rs` as a `constraint`, not an `address`:
// `address` on an ALL-CAPS const path is emitted into the IDL and this constant varies per
// feature, which would make the committed `idl/bye_machine.json` environment-specific.
//
// Only the INITIAL value: `set_authorities` can replace the Administrator without a redeploy,
// but only while someone still holds the bootstrap key and can sign with it. A key nobody holds
// leaves `init_protocol` unusable — it fails closed, and recovery is a rebuild and redeploy.
//
// BRANCH ORDER IS LOAD-BEARING. `dev` names two environments: the localnet suite artifact
// (`--features dev,testing`) and the real devnet artifact (`--features dev`). `testing`
// separates them, so it must be tested BEFORE `dev`. The devnet/mainnet artifact must never be
// built with `testing`.
//
//   prod              -> mainnet            (`cargo build-sbf --features prod`)
//   testing           -> localnet suite     (`--features dev,testing`, docker `build` service)
//   dev               -> devnet             (`--features dev`)
//   (no features)     -> localnet suite     (`anchor build`, the host path in README)
cfg_if::cfg_if! {
    if #[cfg(feature = "prod")] {
        // PLACEHOLDER — generated, nobody holds the secret on purpose. Replace with the real
        // mainnet Administrator before the first `--features prod` build.
        pub const INITIAL_ADMINISTRATOR: Pubkey =
            pubkey!("admUem84hPuS2uyZGP1AiXrqW7Gr89bv5BRjGWNuRWR");
    } else if #[cfg(feature = "testing")] {
        // `accounts/dev/deployer.json` — the local throwaway the localnet suite signs with
        // (`tests/helpers/env.ts`'s `loadSuiteAdmin`, airdropped by
        // `scripts/run-integration-tests.sh`). Must equal the no-features arm below, so the
        // docker and host test paths bootstrap with the same key.
        pub const INITIAL_ADMINISTRATOR: Pubkey =
            pubkey!("DCe2NVuNGg8yqNPZq5PChJtjJV7DRPVxhQr6eSVy11np");
    } else if #[cfg(feature = "dev")] {
        // The devnet Administrator — deliberately NOT the localnet throwaway `deployer.json`.
        // `init_protocol` accepts no other signer, and the role it installs carries
        // `set_authorities` over every other authority.
        pub const INITIAL_ADMINISTRATOR: Pubkey =
            pubkey!("HEdUF7Xigs5LdGDrgJDoKJVg4ZX79M7yktSDJPtuHqDc");
    } else {
        // `anchor build` — the host localnet path. `tests/unit/initial-administrator.test.ts`
        // pins this literal against `accounts/dev/deployer.json`, so rotating that keypair fails
        // as one named fixture error instead of as every integration file at once.
        pub const INITIAL_ADMINISTRATOR: Pubkey =
            pubkey!("DCe2NVuNGg8yqNPZq5PChJtjJV7DRPVxhQr6eSVy11np");
    }
}

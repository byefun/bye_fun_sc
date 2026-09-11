# bye_machine

Solana on-chain program built with Anchor, using **MagicBlock EphemeralVrf** for verifiable
on-chain randomness.

## MagicBlock VRF integration

Randomness is requested and consumed across two instructions:

| Instruction    | Role                                                                                                                                                        |
| -------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `commit_rolls` | Opens a batch and requests randomness. Builds the request via `create_request_scoped_randomness_ix`, pinning the callback's account metas into the request. |
| `vrf_callback` | Consumes the randomness. Invocable only by the VRF program — no user or protocol authority can call it.                                                     |

The SDK is pinned in `programs/bye_machine/Cargo.toml`:

```toml
ephemeral-vrf-sdk = { version = "=0.4.1", features = ["anchor-compat"] }
```

`anchor-compat` is load-bearing. The crate's default `anchor` feature aliases `anchor-modern` →
`anchor-lang 1.0`, which does not typecheck against this program's 0.31.1 (E0308 on every
`Pubkey` crossing the boundary). `anchor-compat` selects `anchor-lang >=0.28,<1.0` and resolves
onto the one already in the tree.

### Callback authentication

The callback's sole signer is the VRF program's **scoped** identity,
`PDA([b"identity", bye_machine], vrf)` — not the identity that signs the request:

```rust
#[account(address = ephemeral_vrf_sdk::consts::scoped_vrf_identity(&crate::ID))]
pub vrf_program_identity: Signer<'info>,
```

Substituting the request-side identity, the provider's legacy global identity, or any program
authority fails the address constraint. Dropping the constraint entirely is the provider's
documented top integration footgun — it leaves the callback invocable by anyone. The
`#[vrf_callback]` macro is kept on the struct as a fail-safe: delete the field and the macro puts
the signer back.

`VRF_TIMEOUT_SLOTS` (`common/constants.rs`) bounds how long a batch waits on the queue before it
becomes recoverable. `update_vrf_config` rotates the VRF program, oracle queue and swap pool
addresses post-initialisation.

The EphemeralVrf program ID is identical on devnet and mainnet-beta, so integration work ports
without an address swap (`scripts/constants.ts`).

## Layout

```
programs/bye_machine/    the Anchor program
  src/lib.rs             #[program] module — one thin fn per instruction
  src/program_id.rs      cfg_if over prod / dev / default program IDs
  src/common/            constants, errors, events, seeds, math, guards, vrf_queue
  src/state/             one #[account] per file, #[derive(InitSpace)]
  src/instructions/       <domain>/<instruction>.rs — handler() + #[derive(Accounts)]
scripts/                 TS instruction encoders and operational helpers
  constants.ts           cluster + program ID + mints + VRF addresses
  lib/anchor.ts          Borsh writer, discriminators, PDA derivation
tests/unit/              validator-free tests (CI)
tests/integration/       on-chain tests (need a validator)
tests/fixtures/vrf/      captured VRF oracle-queue account layouts
idl/                     generated IDLs, committed per environment
```

Feature gating is load-bearing: `dev`/`prod` select cluster addresses in `program_id.rs` and
`common/constants.rs`; `testing` gates instructions that must never exist on a real cluster.
**Never build a devnet or mainnet artifact with the `testing` feature.**

## Requirements

| Tool           | Version | Notes                                                    |
| -------------- | ------- | -------------------------------------------------------- |
| Solana CLI     | 4.0.3+  | SIMD-0500 active on devnet — CLI 4.x required for SBPFv3 |
| platform-tools | v1.54+  | Required for `--arch v3`                                 |
| Anchor CLI     | 0.31.1  | Matches `Anchor.toml [toolchain]`                        |
| Rust           | 1.86.0  | Pinned in the Docker image and CI                        |
| Node           | 22+     | pnpm 10.6.1                                              |

## Build / test

```bash
pnpm install
pnpm test                 # unit tests — no validator
pnpm run typecheck
pnpm run test:localnet    # boots a validator, runs tests/integration, tears down
cargo test -p bye_machine

anchor build              # localnet
cargo build-sbf --features dev --arch v3 --tools-version v1.54 \
  --manifest-path programs/bye_machine/Cargo.toml
```

### Docker (no host toolchain)

```bash
cp .env.docker.example .env.docker

docker compose run --rm build        # cargo build-sbf
docker compose up -d validator       # test validator (depends on build)
docker compose run --rm test         # unit + integration tests
docker compose down
```

Image is **linux/amd64 only** (Agave CLI has no aarch64 builds); runs under Rosetta on Apple
Silicon. First build takes ~5–10 min.

## Deploy

```bash
solana config set --url devnet --keypair ./accounts/dev/deployer.json
solana program deploy target/deploy/bye_machine.so \
  --program-id ./accounts/dev/bye_machine-keypair.json
```

CI deploy: the **Deploy to Devnet** workflow (`workflow_dispatch`) needs the `DEPLOYER_KEYPAIR`
and `PROGRAM_KEYPAIR` secrets.

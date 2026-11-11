// Cluster + program addresses shared by scripts/ and tests/.
//
// Keep the program IDs in sync with programs/bye_machine/src/program_id.rs,
// docker-compose.yml and scripts/run-integration-tests.sh.

import { address } from "@solana/kit";
import type { Address } from "@solana/kit";

// ── Native / SPL programs ────────────────────────────────────────────────────
export const SYSTEM_PROGRAM = address("11111111111111111111111111111111");
export const TOKEN_PROGRAM = address("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
export const TOKEN_2022_PROGRAM = address("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
export const ASSOCIATED_TOKEN_PROGRAM = address("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
export const ED25519_PROGRAM = address("Ed25519SigVerify111111111111111111111111111");
export const SYSVAR_INSTRUCTIONS = address("Sysvar1nstructions1111111111111111111111111");
export const SYSVAR_SLOT_HASHES = address("SysvarS1otHashes111111111111111111111111111");
export const COMPUTE_BUDGET_PROGRAM = address("ComputeBudget111111111111111111111111111111");

// ── Metaplex ─────────────────────────────────────────────────────────────────
// Collector Crypt Token Metadata cards are pNFTs.
export const TOKEN_METADATA_PROGRAM = address("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");

/** Meteora DAMM v2 — the venue a drawn blank's face is swapped through. */
export const CP_AMM_PROGRAM = address("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");

/** The venue's single global authority, the same address for every pool it hosts. */
export const CP_AMM_POOL_AUTHORITY = address("HLnpSz9h2S4hiLQ43rnSD9XkcUThA7B8hQMKmDaiTLcC");

/** Seed of the venue's Anchor event-authority PDA. */
export const CP_AMM_EVENT_AUTHORITY_SEED = "__event_authority";
// Token Auth Rules — same canonical address on every cluster; required for pNFT transfers.
export const AUTH_RULES_PROGRAM = address("auth9SigNpDKz4sJJ1DfCTuZrZNSAgh9sFD3rboVmgg");
// MPL Core — the second Metaplex NFT standard, and the one whose assets carry no mint, no
// token account and no master edition. Same canonical address on every cluster.
export const MPL_CORE_PROGRAM = address("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

// ── MagicBlock EphemeralVrf ──────────────────────────────────────────────────
// Same program ID on devnet and mainnet-beta (devnet/mainnet parity is exact), so
// integration work ports without an address swap.
export const VRF_PROGRAM = address("Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz");

// ── Collector Crypt collections ──────────────────────────────────────────────
// Two collections share the name "Collector Crypt" under one update authority.
export const COLLECTOR_CRYPT_TOKEN_METADATA_COLLECTION = address(
  "CCryptWBYktukHDQ2vHGtVcmtjXxYzvw8XNVY64YN2Yf",
);
export const COLLECTOR_CRYPT_MPL_CORE_COLLECTION = address(
  "CCryptUfeFSZ3Fgc9FLeKrhLVAP67FSqi1GuVoj9CRac",
);

// ── Cluster selection ────────────────────────────────────────────────────────
const cluster = process.env.CLUSTER ?? "localnet";
if (cluster !== "localnet" && cluster !== "devnet" && cluster !== "mainnet-beta") {
  throw new Error(`Unknown CLUSTER: ${cluster}. Use "localnet", "devnet" or "mainnet-beta".`);
}

type ClusterEnv = {
  cluster: "localnet" | "devnet" | "mainnet-beta";
  rpcUrl: string;
  wsUrl: string;
  programId: Address;
  deployerKeypairPath: string;
  usdcMint: Address;
  /** $BYE mint — not deployed yet. */
  byeMint: Address | null;
  /**
   * The one address lookup table this cluster's scripts compress transactions against. One table serves every pool: a transaction pays one
   * index byte per key it resolves and nothing for the entries it does not touch.
   *
   * `null` means no table exists on this cluster yet — a caller without one sends the same
   * instruction uncompressed, which is correct for everything but a full-width `settle_roll`.
   */
  lookupTable: Address | null;
};

export const env: ClusterEnv =
  cluster === "mainnet-beta"
    ? {
        cluster: "mainnet-beta",
        rpcUrl: "https://api.mainnet-beta.solana.com",
        wsUrl: "wss://api.mainnet-beta.solana.com",
        programId: address("ByeM1a8XdZb6dCcUh664cDNnfnVtAwrSf9fjncygMkxU"),
        deployerKeypairPath: "./accounts/prod/deployer.json",
        usdcMint: address("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        byeMint: null,
        lookupTable: null,
      }
    : cluster === "devnet"
    ? {
        cluster: "devnet",
        rpcUrl: "https://api.devnet.solana.com",
        wsUrl: "wss://api.devnet.solana.com",
        programId: address("ZDrZoJpGA7w5JkEnDUf2AG8KRLg1fEXdqCCGwVmVSh1"),
        deployerKeypairPath: "./accounts/dev/deployer.json",
        usdcMint: address("Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr"),
        byeMint: null, // TODO: devnet test $BYE mint
        lookupTable: address("9UH7jvGgfJbZT7gNi2eXjKX71FdkG7NygtMAUvLc4xEW"),
      }
    : {
        // The localnet suite: `scripts/run-integration-tests.sh` loads the program at this ID.
        cluster: "localnet",
        rpcUrl: "http://127.0.0.1:8899",
        wsUrl: "ws://127.0.0.1:8900",
        programId: address("szKvUQJx6USMBe6J2yNrd16jEAF98Ys82GdeBP93yqG"),
        deployerKeypairPath: "./accounts/dev/deployer.json",
        usdcMint: address("USDCmintFixture1111111111111111111111111111"),
        byeMint: null,
        lookupTable: null,
      };

export const rpcUrl = process.env.RPC_URL ?? env.rpcUrl;
export const wsUrl = process.env.WSS_URL ?? env.wsUrl;

/** Deployed program ID for the selected cluster. Every PDA derives against this. */
export const PROGRAM_ID = env.programId;

/** The selected cluster's published lookup table. Its contents are read on chain. */
export const LOOKUP_TABLE = env.lookupTable;

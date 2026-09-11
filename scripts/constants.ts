// Cluster + program addresses shared by scripts/ and tests/.
//
// PROGRAM_ID is a placeholder bound to accounts/dev/bye_machine-keypair.json. Keep it in
// sync with Anchor.toml, docker-compose.yml, scripts/run-integration-tests.sh, and
// .github/workflows/deploy-devnet.yml.

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
// The Token Metadata pNFT standard.
export const TOKEN_METADATA_PROGRAM = address("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");
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
// Two collections share the name "Collector Crypt" under one update authority: one under
// Token Metadata, one under MPL Core.
export const COLLECTOR_CRYPT_TOKEN_METADATA_COLLECTION = address(
  "CCryptWBYktukHDQ2vHGtVcmtjXxYzvw8XNVY64YN2Yf",
);
export const COLLECTOR_CRYPT_MPL_CORE_COLLECTION = address(
  "CCryptUfeFSZ3Fgc9FLeKrhLVAP67FSqi1GuVoj9CRac",
);

// ── Cluster selection ────────────────────────────────────────────────────────
const cluster = process.env.CLUSTER ?? "devnet";
if (cluster !== "devnet" && cluster !== "mainnet-beta") {
  throw new Error(`Unknown CLUSTER: ${cluster}. Use "devnet" or "mainnet-beta".`);
}

type ClusterEnv = {
  cluster: "devnet" | "mainnet-beta";
  rpcUrl: string;
  wsUrl: string;
  programId: Address;
  deployerKeypairPath: string;
  usdcMint: Address;
  /** $BYE mint — not deployed yet. */
  byeMint: Address | null;
};

export const env: ClusterEnv =
  cluster === "mainnet-beta"
    ? {
        cluster: "mainnet-beta",
        rpcUrl: "https://api.mainnet-beta.solana.com",
        wsUrl: "wss://api.mainnet-beta.solana.com",
        // TODO: set once the program is deployed to mainnet. Placeholder = System program
        // so an accidental mainnet run fails loudly instead of touching a real program.
        programId: SYSTEM_PROGRAM,
        deployerKeypairPath: "./accounts/mainnet/deployer.json",
        usdcMint: address("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"),
        byeMint: null, // TODO: mainnet $BYE mint (post-TGE)
      }
    : {
        cluster: "devnet",
        rpcUrl: "https://api.devnet.solana.com",
        wsUrl: "wss://api.devnet.solana.com",
        programId: address("4exRPJN7N8MVZh37efYAkwnGGxoJKMVUARQqf7awmYkJ"),
        deployerKeypairPath: "./accounts/dev/deployer.json",
        usdcMint: address("Gh9ZwEmdLJ8DscKNTkTqPbNwLNNBjuSzaG9Vp2KGtKJr"),
        byeMint: null, // TODO: devnet test $BYE mint
      };

export const rpcUrl = process.env.RPC_URL ?? env.rpcUrl;
export const wsUrl = process.env.WSS_URL ?? env.wsUrl;

/** Deployed program ID for the selected cluster. Every PDA derives against this. */
export const PROGRAM_ID = env.programId;

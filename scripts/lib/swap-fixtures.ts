/**
 * Addresses of the three accounts the integration validator loads for the blank settlement path.
 *
 * One home for the literals, so the generator that writes the fixture bytes and the helpers that
 * point the protocol at them cannot name different accounts. `scripts/run-integration-tests.sh`
 * repeats them as shell literals — `tests/unit/swap-fixtures.test.ts` asserts all three agree
 * with the committed files.
 *
 * Nothing signs for any of them: `--account` loads state, not a keypair.
 */
import type { Address } from "@solana/kit";

/** The pinned DAMM v2 pool, `protocol_config.swap_pool`. */
export const SWAP_POOL_FIXTURE = "BYEpoo1Fixture11111111111111111111111111111" as Address;

/** $BYE — nine decimals, `token_a` of the pool above. */
export const BYE_MINT_FIXTURE = "BYEmint1Fixture1111111111111111111111111111" as Address;

/** USDC — six decimals, `token_b`. */
export const USDC_MINT_FIXTURE = "USDCmintFixture1111111111111111111111111111" as Address;

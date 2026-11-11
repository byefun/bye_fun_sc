// The account-substitution sweep: one row per `state`/`token_or_metadata` slot, substituting a
// same-typed sibling account and asserting the rejection.
//
// Transcribed against the crate's own census (`sweep_census_matches_the_committed_fixture`) —
// `SWEEP_SLOTS` is every `State`/`TokenOrMetadata` slot the crate declares, in the crate's own
// classification, so a field added, renamed, or reclassified there is a cell this file no longer
// matches rather than one silently missing. `protocol_config` and the two const-only PDAs are excluded by construction:
// they are never `State`/`TokenOrMetadata` in the crate's own partition, because no sibling can
// exist for a singleton.
//
// **The sweep's own blind spot, restated for this file's callers**: a `FullySelf` slot's seeds
// check passes for *any* sibling, because every discriminating component is read off the account
// being substituted — so `anchor` alone does not say what rejects a row. Each executed row below
// documents the actual mechanism (a cross-account constraint, a handler-body `require!`, or
// neither) rather than assuming the seeds check does the work.

export type SweepClass = "State" | "TokenOrMetadata";
export type SeedAnchorKind = "Singleton" | "FullySelf" | "Partial" | "External" | "NotSeeded";

export interface SweepSlot {
  instruction: string;
  field: string;
  class: SweepClass;
  anchor: SeedAnchorKind;
}

/** Every slot the crate's account-slot census counts as `State` or `TokenOrMetadata` — 320 in
 * total (205 + 109). */
export const SWEEP_SLOTS: readonly SweepSlot[] = [
  // ── admit_collection ──
  { instruction: "admit_collection", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "admit_collection", field: "pool_collection", class: "State", anchor: "External" },
  // ── consolidate_treasuries ──
  { instruction: "consolidate_treasuries", field: "legacy_treasury_03", class: "State", anchor: "External" },
  { instruction: "consolidate_treasuries", field: "legacy_treasury_04", class: "State", anchor: "External" },
  { instruction: "consolidate_treasuries", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "consolidate_treasuries", field: "treasury_03", class: "State", anchor: "Singleton" },
  { instruction: "consolidate_treasuries", field: "treasury_04", class: "State", anchor: "Singleton" },
  { instruction: "consolidate_treasuries", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── init_pool ──
  { instruction: "init_pool", field: "pool", class: "State", anchor: "External" },
  { instruction: "init_pool", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "init_pool", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "init_pool", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "init_pool", field: "treasury_03", class: "State", anchor: "Singleton" },
  { instruction: "init_pool", field: "treasury_04", class: "State", anchor: "Singleton" },
  { instruction: "init_pool", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── migrate_pool ──
  { instruction: "migrate_pool", field: "pool", class: "State", anchor: "NotSeeded" },
  // ── set_pause ──
  { instruction: "set_pause", field: "pool", class: "State", anchor: "FullySelf" },
  // ── update_config ──
  { instruction: "update_config", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "update_config", field: "top_tier", class: "State", anchor: "External" },
  // ── sweep_t04_overflow ──
  { instruction: "sweep_t04_overflow", field: "treasury_03", class: "State", anchor: "Singleton" },
  { instruction: "sweep_t04_overflow", field: "treasury_04", class: "State", anchor: "Singleton" },

  // ── update_bye_mint ──
  { instruction: "update_bye_mint", field: "bye_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── update_protocol_revenue ──
  { instruction: "update_protocol_revenue", field: "protocol_revenue", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── withdraw_collection ──
  { instruction: "withdraw_collection", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "withdraw_collection", field: "pool_collection", class: "State", anchor: "External" },
  // ── deposit ──
  { instruction: "deposit", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "deposit", field: "pool_collection", class: "State", anchor: "NotSeeded" },
  { instruction: "deposit", field: "position", class: "State", anchor: "External" },
  { instruction: "deposit", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "deposit", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "deposit", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "deposit", field: "depositor_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "deposit", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "deposit", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "deposit", field: "depositor_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "deposit", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── deposit_core ──
  { instruction: "deposit_core", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "deposit_core", field: "pool_collection", class: "State", anchor: "NotSeeded" },
  { instruction: "deposit_core", field: "position", class: "State", anchor: "External" },
  { instruction: "deposit_core", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "deposit_core", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "deposit_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "deposit_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── approve_deposit ──
  { instruction: "approve_deposit", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "approve_deposit", field: "position", class: "State", anchor: "Partial" },
  { instruction: "approve_deposit", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "approve_deposit", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "approve_deposit", field: "wallet_stats", class: "State", anchor: "External" },
  // ── reject_deposit ──
  { instruction: "reject_deposit", field: "position", class: "State", anchor: "FullySelf" },
  // ── return_rejected ──
  { instruction: "return_rejected", field: "depositor", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "return_rejected", field: "position", class: "State", anchor: "FullySelf" },
  { instruction: "return_rejected", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "return_rejected", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "return_rejected", field: "depositor_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "return_rejected", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "return_rejected", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "return_rejected", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "return_rejected", field: "depositor_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── return_rejected_core ──
  { instruction: "return_rejected_core", field: "depositor", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "return_rejected_core", field: "position", class: "State", anchor: "FullySelf" },
  { instruction: "return_rejected_core", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "return_rejected_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "return_rejected_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── begin_sweep ──
  { instruction: "begin_sweep", field: "pool", class: "State", anchor: "FullySelf" },
  // ── claim_fee ──
  { instruction: "claim_fee", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "claim_fee", field: "position", class: "State", anchor: "Partial" },
  { instruction: "claim_fee", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "claim_fee", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "claim_fee", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── end_sweep ──
  { instruction: "end_sweep", field: "pool", class: "State", anchor: "FullySelf" },
  // ── record_value ──
  { instruction: "record_value", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "record_value", field: "position", class: "State", anchor: "Partial" },
  { instruction: "record_value", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "record_value", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "record_value", field: "wallet_stats", class: "State", anchor: "External" },
  // ── claim_nft ──
  { instruction: "claim_nft", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "claim_nft", field: "position", class: "State", anchor: "Partial" },
  { instruction: "claim_nft", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "claim_nft", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "claim_nft", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "claim_nft", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "claim_nft", field: "depositor_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "claim_nft", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "claim_nft", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "claim_nft", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "claim_nft", field: "depositor_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── withdraw ──
  { instruction: "withdraw", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "withdraw", field: "position", class: "State", anchor: "Partial" },
  { instruction: "withdraw", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "withdraw", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "withdraw", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "withdraw", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "withdraw", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "withdraw", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "withdraw", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "withdraw", field: "depositor_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "withdraw", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "withdraw", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "withdraw", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "withdraw", field: "depositor_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── claim_nft_core ──
  { instruction: "claim_nft_core", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "claim_nft_core", field: "position", class: "State", anchor: "Partial" },
  { instruction: "claim_nft_core", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "claim_nft_core", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "claim_nft_core", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "claim_nft_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "claim_nft_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── close_seized ──
  { instruction: "close_seized", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "close_seized", field: "position", class: "State", anchor: "Partial" },
  { instruction: "close_seized", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "close_seized", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "close_seized", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "close_seized", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "close_seized", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "close_seized", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── withdraw_core ──
  { instruction: "withdraw_core", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "withdraw_core", field: "position", class: "State", anchor: "Partial" },
  { instruction: "withdraw_core", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "withdraw_core", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "withdraw_core", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "withdraw_core", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "withdraw_core", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "withdraw_core", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "withdraw_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "withdraw_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── update_tier ──
  { instruction: "update_tier", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "update_tier", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "update_tier", field: "position", class: "State", anchor: "Partial" },
  // ── commit_rolls ──
  { instruction: "commit_rolls", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "commit_rolls", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "commit_rolls", field: "batch", class: "State", anchor: "External" },
  { instruction: "commit_rolls", field: "roller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "commit_rolls", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "commit_rolls", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── settle_roll ──
  { instruction: "settle_roll", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "settle_roll", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "settle_roll", field: "batch", class: "State", anchor: "FullySelf" },
  { instruction: "settle_roll", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "settle_roll", field: "roller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "treasury_04", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "protocol_revenue", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "treasury_05", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "position", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "wallet_stats", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "depositor", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "settlement_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "position_vault", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "roller", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "roller_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "roller_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "roller_bye", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "bye_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── settle_roll_core ──
  { instruction: "settle_roll_core", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "settle_roll_core", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "settle_roll_core", field: "batch", class: "State", anchor: "FullySelf" },
  { instruction: "settle_roll_core", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "settle_roll_core", field: "roller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "treasury_04", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "protocol_revenue", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "treasury_05", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "position", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "wallet_stats", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "depositor", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "settlement_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "position_vault", class: "State", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "settle_roll_core", field: "roller", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── vrf_callback ──
  { instruction: "vrf_callback", field: "batch", class: "State", anchor: "FullySelf" },
  { instruction: "vrf_callback", field: "pool", class: "State", anchor: "FullySelf" },
  // ── recover_batch ──
  { instruction: "recover_batch", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "recover_batch", field: "batch", class: "State", anchor: "FullySelf" },
  { instruction: "recover_batch", field: "roller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "recover_batch", field: "principal_vault", class: "State", anchor: "External" },
  // ── close_batch ──
  { instruction: "close_batch", field: "batch", class: "State", anchor: "NotSeeded" },
  { instruction: "close_batch", field: "roller", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── test_resolve_batch ──
  { instruction: "test_resolve_batch", field: "batch", class: "State", anchor: "FullySelf" },
  { instruction: "test_resolve_batch", field: "pool", class: "State", anchor: "FullySelf" },
  // ── activate_acquired ──
  { instruction: "activate_acquired", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "activate_acquired", field: "position", class: "State", anchor: "Partial" },
  { instruction: "activate_acquired", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "activate_acquired", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "activate_acquired", field: "wallet_stats", class: "State", anchor: "External" },
  // ── instant_sell ──
  { instruction: "instant_sell", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "instant_sell", field: "pool_collection", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "instant_sell", field: "position", class: "State", anchor: "External" },
  { instruction: "instant_sell", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "instant_sell", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "instant_sell", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "instant_sell", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "seller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "seller_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "seller_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "loader_escrow", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "loader_escrow_vault", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell", field: "loader_wallet_stats", class: "State", anchor: "NotSeeded" },
  // ── instant_sell_core ──
  { instruction: "instant_sell_core", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "instant_sell_core", field: "pool_collection", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell_core", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "instant_sell_core", field: "position", class: "State", anchor: "External" },
  { instruction: "instant_sell_core", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "instant_sell_core", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "instant_sell_core", field: "weight_index", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell_core", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "instant_sell_core", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell_core", field: "seller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "instant_sell_core", field: "loader_escrow", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell_core", field: "loader_escrow_vault", class: "State", anchor: "NotSeeded" },
  { instruction: "instant_sell_core", field: "loader_wallet_stats", class: "State", anchor: "NotSeeded" },
  // ── t02_claim_fee ──
  { instruction: "t02_claim_fee", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "t02_claim_fee", field: "position", class: "State", anchor: "Partial" },
  { instruction: "t02_claim_fee", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "t02_claim_fee", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "t02_claim_fee", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "t02_claim_fee", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  // ── t02_claim_nft ──
  { instruction: "t02_claim_nft", field: "authority_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_claim_nft", field: "authority_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_claim_nft", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_claim_nft", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_claim_nft", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_claim_nft", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "t02_claim_nft", field: "position", class: "State", anchor: "Partial" },
  { instruction: "t02_claim_nft", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_claim_nft", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "t02_claim_nft", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "t02_claim_nft", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "t02_claim_nft", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  // ── t02_claim_nft_core ──
  { instruction: "t02_claim_nft_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_claim_nft_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_claim_nft_core", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "t02_claim_nft_core", field: "position", class: "State", anchor: "Partial" },
  { instruction: "t02_claim_nft_core", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "t02_claim_nft_core", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "t02_claim_nft_core", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "t02_claim_nft_core", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  // ── t02_deposit ──
  { instruction: "t02_deposit", field: "authority_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_deposit", field: "authority_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_deposit", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_deposit", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_deposit", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_deposit", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "t02_deposit", field: "pool_collection", class: "State", anchor: "NotSeeded" },
  { instruction: "t02_deposit", field: "position", class: "State", anchor: "External" },
  { instruction: "t02_deposit", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_deposit", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "t02_deposit", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "t02_deposit", field: "wallet_stats", class: "State", anchor: "External" },
  // ── t02_deposit_core ──
  { instruction: "t02_deposit_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_deposit_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_deposit_core", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "t02_deposit_core", field: "pool_collection", class: "State", anchor: "NotSeeded" },
  { instruction: "t02_deposit_core", field: "position", class: "State", anchor: "External" },
  { instruction: "t02_deposit_core", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "t02_deposit_core", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "t02_deposit_core", field: "wallet_stats", class: "State", anchor: "External" },
  // ── t02_withdraw ──
  { instruction: "t02_withdraw", field: "authority_token", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_withdraw", field: "authority_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_withdraw", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_withdraw", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_withdraw", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_withdraw", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "t02_withdraw", field: "position", class: "State", anchor: "Partial" },
  { instruction: "t02_withdraw", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_withdraw", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "t02_withdraw", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "t02_withdraw", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "t02_withdraw", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "t02_withdraw", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  { instruction: "t02_withdraw", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "t02_withdraw", field: "weight_index", class: "State", anchor: "NotSeeded" },
  // ── t02_withdraw_core ──
  { instruction: "t02_withdraw_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_withdraw_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded" },
  { instruction: "t02_withdraw_core", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "t02_withdraw_core", field: "position", class: "State", anchor: "Partial" },
  { instruction: "t02_withdraw_core", field: "position_vault", class: "State", anchor: "External" },
  { instruction: "t02_withdraw_core", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "t02_withdraw_core", field: "t02_identity", class: "State", anchor: "External" },
  { instruction: "t02_withdraw_core", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "t02_withdraw_core", field: "treasury_03", class: "State", anchor: "NotSeeded" },
  { instruction: "t02_withdraw_core", field: "wallet_stats", class: "State", anchor: "External" },
  { instruction: "t02_withdraw_core", field: "weight_index", class: "State", anchor: "NotSeeded" },
  // ── open_escrow ──
  { instruction: "open_escrow", field: "escrow", class: "State", anchor: "External" },
  { instruction: "open_escrow", field: "escrow_vault", class: "State", anchor: "External" },
  { instruction: "open_escrow", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── escrow_deposit ──
  { instruction: "escrow_deposit", field: "escrow", class: "State", anchor: "External" },
  { instruction: "escrow_deposit", field: "escrow_vault", class: "State", anchor: "External" },
  { instruction: "escrow_deposit", field: "owner_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── escrow_withdraw ──
  { instruction: "escrow_withdraw", field: "escrow", class: "State", anchor: "External" },
  { instruction: "escrow_withdraw", field: "escrow_vault", class: "State", anchor: "External" },
  { instruction: "escrow_withdraw", field: "owner_usdc", class: "TokenOrMetadata", anchor: "NotSeeded" },
  // ── set_escrow_armed ──
  { instruction: "set_escrow_armed", field: "escrow", class: "State", anchor: "External" },
  // ── claim_fee_to_escrow ──
  { instruction: "claim_fee_to_escrow", field: "pool", class: "State", anchor: "FullySelf" },
  { instruction: "claim_fee_to_escrow", field: "position", class: "State", anchor: "Partial" },
  { instruction: "claim_fee_to_escrow", field: "top_tier", class: "State", anchor: "External" },
  { instruction: "claim_fee_to_escrow", field: "principal_vault", class: "State", anchor: "External" },
  { instruction: "claim_fee_to_escrow", field: "escrow", class: "State", anchor: "External" },
  { instruction: "claim_fee_to_escrow", field: "escrow_vault", class: "State", anchor: "External" },
];

export interface SweepRow extends SweepSlot {
  /** The exact numeric error code the substitution must be rejected with. */
  code: number;
  /** What actually rejects the substitution — never assumed from `anchor` alone, since a
   * `FullySelf` slot's seeds check cannot reject anything and a `Partial` slot's same-pool
   * sibling passes its seeds check too. */
  mechanism: string;
  suite: string;
}

/**
 * Slots with an executed row.
 *
 * All three are `Partial` — the class that needs a *deliberately* same-pool
 * sibling, since a cross-pool one fails at `seeds` for a reason that has nothing to do with the
 * slot under test. None of these three rows' rejections come from the `seeds` check itself
 * (which a same-pool sibling always passes); each is a separate mechanism, named in `mechanism`.
 */
export const EXECUTED_SWEEP_ROWS: readonly SweepRow[] = [
  {
    instruction: "withdraw",
    field: "position",
    class: "State",
    anchor: "Partial",
    code: 6302, // NotPositionOwner
    mechanism: "position.depositor == depositor.key() — an accounts-struct constraint, independent of the seeds check a same-pool sibling passes",
    suite: "legacy-standard-exit",
  },
  {
    instruction: "withdraw_core",
    field: "position",
    class: "State",
    anchor: "Partial",
    code: 6302, // NotPositionOwner
    mechanism: "position.depositor == depositor.key() — same accounts-struct constraint as withdraw's Token Metadata twin",
    suite: "core-standard-exit",
  },
  {
    instruction: "activate_acquired",
    field: "position",
    class: "State",
    anchor: "Partial",
    code: 2006, // Anchor ConstraintSeeds
    mechanism: "a stranger's own Pending position substituted for the treasury's — rejected at wallet_stats' own seeds re-derivation, not at position's",
    suite: "treasury-activation",
  },
  {
    instruction: "update_tier",
    field: "position",
    class: "State",
    anchor: "Partial",
    code: 6301, // InvalidPositionState
    mechanism: "a same-pool sibling already in_tier substituted for an eligible candidate — tier::is_eligible_candidate rejects it, independent of the seeds check both positions pass identically",
    suite: "unexecuted-encoders",
  },
];

/** Slots with no executed row, and why — grouped by the actual reason, not a placeholder. Every
 * entry names a cell `SWEEP_SLOTS` actually declares, so a slot cannot be parked here by
 * misspelling it, and every reason is substantive enough to weigh: which mechanism (if any) has
 * not yet been identified, or what fixture a `Partial` slot's own row is still missing. */
export const DEFERRED_SWEEP_SLOTS: readonly (SweepSlot & { reason: string })[] = [
  // ── consolidate_treasuries ──
  { instruction: "consolidate_treasuries", field: "legacy_treasury_03", class: "State", anchor: "External", reason: "external anchor: the seeds read `pool.key()`, so another pool's Treasury 03 fails the seeds check outright, and `address = pool.treasury_03` must agree with it too. Mechanism identified, execution not yet written" },
  { instruction: "consolidate_treasuries", field: "legacy_treasury_04", class: "State", anchor: "External", reason: "external anchor: the seeds read `pool.key()`, so another pool's Treasury 04 fails the seeds check outright. Mechanism identified, execution not yet written" },
  { instruction: "consolidate_treasuries", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor by design: the Administrator chooses which pool to consolidate, so another pool here is the instruction's input, not a substitution. What a substitution cannot do is detach the per-pool treasuries from it — their seeds read this pool's key" },
  { instruction: "consolidate_treasuries", field: "treasury_03", class: "State", anchor: "Singleton", reason: "singleton anchor — its seeds are one constant, so the shared treasury has no sibling to substitute, and any other token account fails the seeds check. Mechanism identified, execution not yet written" },
  { instruction: "consolidate_treasuries", field: "treasury_04", class: "State", anchor: "Singleton", reason: "singleton anchor — its seeds are one constant, so the shared treasury has no sibling to substitute, and any other token account fails the seeds check. Mechanism identified, execution not yet written" },
  { instruction: "consolidate_treasuries", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── admit_collection ──
  { instruction: "admit_collection", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "admit_collection", field: "pool_collection", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── init_pool ──
  { instruction: "init_pool", field: "pool", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "init_pool", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "init_pool", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "init_pool", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "init_pool", field: "treasury_03", class: "State", anchor: "Singleton", reason: "singleton anchor — its seeds are one constant, so the shared treasury has no sibling to substitute, and any other token account fails the seeds check. Mechanism identified, execution not yet written" },
  { instruction: "init_pool", field: "treasury_04", class: "State", anchor: "Singleton", reason: "singleton anchor — its seeds are one constant, so the shared treasury has no sibling to substitute, and any other token account fails the seeds check. Mechanism identified, execution not yet written" },
  { instruction: "init_pool", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── migrate_pool ──
  { instruction: "migrate_pool", field: "pool", class: "State", anchor: "NotSeeded", reason: "unchecked by design: `Account<Pool>` cannot read a pool at the old length, and the Administrator chooses which pool to grow, so another pool here is the instruction's input, not a substitution. Anything that is not a program-owned `Pool` at the old or current length is refused by the handler's owner, discriminator and length checks, which unit tests cover byte by byte; no sweep row executes it" },
  // ── set_pause ──
  { instruction: "set_pause", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  // ── update_config ──
  { instruction: "update_config", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "update_config", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── sweep_t04_overflow ──
  { instruction: "sweep_t04_overflow", field: "treasury_03", class: "State", anchor: "Singleton", reason: "singleton anchor — its seeds are one constant, so the shared treasury has no sibling to substitute, and any other token account fails the seeds check. Mechanism identified, execution not yet written" },
  { instruction: "sweep_t04_overflow", field: "treasury_04", class: "State", anchor: "Singleton", reason: "singleton anchor — its seeds are one constant, so the shared treasury has no sibling to substitute, and any other token account fails the seeds check. Mechanism identified, execution not yet written" },

  // ── update_bye_mint ──
  { instruction: "update_bye_mint", field: "bye_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── update_protocol_revenue ──
  { instruction: "update_protocol_revenue", field: "protocol_revenue", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — a plain typed account whose only guard is the usdc_mint constraint; its rejection mechanism, if any, has not been identified or executed" },
  // ── withdraw_collection ──
  { instruction: "withdraw_collection", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "withdraw_collection", field: "pool_collection", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── deposit ──
  { instruction: "deposit", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "deposit", field: "pool_collection", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "deposit", field: "position", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "deposit", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "deposit", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "deposit", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "deposit", field: "depositor_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "deposit", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "deposit", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "deposit", field: "depositor_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "deposit", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── deposit_core ──
  { instruction: "deposit_core", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "deposit_core", field: "pool_collection", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "deposit_core", field: "position", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "deposit_core", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "deposit_core", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "deposit_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "deposit_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── approve_deposit ──
  { instruction: "approve_deposit", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "approve_deposit", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling already Active (deposited and approved once) substituted for the intended Pending one — approve_deposit's own require!(position.state == Pending) rejects with PositionNotPending (6400) once built" },
  { instruction: "approve_deposit", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "approve_deposit", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "approve_deposit", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── reject_deposit ──
  { instruction: "reject_deposit", field: "position", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  // ── return_rejected ──
  { instruction: "return_rejected", field: "depositor", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "return_rejected", field: "position", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "return_rejected", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "return_rejected", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "return_rejected", field: "depositor_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "return_rejected", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "return_rejected", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "return_rejected", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "return_rejected", field: "depositor_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── return_rejected_core ──
  { instruction: "return_rejected_core", field: "depositor", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "return_rejected_core", field: "position", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "return_rejected_core", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "return_rejected_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "return_rejected_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── begin_sweep ──
  { instruction: "begin_sweep", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  // ── claim_fee ──
  { instruction: "claim_fee", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "claim_fee", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling belonging to a different depositor substituted for the intended position — claim_fee's own constraint = position.depositor == depositor.key() rejects with NotPositionOwner (6302) once built" },
  { instruction: "claim_fee", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "claim_fee", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "claim_fee", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── end_sweep ──
  { instruction: "end_sweep", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  // ── record_value ──
  { instruction: "record_value", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "record_value", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling still Pending substituted for the intended Active one — apply_record_value's require!(position.state == Active) rejects with InvalidPositionState (6301) once built" },
  { instruction: "record_value", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "record_value", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "record_value", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── claim_nft ──
  { instruction: "claim_nft", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "claim_nft", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling belonging to a different depositor substituted for the intended position — claim_nft's own constraint position.depositor == depositor.key() rejects with NotPositionOwner (6302) once built" },
  { instruction: "claim_nft", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "claim_nft", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "claim_nft", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "claim_nft", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "claim_nft", field: "depositor_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "claim_nft", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "claim_nft", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "claim_nft", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "claim_nft", field: "depositor_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── withdraw ──
  { instruction: "withdraw", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "withdraw", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "withdraw", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "withdraw", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "withdraw", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "withdraw", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw", field: "depositor_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw", field: "depositor_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── claim_nft_core ──
  { instruction: "claim_nft_core", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "claim_nft_core", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling belonging to a different depositor substituted for the intended position — claim_nft_core's own constraint position.depositor == depositor.key() rejects with NotPositionOwner (6302) once built" },
  { instruction: "claim_nft_core", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "claim_nft_core", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "claim_nft_core", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "claim_nft_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "claim_nft_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── close_seized ──
  { instruction: "close_seized", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "close_seized", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling actually in the one PositionState this accounts struct's own constraint excludes — reaching that state requires the full seizure fixture (a permanent freeze delegate triggering a real seizure), not built here" },
  { instruction: "close_seized", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "close_seized", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "close_seized", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "close_seized", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "close_seized", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "close_seized", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── withdraw_core ──
  { instruction: "withdraw_core", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "withdraw_core", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw_core", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "withdraw_core", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "withdraw_core", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "withdraw_core", field: "depositor_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw_core", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "withdraw_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "withdraw_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── update_tier ──
  { instruction: "update_tier", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "update_tier", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── commit_rolls ──
  { instruction: "commit_rolls", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "commit_rolls", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "commit_rolls", field: "batch", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "commit_rolls", field: "roller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "commit_rolls", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "commit_rolls", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── settle_roll ──
  { instruction: "settle_roll", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "settle_roll", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "settle_roll", field: "batch", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "settle_roll", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "settle_roll", field: "roller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "treasury_04", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — it is pinned by `address = pool.treasury_04`, so a substituted token account is refused by the address constraint rather than by seeds. Mechanism identified, execution not yet written" },
  { instruction: "settle_roll", field: "protocol_revenue", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — it is pinned by `address = pool.treasury_03`, so a substituted token account is refused by the address constraint rather than by seeds. Mechanism identified, execution not yet written — the treasury split is what put this slot here, and the row that executes it belongs with that split's own coverage" },
  { instruction: "settle_roll", field: "treasury_05", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — it is pinned by Anchor's associated_token::mint and associated_token::authority constraints against protocol_config.t05_authority, so a substituted account is refused by the ATA derivation check. Mechanism identified, execution not yet written — same treasury split, same owing row" },
  { instruction: "settle_roll", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "position", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "wallet_stats", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "depositor", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "settlement_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "position_vault", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "roller", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "roller_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "roller_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "roller_bye", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "bye_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── settle_roll_core ──
  { instruction: "settle_roll_core", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "settle_roll_core", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "settle_roll_core", field: "batch", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "settle_roll_core", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "settle_roll_core", field: "roller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "treasury_04", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — it is pinned by `address = pool.treasury_04`, so a substituted token account is refused by the address constraint rather than by seeds. Mechanism identified, execution not yet written" },
  { instruction: "settle_roll_core", field: "protocol_revenue", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — it is pinned by `address = pool.treasury_03`, so a substituted token account is refused by the address constraint rather than by seeds. Mechanism identified, execution not yet written — the treasury split is what put this slot here, and the row that executes it belongs with that split's own coverage" },
  { instruction: "settle_roll_core", field: "treasury_05", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — it is pinned by Anchor's associated_token::mint and associated_token::authority constraints against protocol_config.t05_authority, so a substituted account is refused by the ATA derivation check. Mechanism identified, execution not yet written — same treasury split, same owing row" },
  { instruction: "settle_roll_core", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "position", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "wallet_stats", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "depositor", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "settlement_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "position_vault", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "settle_roll_core", field: "roller", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── vrf_callback ──
  { instruction: "vrf_callback", field: "batch", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "vrf_callback", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  // ── recover_batch ──
  { instruction: "recover_batch", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "recover_batch", field: "batch", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "recover_batch", field: "roller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "recover_batch", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── close_batch ──
  { instruction: "close_batch", field: "batch", class: "State", anchor: "NotSeeded", reason: "unchecked by design so a batch of either layout closes: the handler requires program ownership, one of the two lengths, `RollBatch`'s discriminator, a finished state, and that the account sits at the PDA its own pool, batch id and bump derive. Another closable batch here closes that batch, paying the rent to the signer; unit tests cover each check, and no sweep row executes it" },
  { instruction: "close_batch", field: "roller", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "never written, required by the handler to equal the roller the batch records; unit tests cover the refusal of any other account, and no sweep row executes it" },
  // ── test_resolve_batch ──
  { instruction: "test_resolve_batch", field: "batch", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "test_resolve_batch", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  // ── activate_acquired ──
  { instruction: "activate_acquired", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "activate_acquired", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "activate_acquired", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "activate_acquired", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── instant_sell ──
  { instruction: "instant_sell", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "instant_sell", field: "pool_collection", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell", field: "position", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "seller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "seller_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "seller_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell", field: "loader_escrow", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — an Option<UncheckedAccount> the Operator names. The handler binds it: a program-owned `Escrow` at `[ESCROW_SEED, escrow.owner]`, or no data at all (no escrow path); anything else fails EscrowAccountsMismatch (6802). Another loader's escrow is a different loader, not a substitution the program can see" },
  { instruction: "instant_sell", field: "loader_escrow_vault", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — an Option<UncheckedAccount> the handler binds to `[ESCROW_VAULT_SEED, loader_escrow]`; another escrow's vault fails EscrowAccountsMismatch (6802), which the instant-sell escrow suite asserts" },
  { instruction: "instant_sell", field: "loader_wallet_stats", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — an Option<UncheckedAccount> the handler binds to `[WALLET_STATS_SEED, pool, escrow.owner]`; another wallet's rollup fails EscrowAccountsMismatch (6802), which the instant-sell escrow suite asserts" },
  // ── instant_sell_core ──
  { instruction: "instant_sell_core", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "instant_sell_core", field: "pool_collection", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell_core", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell_core", field: "position", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell_core", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell_core", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell_core", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell_core", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "instant_sell_core", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell_core", field: "seller_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "instant_sell_core", field: "loader_escrow", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — an Option<UncheckedAccount> the Operator names. The handler binds it: a program-owned `Escrow` at `[ESCROW_SEED, escrow.owner]`, or no data at all (no escrow path); anything else fails EscrowAccountsMismatch (6802). Another loader's escrow is a different loader, not a substitution the program can see" },
  { instruction: "instant_sell_core", field: "loader_escrow_vault", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — an Option<UncheckedAccount> the handler binds to `[ESCROW_VAULT_SEED, loader_escrow]`; another escrow's vault fails EscrowAccountsMismatch (6802), which the instant-sell escrow suite asserts" },
  { instruction: "instant_sell_core", field: "loader_wallet_stats", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers — an Option<UncheckedAccount> the handler binds to `[WALLET_STATS_SEED, pool, escrow.owner]`; another wallet's rollup fails EscrowAccountsMismatch (6802), which the instant-sell escrow suite asserts" },
  // ── t02_claim_fee ──
  { instruction: "t02_claim_fee", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "t02_claim_fee", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling that is not owned by the Treasury 02 identity substituted for the intended position — the instruction's own constraint position.depositor == t02_identity.key() is what should reject it; the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_fee", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_fee", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_fee", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_fee", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── t02_claim_nft ──
  { instruction: "t02_claim_nft", field: "authority_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_claim_nft", field: "authority_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_claim_nft", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_claim_nft", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_claim_nft", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_claim_nft", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "t02_claim_nft", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling that is not owned by the Treasury 02 identity substituted for the intended position — the instruction's own constraint position.depositor == t02_identity.key() is what should reject it; the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_nft", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_claim_nft", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_nft", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_nft", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_nft", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── t02_claim_nft_core ──
  { instruction: "t02_claim_nft_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_claim_nft_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_claim_nft_core", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "t02_claim_nft_core", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling that is not owned by the Treasury 02 identity substituted for the intended position — the instruction's own constraint position.depositor == t02_identity.key() is what should reject it; the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_nft_core", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_nft_core", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_nft_core", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_claim_nft_core", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── t02_deposit ──
  { instruction: "t02_deposit", field: "authority_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit", field: "authority_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "t02_deposit", field: "pool_collection", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit", field: "position", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_deposit", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_deposit", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_deposit", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── t02_deposit_core ──
  { instruction: "t02_deposit_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit_core", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "t02_deposit_core", field: "pool_collection", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_deposit_core", field: "position", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_deposit_core", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_deposit_core", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_deposit_core", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  // ── t02_withdraw ──
  { instruction: "t02_withdraw", field: "authority_token", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw", field: "authority_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw", field: "master_edition", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw", field: "metadata", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw", field: "nft_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "t02_withdraw", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling that is not owned by the Treasury 02 identity substituted for the intended position — the instruction's own constraint position.depositor == t02_identity.key() is what should reject it; the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw", field: "position_token_record", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── t02_withdraw_core ──
  { instruction: "t02_withdraw_core", field: "asset", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw_core", field: "collection", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw_core", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling here, since every discriminating component is read off the account itself; the actual rejecting mechanism (a cross-account constraint elsewhere, or none at all) has not been identified for this slot" },
  { instruction: "t02_withdraw_core", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling that is not owned by the Treasury 02 identity substituted for the intended position — the instruction's own constraint position.depositor == t02_identity.key() is what should reject it; the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw_core", field: "position_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw_core", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw_core", field: "t02_identity", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw_core", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw_core", field: "treasury_03", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  { instruction: "t02_withdraw_core", field: "wallet_stats", class: "State", anchor: "External", reason: "external anchor — a same-typed sibling from a different pool should fail the seeds check on the recorded external key, but the row asserting that exact code has not been executed" },
  { instruction: "t02_withdraw_core", field: "weight_index", class: "State", anchor: "NotSeeded", reason: "not a PDA the seeds census covers (address =, an Option<Account>, or a plain typed account with no discriminating constraint) — its rejection mechanism, if any, has not been identified or executed" },
  // ── open_escrow ──
  { instruction: "open_escrow", field: "escrow", class: "State", anchor: "External", reason: "external anchor — init with seeds read from owner.key(); any other address fails the seeds check before the account is created. Mechanism identified, execution not yet written" },
  { instruction: "open_escrow", field: "escrow_vault", class: "State", anchor: "External", reason: "external anchor — init with seeds read from escrow.key(); any other address fails the seeds check before the account is created. Mechanism identified, execution not yet written" },
  { instruction: "open_escrow", field: "usdc_mint", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA — pinned by address = protocol_config.usdc_mint, so another mint fails ConstraintAddress. Mechanism identified, execution not yet written" },
  // ── escrow_deposit ──
  { instruction: "escrow_deposit", field: "escrow", class: "State", anchor: "External", reason: "external anchor — the seeds read owner.key(), so another wallet's escrow fails the seeds check, the same 2006 the escrow suite's stranger row asserts for the signer cell. The substitution row itself is not yet written" },
  { instruction: "escrow_deposit", field: "escrow_vault", class: "State", anchor: "External", reason: "external anchor — the seeds read escrow.key(), so another escrow's vault, or any other token account, fails the seeds check. Mechanism identified, execution not yet written" },
  { instruction: "escrow_deposit", field: "owner_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA — bound by token::authority = owner and token::mint = escrow_vault.mint, so another wallet's USDC account fails ConstraintTokenOwner. Mechanism identified, execution not yet written" },
  // ── escrow_withdraw ──
  { instruction: "escrow_withdraw", field: "escrow", class: "State", anchor: "External", reason: "external anchor — the seeds read owner.key(), so another wallet's escrow fails the seeds check, the same 2006 the escrow suite's stranger row asserts for the signer cell. The substitution row itself is not yet written" },
  { instruction: "escrow_withdraw", field: "escrow_vault", class: "State", anchor: "External", reason: "external anchor — the seeds read escrow.key(), so another escrow's vault, or any other token account, fails the seeds check. Mechanism identified, execution not yet written" },
  { instruction: "escrow_withdraw", field: "owner_usdc", class: "TokenOrMetadata", anchor: "NotSeeded", reason: "not a PDA — bound by token::authority = owner and token::mint = escrow_vault.mint, so another wallet's USDC account fails ConstraintTokenOwner. Mechanism identified, execution not yet written" },
  // ── set_escrow_armed ──
  { instruction: "set_escrow_armed", field: "escrow", class: "State", anchor: "External", reason: "external anchor — the seeds read owner.key(), so another wallet's escrow fails the seeds check, the same 2006 the escrow suite's stranger row asserts for the signer cell. The substitution row itself is not yet written" },
  // ── claim_fee_to_escrow ──
  { instruction: "claim_fee_to_escrow", field: "pool", class: "State", anchor: "FullySelf", reason: "fully-self anchor — the seeds check cannot reject a substituted sibling pool here, as for claim_fee; what does reject it is the position's own seeds, which read pool.key(), so a sibling pool fails at the position slot. Mechanism identified, execution not yet written" },
  { instruction: "claim_fee_to_escrow", field: "position", class: "State", anchor: "Partial", reason: "needs a same-pool sibling belonging to a different depositor — the constraint position.depositor == depositor.key() rejects it with NotPositionOwner (6302), the code the escrow suite's stranger row asserts for the signer cell; the substitution row itself is not yet written" },
  { instruction: "claim_fee_to_escrow", field: "top_tier", class: "State", anchor: "External", reason: "external anchor — the seeds read pool.key(), so another pool's TopTier fails the seeds check outright. Mechanism identified, execution not yet written" },
  { instruction: "claim_fee_to_escrow", field: "principal_vault", class: "State", anchor: "External", reason: "external anchor — the seeds read pool.key(), so another pool's principal vault fails the seeds check outright. Mechanism identified, execution not yet written" },
  { instruction: "claim_fee_to_escrow", field: "escrow", class: "State", anchor: "External", reason: "external anchor — the seeds read depositor.key(), so another wallet's escrow fails the seeds check, which is what keeps a fee from landing in someone else's escrow. Mechanism identified, execution not yet written" },
  { instruction: "claim_fee_to_escrow", field: "escrow_vault", class: "State", anchor: "External", reason: "external anchor — the seeds read escrow.key(), so another escrow's vault, or any other token account, fails the seeds check. Mechanism identified, execution not yet written" },
];

export function sweepSlotKey(slot: SweepSlot): string {
  return `${slot.instruction}.${slot.field}`;
}

pub mod approve_deposit;
#[allow(clippy::module_inception)]
pub mod deposit;
pub mod deposit_core;
pub mod reject_deposit;
pub mod return_rejected;
pub mod return_rejected_core;

// Only the `Accounts` context types are re-exported; handlers are called by full module path
// in lib.rs, since every one of them is named `handler`.
pub use approve_deposit::ApproveDeposit;
pub use deposit::Deposit;
pub use deposit_core::DepositCore;
pub use reject_deposit::RejectDeposit;
pub use return_rejected::ReturnRejected;
pub use return_rejected_core::ReturnRejectedCore;

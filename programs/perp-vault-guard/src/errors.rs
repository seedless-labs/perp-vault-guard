use anchor_lang::prelude::*;

#[error_code]
pub enum VaultError {
    #[msg("Signer is not the vault manager")]
    NotManager,
    #[msg("Vault is killed: only reduce-only orders are allowed")]
    VaultKilled,
    #[msg("Market is not in the vault's allowed list")]
    MarketNotAllowed,
    #[msg("Order notional is above the per-order cap")]
    OrderTooLarge,
    #[msg("Order would take gross exposure above max leverage")]
    LeverageExceeded,
    #[msg("Limit price is too far from the market oracle")]
    PriceOutOfBand,
    #[msg("Market account is not the perp program's market PDA")]
    BadMarketAccount,
    #[msg("Perp program does not match the vault's")]
    BadPerpProgram,
    #[msg("Perp user account does not belong to this vault")]
    BadPerpUser,
    #[msg("Amount must be above zero")]
    ZeroAmount,
    #[msg("Invalid policy")]
    InvalidPolicy,
    #[msg("Math overflow")]
    MathOverflow,
}

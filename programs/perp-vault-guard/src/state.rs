use anchor_lang::prelude::*;

pub const MAX_MARKETS: usize = 8;
pub const BPS: u128 = 10_000;
/// Drift precisions: base 1e9, price 1e6, quote (USDC) 1e6.
pub const BASE_PRECISION: u128 = 1_000_000_000;

/// The rules the manager trades under. Only the owner can change them.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace, Debug)]
pub struct Policy {
    /// Perp market indexes the manager may open positions in.
    pub allowed_markets: [u16; MAX_MARKETS],
    pub market_count: u8,
    /// Max gross notional / deposited collateral, in bps (30_000 = 3x).
    pub max_leverage_bps: u32,
    /// Max notional of a single order, in USDC (1e6).
    pub max_order_notional: u64,
    /// Max distance between the manager's limit price and the market oracle, in bps.
    pub max_oracle_deviation_bps: u16,
}

impl Policy {
    pub fn allows(&self, market_index: u16) -> bool {
        self.allowed_markets[..self.market_count as usize].contains(&market_index)
    }

    pub fn validate(&self) -> bool {
        (self.market_count as usize) <= MAX_MARKETS
            && self.market_count > 0
            && self.max_leverage_bps > 0
            && self.max_leverage_bps <= 200_000 // 20x hard ceiling
            && self.max_order_notional > 0
            && self.max_oracle_deviation_bps > 0
            && self.max_oracle_deviation_bps <= 1_000 // 10% hard ceiling
    }
}

#[account]
#[derive(InitSpace)]
pub struct Vault {
    /// Depositor. Sets policy, kills, replaces or revokes the manager.
    pub owner: Pubkey,
    /// Trader. Can only open/close perps through this program, within policy.
    pub manager: Pubkey,
    /// The perp program the vault's PDA account lives on (Drift v2 on mainnet).
    /// Fixed at init, so the manager can never point CPIs somewhere else.
    pub perp_program: Pubkey,
    pub usdc_mint: Pubkey,
    pub bump: u8,
    /// Kill switch: once set, only reduce-only orders go through.
    pub killed: bool,
    /// Net USDC deposited (1e6). Collateral base for the leverage check.
    pub deposited: u64,
    /// Gross notional opened and not yet closed (1e6), tracked on order intent.
    pub exposure: u64,
    pub policy: Policy,
}

#[repr(u8)]
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Long = 0,
    Short = 1,
}

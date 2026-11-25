//! MOCK PERP PROGRAM, DEVNET TEST DOUBLE ONLY. NOT A REAL EXCHANGE.
//!
//! Drift v2's devnet deployment cannot take deposits or orders right now: its market
//! accounts are stale-sized (SpotMarket 776 bytes on chain vs 808 in the current IDL,
//! PerpMarket 1216 vs 1240), so every deposit fails with SpotMarketNotFound (6087) and
//! every order with PerpMarketNotFound (6078). See scripts/raw-drift-check.ts.
//!
//! This program speaks the same interface as Drift for the four instructions the vault
//! uses (same names, so same Anchor discriminators; same account order; same arg layout;
//! same PDA seeds; oracle price at the same byte offset in the market account). The
//! vault program is byte-for-byte the same build either way: it stores which perp
//! program it talks to at init. Mainnet = Drift, devnet demo = this.
//!
//! Fills are instant at the stored oracle price. No AMM, no funding, no liquidation.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer};

declare_id!("V8Dip6eZLe2UYU6LSYitwWXrqHG2k2y9Muhd92RbQYW");

pub const MAX_POSITIONS: usize = 8;

#[program]
pub mod perp_mock {
    use super::*;

    // ---------- mock-only admin ----------

    pub fn initialize(ctx: Context<Initialize>) -> Result<()> {
        ctx.accounts.state.admin = ctx.accounts.admin.key();
        ctx.accounts.state.usdc_mint = ctx.accounts.usdc_mint.key();
        Ok(())
    }

    pub fn init_market(ctx: Context<InitMarket>, market_index: u16, price: i64) -> Result<()> {
        require!(price > 0, MockError::BadPrice);
        let m = &mut ctx.accounts.market;
        m.oracle = Pubkey::default();
        m.last_oracle_price = price;
        m.market_index = market_index;
        Ok(())
    }

    pub fn set_price(ctx: Context<SetPrice>, price: i64) -> Result<()> {
        require!(price > 0, MockError::BadPrice);
        ctx.accounts.market.last_oracle_price = price;
        Ok(())
    }

    // ---------- Drift-compatible surface ----------

    pub fn initialize_user_stats(ctx: Context<InitializeUserStats>) -> Result<()> {
        ctx.accounts.user_stats.authority = ctx.accounts.authority.key();
        Ok(())
    }

    pub fn initialize_user(ctx: Context<InitializeUser>, sub_account_id: u16, _name: [u8; 32]) -> Result<()> {
        require!(sub_account_id == 0, MockError::SubAccount);
        let u = &mut ctx.accounts.user;
        u.authority = ctx.accounts.authority.key();
        u.collateral = 0;
        u.positions = [0; MAX_POSITIONS];
        Ok(())
    }

    pub fn deposit(ctx: Context<DepositCtx>, market_index: u16, amount: u64, _reduce_only: bool) -> Result<()> {
        require!(market_index == 0, MockError::SpotMarket);
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.user_token_account.to_account_info(),
                    to: ctx.accounts.spot_market_vault.to_account_info(),
                    authority: ctx.accounts.authority.to_account_info(),
                },
            ),
            amount,
        )?;
        let u = &mut ctx.accounts.user;
        u.collateral = u.collateral.checked_add(amount).ok_or(MockError::Overflow)?;
        Ok(())
    }

    pub fn place_perp_order<'info>(ctx: Context<'_, '_, 'info, 'info, PlacePerpOrder<'info>>, params: OrderParams) -> Result<()> {
        require!(params.market_type == 1, MockError::NotPerp);
        require!((params.market_index as usize) < MAX_POSITIONS, MockError::NoMarket);
        let (market_key, _) = Pubkey::find_program_address(&[b"perp_market", &params.market_index.to_le_bytes()], &crate::ID);
        let info = ctx
            .remaining_accounts
            .iter()
            .find(|a| a.key() == market_key && *a.owner == crate::ID)
            .ok_or(MockError::NoMarket)?;
        let market: Account<MockPerpMarket> = Account::try_from(info)?;
        let oracle = market.last_oracle_price as u64;

        // price is the worst acceptable price (Drift market order semantics).
        let long = params.direction == 0;
        if params.price > 0 {
            require!(if long { params.price >= oracle } else { params.price <= oracle }, MockError::PriceBreached);
        }

        let i = params.market_index as usize;
        let u = &mut ctx.accounts.user;
        let cur = u.positions[i];
        let delta = i64::try_from(params.base_asset_amount).map_err(|_| MockError::Overflow)?;
        let next = if long { cur.checked_add(delta) } else { cur.checked_sub(delta) }.ok_or(MockError::Overflow)?;
        if params.reduce_only {
            require!(cur != 0 && next.unsigned_abs() < cur.unsigned_abs() && (next == 0 || next.signum() == cur.signum()), MockError::ReduceOnly);
        }
        u.positions[i] = next;
        msg!("mock fill: market {} {} {} @ {} -> pos {}", i, if long { "long" } else { "short" }, params.base_asset_amount, oracle, next);
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct OrderParams {
    pub order_type: u8,
    pub market_type: u8,
    pub direction: u8,
    pub user_order_id: u8,
    pub base_asset_amount: u64,
    pub price: u64,
    pub market_index: u16,
    pub reduce_only: bool,
    pub post_only: u8,
    pub bit_flags: u8,
    pub max_ts: Option<i64>,
    pub trigger_price: Option<u64>,
    pub trigger_condition: u8,
    pub oracle_price_offset: Option<i32>,
    pub auction_duration: Option<u8>,
    pub auction_start_price: Option<i64>,
    pub auction_end_price: Option<i64>,
}

#[account]
pub struct MockState {
    pub admin: Pubkey,
    pub usdc_mint: Pubkey,
}

/// Laid out so `last_oracle_price` sits at byte 72, like Drift's PerpMarket
/// (8 disc + 32 + 32 oracle).
#[account]
pub struct MockPerpMarket {
    pub _pad: [u8; 32],
    pub oracle: Pubkey,
    pub last_oracle_price: i64,
    pub market_index: u16,
}

#[account]
pub struct MockUserStats {
    pub authority: Pubkey,
}

#[account]
pub struct MockUser {
    pub authority: Pubkey,
    pub collateral: u64,
    pub positions: [i64; MAX_POSITIONS],
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(init, payer = admin, space = 8 + 64, seeds = [b"drift_state"], bump)]
    pub state: Account<'info, MockState>,
    pub usdc_mint: Account<'info, Mint>,
    #[account(init, payer = admin, seeds = [b"spot_market_vault", 0u16.to_le_bytes().as_ref()], bump, token::mint = usdc_mint, token::authority = state)]
    pub spot_market_vault: Account<'info, TokenAccount>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub rent: Sysvar<'info, Rent>,
}

#[derive(Accounts)]
#[instruction(market_index: u16)]
pub struct InitMarket<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(has_one = admin)]
    pub state: Account<'info, MockState>,
    #[account(init, payer = admin, space = 8 + 32 + 32 + 8 + 2, seeds = [b"perp_market", market_index.to_le_bytes().as_ref()], bump)]
    pub market: Account<'info, MockPerpMarket>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SetPrice<'info> {
    pub admin: Signer<'info>,
    #[account(has_one = admin)]
    pub state: Account<'info, MockState>,
    #[account(mut)]
    pub market: Account<'info, MockPerpMarket>,
}

#[derive(Accounts)]
pub struct InitializeUserStats<'info> {
    #[account(init, payer = payer, space = 8 + 32, seeds = [b"user_stats", authority.key().as_ref()], bump)]
    pub user_stats: Account<'info, MockUserStats>,
    #[account(mut)]
    pub state: Account<'info, MockState>,
    /// CHECK: any authority, as in Drift.
    pub authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub rent: Sysvar<'info, Rent>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(sub_account_id: u16)]
pub struct InitializeUser<'info> {
    #[account(init, payer = payer, space = 8 + 32 + 8 + 8 * MAX_POSITIONS, seeds = [b"user", authority.key().as_ref(), sub_account_id.to_le_bytes().as_ref()], bump)]
    pub user: Account<'info, MockUser>,
    #[account(mut, has_one = authority)]
    pub user_stats: Account<'info, MockUserStats>,
    #[account(mut)]
    pub state: Account<'info, MockState>,
    /// CHECK: any authority, as in Drift.
    pub authority: UncheckedAccount<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub rent: Sysvar<'info, Rent>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DepositCtx<'info> {
    pub state: Account<'info, MockState>,
    #[account(mut, has_one = authority)]
    pub user: Account<'info, MockUser>,
    #[account(mut, has_one = authority)]
    pub user_stats: Account<'info, MockUserStats>,
    pub authority: Signer<'info>,
    #[account(mut, seeds = [b"spot_market_vault", 0u16.to_le_bytes().as_ref()], bump)]
    pub spot_market_vault: Account<'info, TokenAccount>,
    #[account(mut)]
    pub user_token_account: Account<'info, TokenAccount>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct PlacePerpOrder<'info> {
    pub state: Account<'info, MockState>,
    #[account(mut, has_one = authority)]
    pub user: Account<'info, MockUser>,
    pub authority: Signer<'info>,
}

#[error_code]
pub enum MockError {
    #[msg("price must be > 0")]
    BadPrice,
    #[msg("only sub account 0")]
    SubAccount,
    #[msg("only spot market 0 (USDC)")]
    SpotMarket,
    #[msg("perp orders only")]
    NotPerp,
    #[msg("perp market account missing from remaining accounts")]
    NoMarket,
    #[msg("oracle price is worse than the order's limit")]
    PriceBreached,
    #[msg("reduce-only order would grow or flip the position")]
    ReduceOnly,
    #[msg("overflow")]
    Overflow,
}

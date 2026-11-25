//! perp-vault-guard: a USDC vault whose manager can trade perps through CPI,
//! but only inside limits the owner sets on chain.
//!
//! The manager never holds funds and never signs for the perp account. The vault PDA
//! is the perp account authority, and the only way to make it sign is through
//! `open_perp` / `close_perp`, which check the policy first.

use anchor_lang::prelude::*;
use anchor_spl::{
    associated_token::AssociatedToken,
    token::{self, Mint, Token, TokenAccount, Transfer},
};

pub mod drift;
pub mod errors;
pub mod state;

use errors::VaultError;
use state::*;

declare_id!("Fcfy2AqyC4Yrus9NxmShBZcgxzYvzdz3rWjz17rZy5x");

#[program]
pub mod perp_vault_guard {
    use super::*;

    /// Owner creates the vault, its USDC account, and the vault's account on the perp program.
    pub fn init_vault(ctx: Context<InitVault>, manager: Pubkey, policy: Policy) -> Result<()> {
        require!(policy.validate(), VaultError::InvalidPolicy);
        let v = &mut ctx.accounts.vault;
        v.owner = ctx.accounts.owner.key();
        v.manager = manager;
        v.perp_program = ctx.accounts.perp_program.key();
        v.usdc_mint = ctx.accounts.usdc_mint.key();
        v.bump = ctx.bumps.vault;
        v.killed = false;
        v.deposited = 0;
        v.exposure = 0;
        v.policy = policy;

        let a = &ctx.accounts;
        let pp = a.perp_program.key();
        require_keys_eq!(a.perp_user.key(), drift::user_pda(&pp, &a.vault.key()), VaultError::BadPerpUser);
        require_keys_eq!(a.perp_user_stats.key(), drift::user_stats_pda(&pp, &a.vault.key()), VaultError::BadPerpUser);

        let vault_info = a.vault.to_account_info();
        let owner = a.owner.to_account_info();
        let rent = a.rent.to_account_info();
        let system = a.system_program.to_account_info();
        drift::initialize_user_stats(
            &a.perp_program,
            [&a.perp_user_stats, &a.perp_state, &vault_info, &owner, &rent, &system],
        )?;
        let mut name = [b' '; 32];
        name[..16].copy_from_slice(b"perp-vault-guard");
        drift::initialize_user(
            &a.perp_program,
            [&a.perp_user, &a.perp_user_stats, &a.perp_state, &vault_info, &owner, &rent, &system],
            name,
        )?;
        emit!(VaultCreated { vault: a.vault.key(), owner: a.owner.key(), manager, perp_program: pp });
        Ok(())
    }

    /// Owner moves USDC into the vault, and the vault deposits it as perp collateral.
    pub fn deposit<'info>(ctx: Context<'_, '_, 'info, 'info, Deposit<'info>>, amount: u64) -> Result<()> {
        require!(amount > 0, VaultError::ZeroAmount);
        let a = &ctx.accounts;
        token::transfer(
            CpiContext::new(
                a.token_program.to_account_info(),
                Transfer { from: a.owner_usdc.to_account_info(), to: a.vault_usdc.to_account_info(), authority: a.owner.to_account_info() },
            ),
            amount,
        )?;
        let owner_key = a.vault.owner;
        let seeds: &[&[u8]] = &[b"vault", owner_key.as_ref(), &[a.vault.bump]];
        let vault_info = a.vault.to_account_info();
        let vault_usdc = a.vault_usdc.to_account_info();
        let token_program = a.token_program.to_account_info();
        drift::deposit(
            &a.perp_program,
            [&a.perp_state, &a.perp_user, &a.perp_user_stats, &vault_info, &a.spot_market_vault, &vault_usdc, &token_program],
            ctx.remaining_accounts,
            amount,
            &[seeds],
        )?;
        let v = &mut ctx.accounts.vault;
        v.deposited = v.deposited.checked_add(amount).ok_or(VaultError::MathOverflow)?;
        Ok(())
    }

    /// Manager opens or adds to a position. Every limit is checked here, before the vault signs.
    pub fn open_perp<'info>(
        ctx: Context<'_, '_, 'info, 'info, Trade<'info>>,
        market_index: u16,
        side: Side,
        base_asset_amount: u64,
        limit_price: u64,
    ) -> Result<()> {
        let v = &ctx.accounts.vault;
        require!(!v.killed, VaultError::VaultKilled);
        require!(v.policy.allows(market_index), VaultError::MarketNotAllowed);
        require!(base_asset_amount > 0 && limit_price > 0, VaultError::ZeroAmount);

        let oracle = drift::read_oracle_price(&ctx.accounts.perp_market, &v.perp_program, market_index)?;
        check_band(limit_price, oracle, v.policy.max_oracle_deviation_bps)?;

        let notional = notional(base_asset_amount, limit_price.max(oracle))?;
        require!(notional <= v.policy.max_order_notional, VaultError::OrderTooLarge);
        let new_exposure = v.exposure.checked_add(notional).ok_or(VaultError::MathOverflow)?;
        let cap = (v.deposited as u128) * (v.policy.max_leverage_bps as u128) / BPS;
        require!((new_exposure as u128) <= cap, VaultError::LeverageExceeded);

        cpi_order(&ctx, side, base_asset_amount, limit_price, market_index, false)?;
        ctx.accounts.vault.exposure = new_exposure;
        emit!(PerpOrder { vault: ctx.accounts.vault.key(), market_index, side, base_asset_amount, limit_price, oracle, notional, reduce_only: false });
        Ok(())
    }

    /// Manager reduces a position. Reduce-only, so it can never grow risk.
    /// Still allowed after a kill, so a killed vault can always be unwound.
    pub fn close_perp<'info>(
        ctx: Context<'_, '_, 'info, 'info, Trade<'info>>,
        market_index: u16,
        side: Side,
        base_asset_amount: u64,
        limit_price: u64,
    ) -> Result<()> {
        let v = &ctx.accounts.vault;
        require!(base_asset_amount > 0 && limit_price > 0, VaultError::ZeroAmount);
        let oracle = drift::read_oracle_price(&ctx.accounts.perp_market, &v.perp_program, market_index)?;
        check_band(limit_price, oracle, v.policy.max_oracle_deviation_bps)?;
        let notional = notional(base_asset_amount, oracle)?;

        cpi_order(&ctx, side, base_asset_amount, limit_price, market_index, true)?;
        let v = &mut ctx.accounts.vault;
        v.exposure = v.exposure.saturating_sub(notional);
        emit!(PerpOrder { vault: v.key(), market_index, side, base_asset_amount, limit_price, oracle, notional, reduce_only: true });
        Ok(())
    }

    /// Owner: stop all new risk. Only `close_perp` works after this.
    pub fn kill(ctx: Context<OwnerOnly>) -> Result<()> {
        ctx.accounts.vault.killed = true;
        emit!(VaultKilledEvent { vault: ctx.accounts.vault.key() });
        Ok(())
    }

    /// Owner: lift the kill switch.
    pub fn unkill(ctx: Context<OwnerOnly>) -> Result<()> {
        ctx.accounts.vault.killed = false;
        Ok(())
    }

    /// Owner: replace the manager.
    pub fn set_manager(ctx: Context<OwnerOnly>, manager: Pubkey) -> Result<()> {
        ctx.accounts.vault.manager = manager;
        Ok(())
    }

    /// Owner: remove the manager entirely. Nobody can trade until a new one is set.
    pub fn revoke_manager(ctx: Context<OwnerOnly>) -> Result<()> {
        ctx.accounts.vault.manager = Pubkey::default();
        Ok(())
    }

    /// Owner: change the limits.
    pub fn set_policy(ctx: Context<OwnerOnly>, policy: Policy) -> Result<()> {
        require!(policy.validate(), VaultError::InvalidPolicy);
        ctx.accounts.vault.policy = policy;
        Ok(())
    }
}

/// |limit - oracle| / oracle <= max_bps
fn check_band(limit: u64, oracle: u64, max_bps: u16) -> Result<()> {
    let diff = (limit as i128 - oracle as i128).unsigned_abs();
    require!(diff * BPS <= (oracle as u128) * (max_bps as u128), VaultError::PriceOutOfBand);
    Ok(())
}

/// base (1e9) * price (1e6) / 1e9 = quote (1e6)
fn notional(base: u64, price: u64) -> Result<u64> {
    let n = (base as u128) * (price as u128) / BASE_PRECISION;
    u64::try_from(n).map_err(|_| error!(VaultError::MathOverflow))
}

fn cpi_order<'info>(
    ctx: &Context<'_, '_, 'info, 'info, Trade<'info>>,
    side: Side,
    base: u64,
    price: u64,
    market_index: u16,
    reduce_only: bool,
) -> Result<()> {
    let a = &ctx.accounts;
    let owner_key = a.vault.owner;
    let seeds: &[&[u8]] = &[b"vault", owner_key.as_ref(), &[a.vault.bump]];
    let vault_info = a.vault.to_account_info();
    drift::place_perp_order(
        &a.perp_program,
        [&a.perp_state, &a.perp_user, &vault_info],
        ctx.remaining_accounts,
        drift::order_params(side, base, price, market_index, reduce_only),
        &[seeds],
    )
}

#[derive(Accounts)]
pub struct InitVault<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(init, payer = owner, space = 8 + Vault::INIT_SPACE, seeds = [b"vault", owner.key().as_ref()], bump)]
    pub vault: Account<'info, Vault>,
    pub usdc_mint: Account<'info, Mint>,
    #[account(init, payer = owner, associated_token::mint = usdc_mint, associated_token::authority = vault)]
    pub vault_usdc: Account<'info, TokenAccount>,
    /// CHECK: the perp program (Drift v2 on mainnet). Stored on the vault and fixed from here on.
    #[account(executable)]
    pub perp_program: UncheckedAccount<'info>,
    /// CHECK: perp program state, validated by the perp program.
    #[account(mut)]
    pub perp_state: UncheckedAccount<'info>,
    /// CHECK: vault's perp user PDA, checked against the derivation.
    #[account(mut)]
    pub perp_user: UncheckedAccount<'info>,
    /// CHECK: vault's perp user stats PDA, checked against the derivation.
    #[account(mut)]
    pub perp_user_stats: UncheckedAccount<'info>,
    pub rent: Sysvar<'info, Rent>,
    pub system_program: Program<'info, System>,
    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
}

#[derive(Accounts)]
pub struct Deposit<'info> {
    pub owner: Signer<'info>,
    #[account(mut, has_one = owner, has_one = perp_program, has_one = usdc_mint, seeds = [b"vault", owner.key().as_ref()], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
    pub usdc_mint: Account<'info, Mint>,
    #[account(mut, token::mint = usdc_mint, token::authority = owner)]
    pub owner_usdc: Account<'info, TokenAccount>,
    #[account(mut, associated_token::mint = usdc_mint, associated_token::authority = vault)]
    pub vault_usdc: Account<'info, TokenAccount>,
    /// CHECK: matched to vault.perp_program by has_one.
    pub perp_program: UncheckedAccount<'info>,
    /// CHECK: validated by the perp program.
    pub perp_state: UncheckedAccount<'info>,
    /// CHECK: must be the vault's perp user PDA.
    #[account(mut, address = drift::user_pda(&vault.perp_program, &vault.key()) @ VaultError::BadPerpUser)]
    pub perp_user: UncheckedAccount<'info>,
    /// CHECK: must be the vault's perp user stats PDA.
    #[account(mut, address = drift::user_stats_pda(&vault.perp_program, &vault.key()) @ VaultError::BadPerpUser)]
    pub perp_user_stats: UncheckedAccount<'info>,
    /// CHECK: perp program's USDC spot market vault, validated by the perp program.
    #[account(mut)]
    pub spot_market_vault: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct Trade<'info> {
    pub manager: Signer<'info>,
    #[account(
        mut,
        has_one = perp_program @ VaultError::BadPerpProgram,
        constraint = vault.manager == manager.key() && vault.manager != Pubkey::default() @ VaultError::NotManager,
        seeds = [b"vault", vault.owner.as_ref()],
        bump = vault.bump,
    )]
    pub vault: Account<'info, Vault>,
    /// CHECK: matched to vault.perp_program by has_one.
    pub perp_program: UncheckedAccount<'info>,
    /// CHECK: validated by the perp program.
    pub perp_state: UncheckedAccount<'info>,
    /// CHECK: must be the vault's perp user PDA.
    #[account(mut, address = drift::user_pda(&vault.perp_program, &vault.key()) @ VaultError::BadPerpUser)]
    pub perp_user: UncheckedAccount<'info>,
    /// CHECK: owner and PDA checked in read_oracle_price. Also passed in remaining accounts for the CPI.
    pub perp_market: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct OwnerOnly<'info> {
    pub owner: Signer<'info>,
    #[account(mut, has_one = owner, seeds = [b"vault", owner.key().as_ref()], bump = vault.bump)]
    pub vault: Account<'info, Vault>,
}

#[event]
pub struct VaultCreated {
    pub vault: Pubkey,
    pub owner: Pubkey,
    pub manager: Pubkey,
    pub perp_program: Pubkey,
}

#[event]
pub struct PerpOrder {
    pub vault: Pubkey,
    pub market_index: u16,
    pub side: Side,
    pub base_asset_amount: u64,
    pub limit_price: u64,
    pub oracle: u64,
    pub notional: u64,
    pub reduce_only: bool,
}

#[event]
pub struct VaultKilledEvent {
    pub vault: Pubkey,
}

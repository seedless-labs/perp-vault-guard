//! Hand-written Drift v2 CPI. No `drift` crate dependency (it pins an old anchor),
//! just the instruction discriminators, account order and borsh layout of OrderParams,
//! taken from Drift's published IDL (sdk/src/idl/drift.json, v2.162.0).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    hash::hash,
    instruction::{AccountMeta, Instruction},
    program::{invoke, invoke_signed},
};

use crate::errors::VaultError;
use crate::state::Side;

/// sha256("global:<name>")[..8], Anchor's instruction discriminator.
pub fn disc(name: &str) -> [u8; 8] {
    let mut d = [0u8; 8];
    d.copy_from_slice(&hash(format!("global:{name}").as_bytes()).to_bytes()[..8]);
    d
}

pub fn user_pda(perp_program: &Pubkey, authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"user", authority.as_ref(), &0u16.to_le_bytes()], perp_program).0
}

pub fn user_stats_pda(perp_program: &Pubkey, authority: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[b"user_stats", authority.as_ref()], perp_program).0
}

pub fn perp_market_pda(perp_program: &Pubkey, market_index: u16) -> Pubkey {
    Pubkey::find_program_address(&[b"perp_market", &market_index.to_le_bytes()], perp_program).0
}

/// Byte offset of `PerpMarket.amm.historical_oracle_data.last_oracle_price` (i64, 1e6).
/// 8 (discriminator) + 32 (pubkey) + 32 (amm.oracle). Verified against live devnet
/// accounts: market 0 reads ~80 (SOL), 1 ~69000 (BTC), 2 ~2113 (ETH).
pub const ORACLE_PRICE_OFFSET: usize = 72;

/// Reads the oracle price the perp program itself stores on the market account.
/// The account must be the real market PDA owned by the vault's perp program,
/// so the manager can not hand in a fake price.
pub fn read_oracle_price(market: &AccountInfo, perp_program: &Pubkey, market_index: u16) -> Result<u64> {
    require_keys_eq!(*market.owner, *perp_program, VaultError::BadMarketAccount);
    require_keys_eq!(market.key(), perp_market_pda(perp_program, market_index), VaultError::BadMarketAccount);
    let data = market.try_borrow_data()?;
    require!(data.len() >= ORACLE_PRICE_OFFSET + 8, VaultError::BadMarketAccount);
    let price = i64::from_le_bytes(data[ORACLE_PRICE_OFFSET..ORACLE_PRICE_OFFSET + 8].try_into().unwrap());
    require!(price > 0, VaultError::BadMarketAccount);
    Ok(price as u64)
}

fn meta(info: &AccountInfo, writable: bool, signer: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(info.key(), signer)
    } else {
        AccountMeta::new_readonly(info.key(), signer)
    }
}

/// initialize_user_stats: [user_stats w, state w, authority, payer ws, rent, system]
pub fn initialize_user_stats<'info>(program: &AccountInfo<'info>, accts: [&AccountInfo<'info>; 6]) -> Result<()> {
    let [user_stats, state, authority, payer, rent, system] = accts;
    let ix = Instruction {
        program_id: program.key(),
        accounts: vec![
            meta(user_stats, true, false),
            meta(state, true, false),
            meta(authority, false, false),
            meta(payer, true, true),
            meta(rent, false, false),
            meta(system, false, false),
        ],
        data: disc("initialize_user_stats").to_vec(),
    };
    invoke(&ix, &[user_stats.clone(), state.clone(), authority.clone(), payer.clone(), rent.clone(), system.clone()])?;
    Ok(())
}

/// initialize_user(sub_account_id: u16, name: [u8; 32]):
/// [user w, user_stats w, state w, authority, payer ws, rent, system]
pub fn initialize_user<'info>(program: &AccountInfo<'info>, accts: [&AccountInfo<'info>; 7], name: [u8; 32]) -> Result<()> {
    let [user, user_stats, state, authority, payer, rent, system] = accts;
    let mut data = disc("initialize_user").to_vec();
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&name);
    let ix = Instruction {
        program_id: program.key(),
        accounts: vec![
            meta(user, true, false),
            meta(user_stats, true, false),
            meta(state, true, false),
            meta(authority, false, false),
            meta(payer, true, true),
            meta(rent, false, false),
            meta(system, false, false),
        ],
        data,
    };
    invoke(&ix, &[user.clone(), user_stats.clone(), state.clone(), authority.clone(), payer.clone(), rent.clone(), system.clone()])?;
    Ok(())
}

/// deposit(market_index: u16, amount: u64, reduce_only: bool):
/// [state, user w, user_stats w, authority s, spot_market_vault w, user_token_account w, token_program] + remaining
pub fn deposit<'info>(
    program: &AccountInfo<'info>,
    accts: [&AccountInfo<'info>; 7],
    remaining: &[AccountInfo<'info>],
    amount: u64,
    signer: &[&[&[u8]]],
) -> Result<()> {
    let [state, user, user_stats, authority, spot_vault, token_account, token_program] = accts;
    let mut data = disc("deposit").to_vec();
    data.extend_from_slice(&0u16.to_le_bytes()); // spot market 0 = USDC
    data.extend_from_slice(&amount.to_le_bytes());
    data.push(0); // reduce_only = false
    let mut accounts = vec![
        meta(state, false, false),
        meta(user, true, false),
        meta(user_stats, true, false),
        meta(authority, false, true),
        meta(spot_vault, true, false),
        meta(token_account, true, false),
        meta(token_program, false, false),
    ];
    let mut infos = vec![state.clone(), user.clone(), user_stats.clone(), authority.clone(), spot_vault.clone(), token_account.clone(), token_program.clone()];
    for r in remaining {
        accounts.push(meta(r, r.is_writable, false));
        infos.push(r.clone());
    }
    invoke_signed(&Instruction { program_id: program.key(), accounts, data }, &infos, signer)?;
    Ok(())
}

/// Borsh encoding of Drift's OrderParams for a perp order.
/// Market order with `price` as the worst acceptable price; no trigger, no auction override.
pub fn order_params(side: Side, base_asset_amount: u64, price: u64, market_index: u16, reduce_only: bool) -> Vec<u8> {
    let mut d = Vec::with_capacity(40);
    d.push(0); // order_type: Market
    d.push(1); // market_type: Perp
    d.push(side as u8); // direction
    d.push(0); // user_order_id
    d.extend_from_slice(&base_asset_amount.to_le_bytes());
    d.extend_from_slice(&price.to_le_bytes());
    d.extend_from_slice(&market_index.to_le_bytes());
    d.push(reduce_only as u8);
    d.push(0); // post_only: None
    d.push(0); // bit_flags
    d.push(0); // max_ts: None
    d.push(0); // trigger_price: None
    d.push(0); // trigger_condition: Above
    d.push(0); // oracle_price_offset: None
    d.push(0); // auction_duration: None
    d.push(0); // auction_start_price: None
    d.push(0); // auction_end_price: None
    d
}

/// place_perp_order(params): [state, user w, authority s] + remaining (oracles, spot markets, perp markets)
pub fn place_perp_order<'info>(
    program: &AccountInfo<'info>,
    accts: [&AccountInfo<'info>; 3],
    remaining: &[AccountInfo<'info>],
    params: Vec<u8>,
    signer: &[&[&[u8]]],
) -> Result<()> {
    let [state, user, authority] = accts;
    let mut data = disc("place_perp_order").to_vec();
    data.extend_from_slice(&params);
    let mut accounts = vec![meta(state, false, false), meta(user, true, false), meta(authority, false, true)];
    let mut infos = vec![state.clone(), user.clone(), authority.clone()];
    for r in remaining {
        accounts.push(meta(r, r.is_writable, false));
        infos.push(r.clone());
    }
    invoke_signed(&Instruction { program_id: program.key(), accounts, data }, &infos, signer)?;
    Ok(())
}

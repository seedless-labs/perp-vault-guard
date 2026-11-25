# Design

## The problem

A depositor wants someone else to trade perps with their money, but does not want to trust that person with the money. Most vaults handle this off chain: the manager holds a key and promises to behave. This vault makes the promise a program.

## Roles

- **Owner**: deposits, sets the policy, can kill the vault, and can replace or revoke the manager.
- **Manager**: can call only `open_perp` and `close_perp`. Holds no funds and no perp-account key.
- **Vault PDA** (`["vault", owner]`): owns the USDC account and is the `authority` of the vault's perp user account. The vault program can sign for it only from inside `deposit`, `open_perp` and `close_perp`.

## Limits and their math

Units follow Drift: base 1e9, price 1e6, quote (USDC) 1e6.

```
oracle      = i64 at byte 72 of the perp market account   (amm.historical_oracle_data.last_oracle_price)
band        : |limit - oracle| * 10_000 <= oracle * max_oracle_deviation_bps
notional    = base * max(limit, oracle) / 1e9              (worst price, so the check is never optimistic)
order cap   : notional <= max_order_notional
leverage    : exposure + notional <= deposited * max_leverage_bps / 10_000
```

`open_perp` runs every check before it signs. `close_perp` sends `reduce_only = true`, so the perp program itself refuses anything that grows or flips the position. Because of that, `close_perp` skips the size and leverage checks and stays open after a kill. A killed vault can always be unwound.

The oracle account is checked twice before it is read. It must be owned by the vault's perp program, and it must be that program's `["perp_market", index]` PDA. So the manager cannot pass in an account with a fake price.

## Why the price band exists

The order is a Drift market order, and its `price` field is the worst price the vault will accept. Without a band, a manager could send a long with a limit of 10x the oracle. On a thin book, or against their own resting order on the other side, that is a way to move vault money to themselves. The band caps that loss at `max_oracle_deviation_bps` per fill.

## CPI without the drift crate

The `drift` crate pins an older Anchor, so the CPI is written by hand in `src/drift.rs`. It covers four instructions:

- `initialize_user_stats`
- `initialize_user`
- `deposit`
- `place_perp_order`

Each one is built from the instruction discriminator (`sha256("global:<name>")[..8]`), the account order in Drift's IDL, and a borsh-encoded `OrderParams` (enums as `u8`, options as a tag byte).

Remaining accounts go to Drift unchanged, in the order the SDK sends them: oracles, then spot markets, then perp markets. Drift validates them. The vault validates only the accounts its own checks depend on.

## Known gaps (what I would do next)

1. **Exposure is tracked on order intent, not on fills.** Mainnet Drift market orders fill through an auction, and they can fill partly or not at all. The next step is to read `base_asset_amount` back from the vault's own Drift `User` account after the CPI, and to compute exposure from actual positions times the oracle price.
2. **Leverage uses deposits, not equity.** Unrealised PnL is ignored. That is conservative while the vault is losing, but too strict while it is winning. The fix is to read the user's total collateral from Drift instead.
3. **There is no withdraw path yet.** The owner would call Drift `withdraw` through the vault and then move the USDC out. Withdraw is owner only, and the policy may want it to require `killed`.
4. **One vault per owner.** The seeds are `["vault", owner]`. Adding a `vault_id: u64` to the seeds allows many vaults per owner.
5. **Oracle staleness.** `last_oracle_price` is what Drift last recorded. A production version should also read `last_oracle_price_twap_ts` and reject prices older than N slots, or read the Pyth/Switchboard account directly.
6. **Policy changes apply at once.** A timelock on `set_policy` (and on `set_manager`) would protect depositors if the owner key is a shared or upgradeable authority.
7. **Upgrade authority.** Both programs are deployed with a single-key upgrade authority, which is fine for a devnet demo. For mainnet it should be a Squads multisig or the program should be made immutable. Otherwise every limit above can be removed by one key.

## Mainnet path

1. Init the vault with `perp_program = dRiftyHA39MWEi3m9aunc5MzRF1JYuBsbn6VPcn33UH`. Pass Drift's real state, spot market vault and markets in the same slots the mock uses.
2. Close gaps 1, 3, 5 and 7 above.
3. Run against a mainnet fork with Drift's real accounts (`solana-test-validator --clone`). Drift devnet cannot test deposits or orders today; see the README.

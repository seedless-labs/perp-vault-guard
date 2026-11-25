# perp-vault-guard

A USDC vault on Solana whose manager can trade perps, but only inside limits the owner sets on chain.

**Demo page:** https://seedless-labs.github.io/perp-vault-guard (the devnet run, step by step, with explorer links)

The manager never holds the funds and never holds the key to the perp account. The vault PDA is the perp account's authority, and the only way to make it sign an order is through this program. Each order is checked against the policy before the program signs.

| Limit | Enforced in |
|---|---|
| Allowed markets (up to 8 perp market indexes) | `open_perp` |
| Max order size (USDC notional per order) | `open_perp` |
| Max leverage (gross notional / deposits) | `open_perp` |
| Price band (limit vs the perp program's own oracle) | `open_perp`, `close_perp` |
| Kill switch (once on, only reduce-only orders are allowed) | `kill`, `open_perp` |
| Revoke manager (nobody can trade until a new one is set) | `revoke_manager`, every trade |

The design notes, the limit math and the known gaps are in [DESIGN.md](DESIGN.md).

## Devnet

| | |
|---|---|
| perp-vault-guard | [`Fcfy2AqyC4Yrus9NxmShBZcgxzYvzdz3rWjz17rZy5x`](https://explorer.solana.com/address/Fcfy2AqyC4Yrus9NxmShBZcgxzYvzdz3rWjz17rZy5x?cluster=devnet) |
| perp-mock (MOCK, see below) | [`V8Dip6eZLe2UYU6LSYitwWXrqHG2k2y9Muhd92RbQYW`](https://explorer.solana.com/address/V8Dip6eZLe2UYU6LSYitwWXrqHG2k2y9Muhd92RbQYW?cluster=devnet) |
| Demo vault | [`5sestrYhi6TdSQ7QJYotqpjcdH5Xx5mSGLSLyTFvDMai`](https://explorer.solana.com/address/5sestrYhi6TdSQ7QJYotqpjcdH5Xx5mSGLSLyTFvDMai?cluster=devnet) |

### Last run: 11 of 11 passing ([full output](docs/devnet-run.txt))

Policy for the run: SOL-PERP only, max 3x, max 2,500 USDC per order, 1% price band. Collateral: 1,000 USDC. SOL price: the live oracle price from Drift's devnet SOL-PERP market ($80.02).

| Step | Result |
|---|---|
| Owner creates the vault | [tx](https://explorer.solana.com/tx/wJK31jJv28iDLgHUMGZB1RhbhV27J3revPnfNX2RHfZGQfE23jqQVVf1W1Ns44U57D1FXzqgFAKGcFSbT697cvC?cluster=devnet) |
| Owner deposits 1,000 USDC | [tx](https://explorer.solana.com/tx/6v8Ucm8sjgUdgnaZcVkoCXt5nmhwBbMKxPv8oNegzHmYQRZgdFuGrLCa2AAg6hwNAx8zBZM5a7y8tHv2XmUhAQU?cluster=devnet) |
| Manager opens a 10 SOL long (~805 USDC) | allowed, [tx](https://explorer.solana.com/tx/5d3aLFELyGs1A5Tq2f3p6afNZKxEeGZjUK6L6EFCU8eadhZEKb4XxYG5EFMh9MAXpBbPi3GjjaMmKkfBGSbciiRx?cluster=devnet) |
| Manager sends a 40 SOL order (~3,200 USDC, above the 2,500 cap) | `OrderTooLarge` |
| Manager adds 28 SOL (total ~3,060, above 3x of 1,000) | `LeverageExceeded` |
| Manager tries BTC-PERP | `MarketNotAllowed` |
| Manager sets a limit price 5% off the oracle | `PriceOutOfBand` |
| A key that is not the manager tries to trade | `NotManager` |
| Owner turns on the kill switch, then the manager tries to open | [kill tx](https://explorer.solana.com/tx/4KY3iWCQE4EGEgSNoggYa9hwfzojrBiVJTvsC4PyXLaVgteq6SydG9jN98aQvkaXx3KEzhCrWAYRPNvsJzytq2VV?cluster=devnet), then `VaultKilled` |
| After the kill, the manager closes the 10 SOL (reduce-only) | allowed, [tx](https://explorer.solana.com/tx/kC8ae3W3Joy83VxXnPsYSXnusi8M1ZbXXcu2Buj6YSkFLfs4MAH6CFAfKi2PjRkzyj38ojygdpBD5ztVBRtv4Rn?cluster=devnet) |
| Owner revokes the manager, then the manager tries to close | [revoke tx](https://explorer.solana.com/tx/2BmqQU8NVcBQ8LPsEuDUvK72QVE6BSiLn2C8vhY6BfSPqpTPNhsiReew68QjcdFTqjicwy1DhGAMncicTRbH4Ygb?cluster=devnet), then `NotManager` |

Rejected orders are stopped in simulation, so they leave no tx on chain. The error names come from the program, and the test checks each one.

## About the mock

`programs/perp-mock` is a **test double. It is not an exchange.** It exists because Drift v2's devnet deployment does not accept deposits or orders right now. Its market accounts were never migrated to the current layout:

| Account | Size on devnet | Size in the current Drift IDL |
|---|---|---|
| SpotMarket 0 `6gMq3mRC…` | 776 bytes | 808 bytes |
| PerpMarket 0 `8UJgxaiQ…` | 1216 bytes | 1240 bytes |

Because of this, every `deposit` fails with `SpotMarketNotFound` (6087) and every `place_perp_order` fails with `PerpMarketNotFound` (6078). `scripts/raw-drift-check.ts` reproduces both errors against Drift directly. You can check the sizes yourself with `solana account <address> -u devnet`.

The mock uses the same interface as Drift for the four instructions the vault calls:

- the same instruction names, so the Anchor discriminators match
- the same account order and argument layout
- the same PDA seeds
- the oracle price at the same byte offset in the market account

The vault program does not change between the two. It stores its perp program at `init_vault`, so it can point at Drift on mainnet and at the mock on devnet. Fills in the mock happen instantly at the stored oracle price. There is no AMM, no funding and no liquidation.

## Run it

```bash
anchor build
anchor deploy --provider.cluster devnet   # or use the deployed ids above
npm run devnet                            # needs ~0.2 devnet SOL + 1,000 devnet USDC in ~/.config/solana/id.json
```

To get devnet USDC, use Drift's token faucet (`V4v1mQiAdLz4qwckEb45WqHYceYizoib39cDBHSWfaB`, `mint_to_user`) for mint `8zGuJQqwhZafTah7Uc7Z4tXRnguqkn5KLFAP8oV6PHe2`.

Toolchain: Anchor 0.31.1, Solana CLI 2.x+, Node 20+.

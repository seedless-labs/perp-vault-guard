// Devnet walkthrough: every limit the vault enforces, tried for real.
// Run: npm run devnet   (needs ~0.2 devnet SOL and devnet USDC in ~/.config/solana/id.json)
import * as anchor from "@coral-xyz/anchor";
import { BN, Program } from "@coral-xyz/anchor";
import {
  Connection, Keypair, PublicKey, SystemProgram, SYSVAR_RENT_PUBKEY, LAMPORTS_PER_SOL, Transaction,
} from "@solana/web3.js";
import {
  getAssociatedTokenAddressSync, getOrCreateAssociatedTokenAccount, transfer, TOKEN_PROGRAM_ID, ASSOCIATED_TOKEN_PROGRAM_ID,
} from "@solana/spl-token";
import { expect } from "chai";
import { PerpVaultGuard } from "../target/types/perp_vault_guard";
import { PerpMock } from "../target/types/perp_mock";

const RPC = process.env.RPC_URL ?? "https://api.devnet.solana.com";
// Drift's devnet USDC (from Drift's token faucet), used as the vault's collateral.
const USDC = new PublicKey("8zGuJQqwhZafTah7Uc7Z4tXRnguqkn5KLFAP8oV6PHe2");
// Drift devnet SOL-PERP market. Read only for its live oracle price, so the mock trades at a real number.
const DRIFT_SOL_PERP = new PublicKey("8UJgxaiQx5nTrdDgph5FiahMmzduuLTLf5WmsPegYA6W");

const connection = new Connection(RPC, "confirmed");
const payer = anchor.Wallet.local();
const provider = new anchor.AnchorProvider(connection, payer, { commitment: "confirmed" });
anchor.setProvider(provider);

const vaultProg = anchor.workspace.PerpVaultGuard as Program<PerpVaultGuard>;
const mock = anchor.workspace.PerpMock as Program<PerpMock>;
const P = mock.programId;

const owner = Keypair.generate();
const manager = Keypair.generate();
const stranger = Keypair.generate();

const pda = (seeds: (Buffer | Uint8Array)[], prog = P) => PublicKey.findProgramAddressSync(seeds, prog)[0];
const u16 = (n: number) => { const b = Buffer.alloc(2); b.writeUInt16LE(n); return b; };
const state = pda([Buffer.from("drift_state")]);
const spotVault = pda([Buffer.from("spot_market_vault"), u16(0)]);
const market = (i: number) => pda([Buffer.from("perp_market"), u16(i)]);
const vault = pda([Buffer.from("vault"), owner.publicKey.toBuffer()], vaultProg.programId);
const perpUser = pda([Buffer.from("user"), vault.toBuffer(), u16(0)]);
const perpUserStats = pda([Buffer.from("user_stats"), vault.toBuffer()]);
const vaultUsdc = getAssociatedTokenAddressSync(USDC, vault, true);

const usdc = (n: number) => new BN(n * 1e6);
const sol = (n: number) => new BN(Math.round(n * 1e9));
const link = (sig: string) => `https://explorer.solana.com/tx/${sig}?cluster=devnet`;
const log = (label: string, sig: string) => console.log(`    ${label}\n      ${link(sig)}`);

async function expectErr(p: Promise<unknown>, code: string) {
  try {
    await p;
  } catch (e: any) {
    const got = e?.error?.errorCode?.code ?? String(e);
    expect(String(got)).to.contain(code);
    console.log(`    rejected: ${code}`);
    return;
  }
  throw new Error(`expected ${code}, tx went through`);
}

let oracle = 0; // SOL price, 1e6

const trade = (signer: Keypair, idx: number) => ({
  manager: signer.publicKey, vault, perpProgram: P, perpState: state, perpUser, perpMarket: market(idx),
});
const rem = (idx: number) => [{ pubkey: market(idx), isWritable: false, isSigner: false }];
const open = (signer: Keypair, idx: number, side: "long" | "short", base: BN, price: number) =>
  vaultProg.methods.openPerp(idx, { [side]: {} } as any, base, new BN(price))
    .accountsPartial(trade(signer, idx)).remainingAccounts(rem(idx)).signers([signer]).rpc();
const close = (signer: Keypair, idx: number, side: "long" | "short", base: BN, price: number) =>
  vaultProg.methods.closePerp(idx, { [side]: {} } as any, base, new BN(price))
    .accountsPartial(trade(signer, idx)).remainingAccounts(rem(idx)).signers([signer]).rpc();
const ownerIx = (name: "kill" | "revokeManager") =>
  (vaultProg.methods[name] as any)().accountsPartial({ owner: owner.publicKey, vault }).signers([owner]).rpc();

describe("perp-vault-guard on devnet", () => {
  before(async () => {
    const tx = new Transaction();
    for (const k of [owner, manager, stranger])
      tx.add(SystemProgram.transfer({ fromPubkey: payer.publicKey, toPubkey: k.publicKey, lamports: (k === owner ? 0.1 : 0.01) * LAMPORTS_PER_SOL }));
    await provider.sendAndConfirm(tx);

    const from = getAssociatedTokenAddressSync(USDC, payer.publicKey);
    const ownerAta = await getOrCreateAssociatedTokenAccount(connection, payer.payer, USDC, owner.publicKey);
    await transfer(connection, payer.payer, from, ownerAta.address, payer.payer, 1_000e6);

    const drift = await connection.getAccountInfo(DRIFT_SOL_PERP);
    oracle = Number(drift!.data.readBigInt64LE(72));
    console.log(`    Drift devnet SOL-PERP oracle: $${(oracle / 1e6).toFixed(4)}`);

    // One-time mock setup (idempotent).
    if (!(await connection.getAccountInfo(state)))
      await mock.methods.initialize().accountsPartial({ admin: payer.publicKey, state, usdcMint: USDC, spotMarketVault: spotVault }).rpc();
    for (const [i, px] of [[0, oracle], [1, 69_000e6]] as const) {
      if (!(await connection.getAccountInfo(market(i))))
        await mock.methods.initMarket(i, new BN(px)).accountsPartial({ admin: payer.publicKey, state, market: market(i) }).rpc();
    }
    await mock.methods.setPrice(new BN(oracle)).accountsPartial({ admin: payer.publicKey, state, market: market(0) }).rpc();
  });

  it("owner creates vault: SOL-PERP only, 3x max, 2,500 USDC per order, 1% price band", async () => {
    const policy = {
      allowedMarkets: [0, 0, 0, 0, 0, 0, 0, 0], marketCount: 1,
      maxLeverageBps: 30_000, maxOrderNotional: usdc(2_500), maxOracleDeviationBps: 100,
    };
    const sig = await vaultProg.methods.initVault(manager.publicKey, policy)
      .accountsPartial({
        owner: owner.publicKey, vault, usdcMint: USDC, vaultUsdc, perpProgram: P, perpState: state, perpUser, perpUserStats,
        rent: SYSVAR_RENT_PUBKEY, systemProgram: SystemProgram.programId, tokenProgram: TOKEN_PROGRAM_ID, associatedTokenProgram: ASSOCIATED_TOKEN_PROGRAM_ID,
      }).signers([owner]).rpc();
    log("init_vault", sig);
  });

  it("owner deposits 1,000 USDC as collateral", async () => {
    const sig = await vaultProg.methods.deposit(usdc(1_000))
      .accountsPartial({
        owner: owner.publicKey, vault, usdcMint: USDC, ownerUsdc: getAssociatedTokenAddressSync(USDC, owner.publicKey), vaultUsdc,
        perpProgram: P, perpState: state, perpUser, perpUserStats, spotMarketVault: spotVault, tokenProgram: TOKEN_PROGRAM_ID,
      }).signers([owner]).rpc();
    log("deposit 1,000 USDC", sig);
    expect((await vaultProg.account.vault.fetch(vault)).deposited.toString()).to.eq(usdc(1_000).toString());
  });

  it("manager opens 10 SOL long (~805 USDC): allowed", async () => {
    log("open 10 SOL long", await open(manager, 0, "long", sol(10), Math.floor(oracle * 1.005)));
    const u = await mock.account.mockUser.fetch(perpUser);
    expect(u.positions[0].toString()).to.eq(sol(10).toString());
  });

  it("manager tries 40 SOL in one order (~3,200 USDC > 2,500 cap): rejected", () =>
    expectErr(open(manager, 0, "long", sol(40), Math.floor(oracle * 1.005)), "OrderTooLarge"));

  it("manager tries 28 SOL more (total ~3,060 > 3x of 1,000): rejected", () =>
    expectErr(open(manager, 0, "long", sol(28), Math.floor(oracle * 1.005)), "LeverageExceeded"));

  it("manager tries BTC-PERP (not on the allow list): rejected", () =>
    expectErr(open(manager, 1, "long", new BN(1e7), 69_000e6), "MarketNotAllowed"));

  it("manager sets a limit 5% off the oracle: rejected", () =>
    expectErr(open(manager, 0, "long", sol(1), Math.floor(oracle * 1.05)), "PriceOutOfBand"));

  it("a key that is not the manager tries to trade: rejected", () =>
    expectErr(open(stranger, 0, "long", sol(1), Math.floor(oracle * 1.005)), "NotManager"));

  it("owner hits the kill switch; manager tries to open: rejected", async () => {
    log("kill", await ownerIx("kill"));
    await expectErr(open(manager, 0, "long", sol(1), Math.floor(oracle * 1.005)), "VaultKilled");
  });

  it("after the kill, manager can still close (reduce-only): allowed", async () => {
    log("close 10 SOL (reduce-only)", await close(manager, 0, "short", sol(10), Math.floor(oracle * 0.995)));
    const u = await mock.account.mockUser.fetch(perpUser);
    expect(u.positions[0].toString()).to.eq("0");
  });

  it("owner revokes the manager; manager can not even close: rejected", async () => {
    log("revoke_manager", await ownerIx("revokeManager"));
    await expectErr(close(manager, 0, "short", sol(1), Math.floor(oracle * 0.995)), "NotManager");
  });

  after(() => console.log(`\n    vault ${vault.toBase58()}\n    https://explorer.solana.com/address/${vault.toBase58()}?cluster=devnet`));
});

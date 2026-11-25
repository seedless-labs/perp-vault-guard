// De-risk check: talk to Drift devnet directly (no vault) to confirm orders are accepted.
import { Connection, Keypair, PublicKey, Transaction, TransactionInstruction, SystemProgram, SYSVAR_RENT_PUBKEY, sendAndConfirmTransaction, ComputeBudgetProgram } from "@solana/web3.js";
import { getOrCreateAssociatedTokenAccount, TOKEN_PROGRAM_ID } from "@solana/spl-token";
import { createHash } from "crypto";
import fs from "fs"; import os from "os";
const DRIFT = new PublicKey("dRiftyHA39MWEi3m9aunc5MzRF1JYuBsbn6VPcn33UH");
const FAUCET = new PublicKey("V4v1mQiAdLz4qwckEb45WqHYceYizoib39cDBHSWfaB");
const USDC = new PublicKey("8zGuJQqwhZafTah7Uc7Z4tXRnguqkn5KLFAP8oV6PHe2");
const disc = (n: string) => createHash("sha256").update("global:" + n).digest().subarray(0, 8);
const pda = (s: (Buffer|Uint8Array)[], p = DRIFT) => PublicKey.findProgramAddressSync(s, p)[0];
const u16 = (n: number) => { const b = Buffer.alloc(2); b.writeUInt16LE(n); return b; };
const u64 = (n: bigint) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(n); return b; };
const m = (pubkey: PublicKey, isWritable = false, isSigner = false) => ({ pubkey, isWritable, isSigner });
(async () => {
  const c = new Connection("https://api.devnet.solana.com", "confirmed");
  const me = Keypair.fromSecretKey(Uint8Array.from(JSON.parse(fs.readFileSync(os.homedir() + "/.config/solana/id.json", "utf8"))));
  const state = pda([Buffer.from("drift_state")]);
  const spot0 = pda([Buffer.from("spot_market"), u16(0)]), perp0 = pda([Buffer.from("perp_market"), u16(0)]);
  const spotVault = pda([Buffer.from("spot_market_vault"), u16(0)]);
  const usdcOracle = new PublicKey("9VCioxmni2gDLv11qufWzT3RDERhQE4iY5Gf7NTfYyAV"), solOracle = new PublicKey("3m6i4RFWEDw2Ft4tFHPJtYgmpPe21k56M3FHeWYrgGBz");
  const user = pda([Buffer.from("user"), me.publicKey.toBuffer(), u16(0)]), stats = pda([Buffer.from("user_stats"), me.publicKey.toBuffer()]);
  const ata = await getOrCreateAssociatedTokenAccount(c, me, USDC, me.publicKey);
  const send = async (label: string, ...ixs: TransactionInstruction[]) => {
    try { const sig = await sendAndConfirmTransaction(c, new Transaction().add(ComputeBudgetProgram.setComputeUnitLimit({ units: 400000 }), ...ixs), [me]); console.log("OK", label, sig); }
    catch (e: any) { console.log("FAIL", label, (e.logs || [e.message]).slice(-6).join("\n  ")); }
  };
  const cfg = pda([Buffer.from("faucet_config"), USDC.toBuffer()], FAUCET), mintAuth = pda([Buffer.from("mint_authority"), USDC.toBuffer()], FAUCET);
  await send("faucet", new TransactionInstruction({ programId: FAUCET, keys: [m(cfg), m(USDC, true), m(ata.address, true), m(mintAuth), m(TOKEN_PROGRAM_ID)], data: Buffer.concat([disc("mint_to_user"), u64(10_000_000_000n)]) }));
  if (!(await c.getAccountInfo(user))) await send("init user", 
    new TransactionInstruction({ programId: DRIFT, keys: [m(stats, true), m(state, true), m(me.publicKey), m(me.publicKey, true, true), m(SYSVAR_RENT_PUBKEY), m(SystemProgram.programId)], data: disc("initialize_user_stats") }),
    new TransactionInstruction({ programId: DRIFT, keys: [m(user, true), m(stats, true), m(state, true), m(me.publicKey), m(me.publicKey, true, true), m(SYSVAR_RENT_PUBKEY), m(SystemProgram.programId)], data: Buffer.concat([disc("initialize_user"), u16(0), Buffer.alloc(32)]) }));
  await send("deposit 1000", new TransactionInstruction({ programId: DRIFT, keys: [m(state), m(user, true), m(stats, true), m(me.publicKey, false, true), m(spotVault, true), m(ata.address, true), m(TOKEN_PROGRAM_ID), m(usdcOracle), m(spot0, true), m(USDC)], data: Buffer.concat([disc("deposit"), u16(0), u64(1_000_000_000n), Buffer.from([0])]) }));
  // OrderParams: Market, Perp, Long, uid 0, base 0.1 SOL (1e9 precision), price 0, market 0, !reduceOnly, postOnly None, bitFlags 0, all options None, trigger Above
  const params = Buffer.concat([Buffer.from([0, 1, 0, 0]), u64(100_000_000n), u64(0n), u16(0), Buffer.from([0, 0, 0]), Buffer.from([0, 0]), Buffer.from([0]), Buffer.from([0, 0, 0, 0])]);
  await send("place SOL-PERP market long 0.1", new TransactionInstruction({ programId: DRIFT, keys: [m(state), m(user, true), m(me.publicKey, false, true), m(solOracle), m(usdcOracle), m(spot0), m(perp0, true)], data: Buffer.concat([disc("place_perp_order"), params]) }));
})();

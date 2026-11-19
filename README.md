# OracleLink

Standalone sovereign Substrate chain (Aura + GRANDPA) implementing a cross-chain oracle registry and an append-only verification ledger.

## Assets
- **OCC (Oracle)**: main transferable/accounting currency. Exposed via `Occ` pallet (`Occ::transfer`).
- **TEN (Tien)**: sole gas/fee/transaction-payment currency. System balances and `pallet_transaction_payment` are wired to `Balances` (TEN) only; all extrinsics pay fees in TEN.

## Consensus & node
- **Block production**: Aura (sr25519 authorities)
- **Finality**: GRANDPA
- Standalone sovereign chain (no parachain/collator). Session keys: Aura + Grandpa.

## Governance (no sudo)
- No `pallet-sudo` and no smart-contract pallets (no contracts/EVM/ink!).
- Privileged actions gated by governance origins: Council (`pallet-collective`) super-majority/majority origins composed with `EnsureRoot` for dispatch routing, `pallet-democracy` (referenda), `pallet-scheduler`, `pallet-preimage`, and `pallet-utility`.
- Oracle registry admin origin: `Council` 2/3 proportion-at-least (or `EnsureRoot` dispatch origin where configured).

## Oracle model (Decision A)
- **Registry (`pallet-oracle-registry`)**: governed registration/deployment of per-chain oracles with authorized signer sets and M-of-N thresholds, signer rotation, pause/resume, bounded metadata, and per-oracle nonce/replay tracking.
- **Ledger (`pallet-oracle-ledger`)**: append-only, immutable verification ledger. Signed relay submissions are verified in-runtime against the registry signer set/threshold (no external light-client proofs). Each accepted submission:
  - consumes the oracle nonce (replay protection) and rejects duplicate payload hashes per oracle,
  - performs a historical check over the append-only ledger for the same `(oracle_id, domain_key)` and records an outcome (`New`, `ConsistentWithHistory`, `ConflictDetected`, `Supersedes`),
  - links the OracleLink transaction context `(block_number, extrinsic_index)` and bounded external-chain transaction references attested in the payload,
  - appends an immutable entry and updates a hash-chain accumulator/root (`entry_hash`, `accumulator_root`) for tamper-evidence, emitting `DataAppended`/`VerificationRecorded` (and `ConflictDetected` when applicable).
- Trust boundary: signed relays trusted per declared authorized signer sets/thresholds.

## Workspace layout
- `node/` — CLI, chain spec, Aura/GRANDPA service, RPC
- `runtime/` — runtime composition, OCC/TEN config, governance, oracle pallets (no sudo/contracts)
- `pallets/oracle-registry/` — oracle/chain deployment & signer management
- `pallets/oracle-ledger/` — append-only verification ledger & submission handling

## Build & run
Prereqs: Rust stable toolchain with `wasm32-unknown-unknown`, `rustfmt`, `clippy` (see `rust-toolchain.toml`), protobuf/clang/cmake as required by your environment.

```bash
# build
cargo build --release

# dev node (single authority Alice, Aura+GRANDPA, temp storage)
./target/release/oraclelink-node --dev

# local testnet spec
./target/release/oraclelink-node build-spec --chain local --raw > oraclelink-local.json
```

## Checks
```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

## Extrinsic surface (intended)
System/core + Aura/GRANDPA/timestamp, TEN balances & tx-payment, OCC transfers, governance (council/democracy/scheduler/preimage/utility), and oracle registry/ledger protocol calls. No sudo, no contracts/EVM/ink! pallets.

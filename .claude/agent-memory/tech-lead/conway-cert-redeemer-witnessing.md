# Conway cert script-witness / Cert-redeemer requirements (getScriptWitnessConwayTxCert)

Which Conway certs need a `Cert`/`Certifying` redeemer when their credential is a
Plutus script — verified against `getScriptWitnessConwayTxCert` /
`getConwayScriptsNeeded` in cardano-ledger-conway-1.23.0.0
(eras/conway/impl/src/Cardano/Ledger/Conway/{TxCert,UTxO}.hs).

A cert contributes a `CertifyingPurpose` to `AlonzoScriptsNeeded` (→ needs a Cert
redeemer for Plutus creds) iff `getScriptWitnessConwayTxCert` returns
`credScriptHash cred` for it:

- **Needs a redeemer (script cred):** ConwayUnRegCert (dereg), ConwayDelegCert,
  ConwayRegDelegCert, and ALL THREE DRep gov-certs treated identically —
  **RegDRep**, UnRegDRep, UpdateDRep (`govWitness` → `credScriptHash cred` for each).
  Committee: AuthCommitteeHotKey, ResignCommitteeColdKey (cold cred).
  **`ConwayRegCert cred (SJust deposit)`** — the DEPOSIT-bearing registration
  (RegDepositTxCert) → `credScriptHash cred`.
- **No redeemer:** `ConwayRegCert _ SNothing` (legacy no-deposit StakeRegistration),
  pool certs (always keyhash), GenesisKeyDeleg, MIR.

**dugite mapping gotcha:** dugite's `Certificate::ConwayStakeRegistration` is ALWAYS
the deposit-bearing form (CBOR tag 7) → REQUIRES a redeemer (script cred). The
legacy no-deposit form is `Certificate::StakeRegistration` → no redeemer.

**Two bugs fixed this session (collateral.rs check_script_redeemers +
check_extra_redeemers):**
- `ConwayStakeRegistration` was in the no-redeemer arm → ExtraRedeemer(Cert) FALSE
  REJECTION when a real tx supplied the deposit-registration redeemer. Fixed
  (commit a6639ae520).
- `RegDRep` was in the no-redeemer arm → same ExtraRedeemer false rejection. Fixed
  (commit 01718b8b88). An existing test had encoded the WRONG assumption
  (`RegDRep does not require`) — flipped it; the oracle (live, source-cited)
  proved all three DRep gov-certs are symmetric.

**Direction of risk:** omitting a cert from the requires-redeemer set causes a
FALSE ExtraRedeemer rejection (real tx supplies the redeemer dugite thinks is
extra), not a false acceptance — so under-listing here is the dangerous case.

See [[conway-plutus-v3-cost-model-seeding]].

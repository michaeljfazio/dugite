---
name: forge-pipeline-complete
description: Complete authoritative Haskell forge pipeline — forkBlockForging, checkShouldForge, forgeShelleyBlock, HFC dispatch, KES evolution, VRF, body hash, tiebreaker
type: reference
---

## Source Files (all verified May 2026)

- NodeKernel.hs: `ouroboros-consensus-diffusion/src/ouroboros-consensus-diffusion/Ouroboros/Consensus/NodeKernel.hs`
- Block/Forging.hs: `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Block/Forging.hs`
- Tracers.hs: `ouroboros-consensus-diffusion/src/ouroboros-consensus-diffusion/Ouroboros/Consensus/Node/Tracers.hs`
- Praos.hs: `ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos.hs`
- Praos/Header.hs: `ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos/Header.hs`
- Praos/VRF.hs: `ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos/VRF.hs`
- Praos/Common.hs: `ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos/Common.hs`
- Shelley/Ledger/Forge.hs: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/Forge.hs`
- Shelley/Protocol/Praos.hs: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Protocol/Praos.hs`
- Shelley/Node/Praos.hs: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Node/Praos.hs`
- Shelley/Ledger/SupportsProtocol.hs: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/SupportsProtocol.hs`
- HFC Forging.hs: `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/HardFork/Combinator/Forging.hs`
- HotKey.hs: `ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Ledger/HotKey.hs`
- Mempool/Query.hs: `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/Mempool/Query.hs`
- Cardano/Node.hs: `ouroboros-consensus-cardano/src/ouroboros-consensus-cardano/Ouroboros/Consensus/Cardano/Node.hs`

## Key Facts

### cardanoProtocolVersion (protocol version stamped in blocks)
- **NOT** the on-chain PParams.protocolVersion
- Set in cardano-node: `ProtVer (natVersion @11) 0` normally, `@12` if experimentalHardForksEnabled
- Stored in `BlockConfig.shelleyProtocolVersion`, passed via `cardanoProtocolVersion` field of `CardanoProtocolParams`
- The `shelleyProtocolVersion` in `ShelleyConfig` is exactly what gets put in `hbProtVer` of every forged block header

### lvPoolDistr source
- For Praos: `protocolLedgerView _cfg st = Praos.LedgerView { lvPoolDistr = nesPd, ... }` where `nesPd = SL.NewEpochState{nesPd}` of the TICKED ledger state
- This is `nesPd` — the "set" snapshot pool distribution memoized at epoch boundary, NOT mark, NOT go
- Confirmed: `nesPd` = `ssStakeMarkPoolDistr (esSnapshots es0)` at epoch boundary (from memory file pool-distr-leader-check.md)

### VRF Tiebreaker in Conway
- `RestrictedVRFTiebreaker 5` (hardcoded to 5 slots max distance)
- VRF comparison only if |slotA - slotB| <= 5
- If slot distance > 5 → `ShouldNotSwitch EQ` (first-seen wins, no VRF comparison)
- `pTieBreakVRFValue` = `certifiedOutput . hbVrfRes . headerBody` (the raw VRF output, NOT range-extended)

### KES: updateForgeState vs checkCanForge
- `updateForgeState` calls `HotKey.evolve hotKey (slotToPeriod curSlot)`:
  - If AfterKESEnd: poisons key, returns `UpdateFailed` → `ForgeStateUpdateError`
  - If BeforeKESStart: returns `Updated info` WITHOUT error (no evolve needed yet)
  - If InKESRange: evolves key forward, returns `Updated`
- `checkCanForge` calls `praosCheckCanForge cfg curSlot kesInfo`:
  - Only fires AFTER `updateForgeState` succeeds
  - Only error: `PraosCannotForgeKeyNotUsableYet wallclockPeriod startPeriod` if current wallclock period < key's start period
  - This handles BeforeKESStart edge case from updateForgeState
  - After KES end is handled in updateForgeState (poisons key, UpdateFailed)

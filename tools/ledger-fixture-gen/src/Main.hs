{-# LANGUAGE DataKinds           #-}
{-# LANGUAGE FlexibleContexts    #-}
{-# LANGUAGE NumericUnderscores  #-}
{-# LANGUAGE ScopedTypeVariables #-}
{-# LANGUAGE TypeApplications    #-}
module Main where

-- cardano-ledger-binary
import           Cardano.Ledger.Binary
  ( EncCBOR, serialize', Version )

-- cardano-ledger-core
import           Cardano.Ledger.BaseTypes
  ( EpochNo (..), ShelleyBase, Globals (..)
  , mkActiveSlotCoeff, unsafeBoundRational
  , Network (..), EpochSize (..)
  )
import           Cardano.Ledger.Core    ( eraProtVerLow )

-- cardano-ledger-conway
import           Cardano.Ledger.Conway  ( ConwayEra )
-- Import the ConwayNEWEPOCH STS type and its instances.
-- The () imports pull in the STS instance declarations for all Conway rules.
import           Cardano.Ledger.Conway.Rules
  ( ConwayNEWEPOCH )

-- cardano-ledger-shelley: NewEpochState type
import           Cardano.Ledger.Shelley.LedgerState ( NewEpochState )

-- small-steps: STS machinery
import           Control.State.Transition.Extended
  ( TRC (..), applySTS )

-- Monad plumbing for ShelleyBase
import           Control.Monad.Reader  ( runReaderT )
import           Data.Functor.Identity ( runIdentity )

-- data-default-class: `def` for NewEpochState
import           Data.Default.Class    ( Default (..) )

-- standard
import qualified Data.ByteString       as BS
import           System.Directory      ( createDirectoryIfMissing )
import           System.Environment    ( getArgs )
import           System.FilePath       ( (</>) )

-- cardano-slotting
import           Cardano.Slotting.Time      ( SystemStart (..), mkSlotLength )
import           Cardano.Slotting.EpochInfo ( fixedEpochInfo )

-- time
import           Data.Time.Clock.POSIX ( posixSecondsToUTCTime )


-- ---------------------------------------------------------------------------
-- Globals: mainnet-compatible values.
--
-- The `Globals` record is defined in cardano-ledger-core and evolves between
-- versions. Fields used here cover GHC-9.6.x compatible cardano-ledger at the
-- SHA pinned in tests/conformance/upstream/sources.toml.
--
-- If compilation fails with an unknown field, consult:
--   libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes.hs
-- at the pinned SHA and adjust accordingly.
-- ---------------------------------------------------------------------------
dugiteGlobals :: Globals
dugiteGlobals = Globals
  { epochInfo                     = fixedEpochInfo (EpochSize 432000) (mkSlotLength 1)
  , slotsPerKESPeriod             = 129600
  , stabilityWindow               = 25920
  , randomnessStabilisationWindow = 25920
  , securityParameter             = 2160
  , maxKESEvo                     = 62
  , quorum                        = 5
  , maxLovelaceSupply             = 45_000_000_000_000_000
  , activeSlotCoeff               = mkActiveSlotCoeff . unsafeBoundRational $ 0.05
  , networkId                     = Mainnet
  , systemStart                   = SystemStart $ posixSecondsToUTCTime 1506203091
  }

-- | Run a `ShelleyBase` action using `dugiteGlobals`.
runShelleyBase :: ShelleyBase a -> a
runShelleyBase act = runIdentity $ runReaderT act dugiteGlobals

-- | The Conway protocol version (used for `serialize'`).
conwayVersion :: Version
conwayVersion = eraProtVerLow @ConwayEra

-- | Encode a value as CBOR (Conway protocol version) and write to a file.
encodeFile :: EncCBOR a => FilePath -> a -> IO ()
encodeFile path x = BS.writeFile path $ serialize' conwayVersion x


-- ---------------------------------------------------------------------------
-- Fixture generation helpers
-- ---------------------------------------------------------------------------

-- | Apply the Conway NEWEPOCH STS rule to `st` with signal `epochNo`.
applyNewEpoch
  :: NewEpochState ConwayEra
  -> EpochNo
  -> Either String (NewEpochState ConwayEra)
applyNewEpoch st epochNo =
  case runShelleyBase $
         applySTS @(ConwayNEWEPOCH ConwayEra) (TRC ((), st, epochNo)) of
    Left  failures -> Left $ "NEWEPOCH STS failed (" ++ show (length failures) ++ " failures)"
    Right st'      -> Right st'

-- | Generate one test-case directory with 4 CBOR files.
--
-- ctx and env for NEWEPOCH are both (), which Haskell serializes as
-- CBOR null (0xF6) via `EncCBOR () = encodeNull`.
generateNewEpochVector
  :: FilePath                 -- ^ Output root directory
  -> String                   -- ^ Test name (sub-directory name)
  -> NewEpochState ConwayEra  -- ^ Initial state (before STS transition)
  -> EpochNo                  -- ^ Signal (target epoch)
  -> IO ()
generateNewEpochVector outDir testName st sig = do
  let dir = outDir </> "ConwayNEWEPOCH" </> testName
  createDirectoryIfMissing True dir
  -- ctx = ()  -> CBOR null (0xF6).  EncCBOR () = encodeNull.
  encodeFile (dir </> "conformance_dump_ctx.cbor") ()
  -- env = ()  -> CBOR null (0xF6).
  encodeFile (dir </> "conformance_dump_env.cbor") ()
  -- st = initial NewEpochState (before the epoch transition).
  encodeFile (dir </> "conformance_dump_st.cbor")  st
  -- sig = EpochNo (CBOR uint).
  encodeFile (dir </> "conformance_dump_sig.cbor") sig
  putStrLn $ "  [ok] ConwayNEWEPOCH/" ++ testName


-- ---------------------------------------------------------------------------
-- Test cases
-- ---------------------------------------------------------------------------

-- | Advance `st` through NEWEPOCH for each epoch in the list, returning the
-- final state.  Logs any STS failures but does not abort.
advanceEpochs
  :: NewEpochState ConwayEra
  -> [EpochNo]
  -> IO (NewEpochState ConwayEra)
advanceEpochs st [] = return st
advanceEpochs st (ep:eps) =
  case applyNewEpoch st ep of
    Left  err -> do
      putStrLn $ "  [warn] advancing to " ++ show ep ++ " failed: " ++ err
      advanceEpochs st eps
    Right st' -> advanceEpochs st' eps

main :: IO ()
main = do
  args <- getArgs
  let outDir = case args of
        ["--output-dir", d] -> d
        _                   -> "."

  putStrLn $ "Generating Conway conformance fixtures -> " ++ outDir

  let st0 = def :: NewEpochState ConwayEra

  -- Test case 1: epoch 0 -> 1 (simplest transition from `def` state).
  generateNewEpochVector outDir "test_epoch_0_to_1" st0 (EpochNo 1)

  -- Test case 2: apply 0->1 first, then generate fixtures for 1->2.
  st1 <- case applyNewEpoch st0 (EpochNo 1) of
    Left err -> do
      putStrLn $ "  [warn] could not advance to epoch 1: " ++ err
      return st0
    Right s  -> return s
  generateNewEpochVector outDir "test_epoch_1_to_2" st1 (EpochNo 2)

  -- Test case 3: advance to epoch 4 step-by-step, then generate 4->5.
  st4 <- advanceEpochs st0 [EpochNo 1, EpochNo 2, EpochNo 3, EpochNo 4]
  generateNewEpochVector outDir "test_epoch_4_to_5" st4 (EpochNo 5)

  -- Test case 4: signal far in the future (tests signal > nesEL guard).
  generateNewEpochVector outDir "test_epoch_0_to_100" st0 (EpochNo 100)

  -- Test case 5: signal equal to current epoch (idempotent / no-op).
  generateNewEpochVector outDir "test_epoch_0_same" st0 (EpochNo 0)

  putStrLn "Done."

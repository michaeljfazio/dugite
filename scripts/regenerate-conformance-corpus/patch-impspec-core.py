#!/usr/bin/env python3
"""
Patch cardano-ledger ImpSpec modules to dump ALL test cases as CBOR.

Two files are patched:

1. libs/cardano-ledger-conformance/src/Test/Cardano/Ledger/Conformance/
      ExecSpecRule/Core.hs
   - testConformance: always dump inputs + Haskell output when the env var
     is set (not just on Haskell/Agda divergence).
   - Used by the QuickCheck-based "Constrained Generators" tests:
     ENACT, DELEG, GOVCERT, POOL, CERT, CERTS, GOV.

2. libs/cardano-ledger-conformance/test/Test/Cardano/Ledger/Conformance/
      Imp/Core.hs
   - conformanceHook: always dump inputs + Haskell output when the env var
     is set (not just on divergence).
   - Used by the hook-based ImpSpec tests for every NEWEPOCH (epoch
     boundary) and LEDGER (tx submission) transition.

Output layout for both patches:
  $CONFORMANCE_CBOR_DUMP_PATH/<RuleName>/test_<N>/
    conformance_dump_ctx.cbor
    conformance_dump_env.cbor
    conformance_dump_st.cbor
    conformance_dump_sig.cbor
    conformance_dump_st_out.cbor  (optional: absent when STS fails)

Usage:
  python3 patch-impspec-core.py <ExecSpecRule/Core.hs> <Imp/Core.hs>
"""

import sys
import re


# ──────────────────────────────────────────────────────────────────────────────
# Patch 0: cardano-ledger-conformance.cabal  (add missing build-depends)
# ──────────────────────────────────────────────────────────────────────────────

def patch_cabal_file(path: str) -> None:
    """Add 'filepath' and 'cardano-ledger-api' to the test-suite tests build-depends.

    The patched Imp/Core.hs imports:
      - System.FilePath  (package: filepath — a GHC boot pkg, but needs explicit listing)
      - Test.Cardano.Ledger.Api.DebugTools  (package: cardano-ledger-api)

    These are not in the test-suite's build-depends at the pinned SHA, so we
    patch the cabal file before running 'cabal build'.
    """
    with open(path) as f:
        content = f.read()

    original = content

    # Strategy: find the test-suite section and add missing build-depends.
    # We look for the *first* build-depends entry inside the test-suite block
    # and insert the two missing packages after it.
    #
    # The cabal file uses commas as list separators with each dep on its own line:
    #   build-depends:
    #       base
    #     , other-package
    #
    # We add our two packages after the first dep in the test-suite section.

    # Locate the test-suite block.
    ts_match = re.search(r'^test-suite\s+tests\b', content, re.MULTILINE)
    if not ts_match:
        print(f"[patch-impspec-core] NOTE: no 'test-suite tests' section in {path}; skipping cabal patch")
        return

    ts_start = ts_match.start()

    # Find the next top-level stanza after the test-suite (library, executable,
    # another test-suite, benchmark, etc.) so we only search within the block.
    next_stanza = re.search(
        r'^\S', content[ts_start + 1:], re.MULTILINE
    )
    ts_end = (ts_start + 1 + next_stanza.start()) if next_stanza else len(content)
    ts_block = content[ts_start:ts_end]

    # Idempotency checks.
    if "filepath" in ts_block and "cardano-ledger-api" in ts_block:
        print(f"[patch-impspec-core] NOTE: cabal deps already present; skipping cabal patch")
        return

    # Find 'build-depends:' inside the block.
    bd_match = re.search(r'build-depends:', ts_block)
    if not bd_match:
        print(f"[patch-impspec-core] WARNING: no build-depends in test-suite tests; skipping")
        return

    # Find the first dependency line (comma-prefix style or bare name).
    # Insert our additions right after the 'build-depends:' header line.
    bd_end = ts_start + bd_match.end()  # absolute offset just after 'build-depends:'

    # Build the insertion: two packages.
    to_add = ""
    if "filepath" not in ts_block:
        to_add += "\n    , filepath"
    if "cardano-ledger-api" not in ts_block:
        to_add += "\n    , cardano-ledger-api"

    if to_add:
        content = content[:bd_end] + to_add + content[bd_end:]

    _write_if_changed(path, original, content)


# ──────────────────────────────────────────────────────────────────────────────
# Patch 1: ExecSpecRule/Core.hs  (testConformance — QuickCheck path)
# ──────────────────────────────────────────────────────────────────────────────

def patch_exec_spec_rule_core(path: str) -> None:
    with open(path) as f:
        content = f.read()

    original = content

    # 1. Add new imports after UnliftIO.Environment import
    sentinel = "import UnliftIO.Environment (lookupEnv)\n"
    if sentinel not in content:
        _die(path, "Could not find 'import UnliftIO.Environment (lookupEnv)'")
    new_imports = (
        "import Control.Monad.IO.Class (liftIO)\n"
        "import Data.IORef (IORef, modifyIORef', newIORef, readIORef)\n"
        "import System.IO.Unsafe (unsafePerformIO)\n"
    )
    content = content.replace(sentinel, sentinel + new_imports, 1)

    # 2. Add symbolVal to GHC.TypeLits import
    content = _replace_once(
        content, path,
        "import GHC.TypeLits (KnownSymbol)",
        "import GHC.TypeLits (KnownSymbol, symbolVal)",
    )

    # 3. Add </> to System.FilePath import
    content = _replace_once(
        content, path,
        "import System.FilePath ((<.>))",
        "import System.FilePath ((<.>), (</>))",
    )

    # 4. Add createDirectoryIfMissing to UnliftIO.Directory import
    content = _replace_once(
        content, path,
        "import UnliftIO.Directory (makeAbsolute)",
        "import UnliftIO.Directory (createDirectoryIfMissing, makeAbsolute)",
    )

    # 5. Add global counter before dumpCbor definition
    counter_decl = (
        "{-# NOINLINE conformanceDumpCounter #-}\n"
        "conformanceDumpCounter :: IORef Int\n"
        "conformanceDumpCounter = unsafePerformIO (newIORef 0)\n"
        "\n"
    )
    content = _replace_once(
        content, path,
        "dumpCbor ::\n",
        counter_decl + "dumpCbor ::\n",
    )

    # 6. Patch testConformance to always dump
    old_body = (
        "testConformance ctx trc = property $ do\n"
        "  ConformanceResult implResTest agdaResTest implRes <- runConformance @rule @era ctx trc\n"
        "  globals <- use impGlobalsL\n"
        "  logDoc $ extraInfo @rule @era globals ctx trc (first (T.pack . show) implRes)\n"
        "  checkConformance @rule @_ ctx trc (first (T.pack . show) implResTest) agdaResTest"
    )
    new_body = (
        "testConformance ctx trc@(TRC (env, st, sig)) = property $ do\n"
        "  ConformanceResult implResTest agdaResTest implRes <- runConformance @rule @era ctx trc\n"
        "  globals <- use impGlobalsL\n"
        "  logDoc $ extraInfo @rule @era globals ctx trc (first (T.pack . show) implRes)\n"
        "  -- Always dump when CONFORMANCE_CBOR_DUMP_PATH is set (not divergence-gated).\n"
        "  -- Produces official ImpSpec-generated inputs with authoritative Haskell outputs.\n"
        "  mbyCborDumpPath <- lookupEnv \"CONFORMANCE_CBOR_DUMP_PATH\"\n"
        "  case mbyCborDumpPath of\n"
        "    Just basePath -> do\n"
        "      n <- liftIO $ do\n"
        "        modifyIORef' conformanceDumpCounter (+ 1)\n"
        "        readIORef conformanceDumpCounter\n"
        "      let ruleName = symbolVal (Proxy @rule)\n"
        "          testDir = basePath </> ruleName </> (\"test_\" ++ show n)\n"
        "      createDirectoryIfMissing True testDir\n"
        "      dumpCbor testDir ctx \"conformance_dump_ctx\"\n"
        "      dumpCbor testDir env \"conformance_dump_env\"\n"
        "      dumpCbor testDir st \"conformance_dump_st\"\n"
        "      dumpCbor testDir sig \"conformance_dump_sig\"\n"
        "      case implRes of\n"
        "        Right (st', _) -> dumpCbor testDir st' \"conformance_dump_st_out\"\n"
        "        Left _ -> pure ()\n"
        "    Nothing -> pure ()\n"
        "  checkConformance @rule @_ ctx trc (first (T.pack . show) implResTest) agdaResTest"
    )
    content = _replace_once(content, path, old_body, new_body)

    _write_if_changed(path, original, content)


# ──────────────────────────────────────────────────────────────────────────────
# Patch 2: Imp/Core.hs  (conformanceHook — ImpSpec hook path)
# ──────────────────────────────────────────────────────────────────────────────

def patch_imp_core(path: str) -> None:
    with open(path) as f:
        content = f.read()

    original = content

    # 1. Add new imports after 'import UnliftIO (evaluateDeep)'
    sentinel = "import UnliftIO (evaluateDeep)\n"
    if sentinel not in content:
        _die(path, "Could not find 'import UnliftIO (evaluateDeep)'")
    new_imports = (
        "import Control.Monad.IO.Class (liftIO)\n"
        "import Data.IORef (IORef, atomicModifyIORef', newIORef)\n"
        "import System.FilePath ((<.>), (</>))\n"
        "import System.IO.Unsafe (unsafePerformIO)\n"
        "import Test.Cardano.Ledger.Api.DebugTools (writeCBOR)\n"
        "import UnliftIO.Directory (createDirectoryIfMissing)\n"
        "import UnliftIO.Environment (lookupEnv)\n"
    )
    content = content.replace(sentinel, sentinel + new_imports, 1)

    # 2. Add global counter before conformanceHook definition
    counter_decl = (
        "{-# NOINLINE hookDumpCounter #-}\n"
        "hookDumpCounter :: IORef Int\n"
        "hookDumpCounter = unsafePerformIO (newIORef 0)\n"
        "\n"
    )
    sentinel2 = "conformanceHook ::\n"
    if sentinel2 not in content:
        _die(path, "Could not find 'conformanceHook ::' definition")
    content = content.replace(sentinel2, counter_decl + sentinel2, 1)

    # 3. Patch conformanceHook to always dump when env var is set.
    #    The original function opens with a single `impAnn` call; we prepend
    #    the dump block in a do-expression before it.
    old_hook_start = (
        "conformanceHook globals ctx trc@(TRC (env, state, signal)) impRuleResult =\n"
        "  impAnn (\"Conformance hook (\" <> symbolVal (Proxy @rule) <> \")\") $ do\n"
    )
    new_hook_start = (
        "conformanceHook globals ctx trc@(TRC (env, state, signal)) impRuleResult = do\n"
        "  -- Always dump when CONFORMANCE_CBOR_DUMP_PATH is set (not divergence-gated).\n"
        "  -- Captures NEWEPOCH (epoch boundaries) and LEDGER (tx submissions) vectors.\n"
        "  mbyCborDumpPath <- lookupEnv \"CONFORMANCE_CBOR_DUMP_PATH\"\n"
        "  case mbyCborDumpPath of\n"
        "    Just basePath -> do\n"
        "      let ruleName = symbolVal (Proxy @rule)\n"
        "      n <- liftIO $ atomicModifyIORef' hookDumpCounter (\\i -> (i + 1, i + 1))\n"
        "      let testDir = basePath </> ruleName </> (\"test_\" ++ show n)\n"
        "      createDirectoryIfMissing True testDir\n"
        "      writeCBOR (eraProtVerLow @era) (testDir </> \"conformance_dump_ctx\" <.> \"cbor\") ctx\n"
        "      writeCBOR (eraProtVerLow @era) (testDir </> \"conformance_dump_env\" <.> \"cbor\") env\n"
        "      writeCBOR (eraProtVerLow @era) (testDir </> \"conformance_dump_st\" <.> \"cbor\") state\n"
        "      writeCBOR (eraProtVerLow @era) (testDir </> \"conformance_dump_sig\" <.> \"cbor\") signal\n"
        "      case impRuleResult of\n"
        "        Right (state', _) ->\n"
        "          writeCBOR (eraProtVerLow @era) (testDir </> \"conformance_dump_st_out\" <.> \"cbor\") state'\n"
        "        Left _ -> pure ()\n"
        "    Nothing -> pure ()\n"
        "  impAnn (\"Conformance hook (\" <> symbolVal (Proxy @rule) <> \")\") $ do\n"
    )
    content = _replace_once(content, path, old_hook_start, new_hook_start)

    _write_if_changed(path, original, content)


# ──────────────────────────────────────────────────────────────────────────────
# Helpers
# ──────────────────────────────────────────────────────────────────────────────

def _replace_once(content: str, path: str, old: str, new: str) -> str:
    if old not in content:
        _die(path, f"Could not find:\n{old!r}")
    return content.replace(old, new, 1)


def _write_if_changed(path: str, original: str, content: str) -> None:
    if content == original:
        print(f"ERROR: No changes made to {path}", file=sys.stderr)
        sys.exit(1)
    with open(path, "w") as f:
        f.write(content)
    print(f"[patch-impspec-core] Patched: {path}")


def _die(path: str, msg: str) -> None:
    print(f"ERROR in {path}: {msg}", file=sys.stderr)
    sys.exit(1)


# ──────────────────────────────────────────────────────────────────────────────
# Entry point
# ──────────────────────────────────────────────────────────────────────────────

if __name__ == "__main__":
    if len(sys.argv) not in (3, 4):
        print(
            f"Usage: {sys.argv[0]} <ExecSpecRule/Core.hs> <Imp/Core.hs> [<conformance.cabal>]",
            file=sys.stderr,
        )
        sys.exit(1)
    patch_exec_spec_rule_core(sys.argv[1])
    patch_imp_core(sys.argv[2])
    if len(sys.argv) == 4:
        patch_cabal_file(sys.argv[3])

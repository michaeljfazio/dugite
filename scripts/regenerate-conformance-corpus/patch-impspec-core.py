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

    # Skip past the 'test-suite tests' line itself before searching for the next
    # top-level stanza (library, executable, another test-suite, benchmark, etc.).
    ts_line_end = content.find('\n', ts_start)
    if ts_line_end == -1:
        ts_line_end = len(content)
    else:
        ts_line_end += 1  # include the newline

    next_stanza = re.search(r'^\S', content[ts_line_end:], re.MULTILINE)
    ts_end = (ts_line_end + next_stanza.start()) if next_stanza else len(content)
    ts_block = content[ts_start:ts_end]

    # Idempotency checks.
    if "filepath" in ts_block and "cardano-ledger-api" in ts_block:
        print(f"[patch-impspec-core] NOTE: cabal deps already present; skipping cabal patch")
        return

    # Find 'build-depends:' inside the block.
    bd_match = re.search(r'\bbuild-depends:', ts_block)
    if not bd_match:
        print(f"[patch-impspec-core] WARNING: no build-depends in test-suite tests; skipping")
        return

    # Find the end of the build-depends list by scanning lines after 'build-depends:'.
    # The list ends at the first line that doesn't continue with a leading comma or
    # indented package name — i.e. at the first blank line or a new field name.
    bd_header_abs = ts_start + bd_match.start()
    after_bd = content[bd_header_abs:]

    # Split into lines; the build-depends block is the header + continuation lines.
    bd_lines = after_bd.split('\n')
    # Line 0 is 'build-depends:...'
    # Lines 1+ are continuation lines (leading whitespace + optional comma + name)
    # A continuation line looks like: '      base', '    , other-pkg', etc.
    last_bd_line_idx = 0
    for i in range(1, len(bd_lines)):
        line = bd_lines[i]
        # Continuation: non-empty and starts with whitespace
        if line and (line[0] == ' ' or line[0] == '\t'):
            # Still a continuation if it looks like a dep line or is blank-ish
            stripped = line.strip()
            # Stop if this line is a new cabal field (e.g. 'hs-source-dirs:')
            if stripped and re.match(r'^[a-z]', stripped) and ':' in stripped.split()[0]:
                break
            last_bd_line_idx = i
        elif not line.strip():
            # Blank line ends the build-depends
            break
        else:
            break

    # Absolute offset of end of the last build-depends continuation line.
    insert_abs = bd_header_abs + sum(len(l) + 1 for l in bd_lines[:last_bd_line_idx + 1])
    # -1 to position before the trailing newline of the last dep line
    insert_abs -= 1  # just before the '\n' that ends the last dep line
    insert_abs += 1  # after the last dep's text (end of line content)
    # Actually, just track the exact byte offset:
    # insert_abs = position right after the last character of the last dep line
    # (before the newline), so we append "\n    , pkg" there.
    offset = bd_header_abs
    for i, line in enumerate(bd_lines):
        if i == last_bd_line_idx + 1:
            break
        offset += len(line) + 1  # +1 for '\n'
    # offset is now at the start of bd_lines[last_bd_line_idx + 1]
    # Insert before that (i.e. after the newline of the last dep line)
    insert_abs = offset

    # Build the insertion: two packages appended after the last existing dep.
    to_add = ""
    if "filepath" not in ts_block:
        to_add += "    , filepath\n"
    if "cardano-ledger-api" not in ts_block:
        to_add += "    , cardano-ledger-api\n"

    if to_add:
        content = content[:insert_abs] + to_add + content[insert_abs:]

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

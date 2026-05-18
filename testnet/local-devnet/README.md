# Local Testnet

A 3-node loopback testnet (1 dugite BP, 1 dugite relay, 1 cardano-node
validator) for verifying dugite block production and diffusion against
the Haskell reference implementation. dugite-bp is the sole forger;
cardano-node runs as a passive validator that applies every dugite block
through the Haskell ledger — exact cross-validation with no asymmetric-
fork risk.

See the published documentation page for setup, usage, and verification:
<https://michaeljfazio.github.io/dugite/running/local-testnet.html>
(or `docs/src/running/local-testnet.md` in this repo).

Design spec: `docs/superpowers/specs/2026-05-16-local-testnet-design.md`

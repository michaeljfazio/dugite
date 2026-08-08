# preprod steady-state soak

- commit: `c8c450ccd0`
- duration: 60 min at tip (after a gated catch-up)
- database: `/Users/michaelfazio/Source/dugite/db-preprod` (reused, never wiped)
- oracle: Koios preprod `https://preprod.koios.rest/api/v1`

| metric | value |
|---|---|
| usable samples | 12 (+0 failed reads) |
| worst tip delta vs Koios | 0 slots |
| minimum peers | 8 |
| ERROR/panic lines | 0 |
| LedgerSeq incoherent | 0 |
| genesis-range declines | 0 |
| RSS | first=10336MB last=9457MB |
| Koios epoch at end | 305 |

Verdict: PASS

Raw samples: `samples.tsv`. Node log: `node.log`.

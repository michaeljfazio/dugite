# preprod steady-state soak

- commit: `1045230b7b`
- duration: 60 min at tip (after a gated catch-up)
- database: `/Users/michaelfazio/Source/dugite/db-preprod` (reused, never wiped)
- oracle: Koios preprod `https://preprod.koios.rest/api/v1`

| metric | value |
|---|---|
| usable samples | 12 (+0 failed reads) |
| worst tip delta vs Koios | 36 slots |
| minimum peers | 8 |
| ERROR/panic lines | 0 |
| LedgerSeq incoherent | 0 |
| genesis-range declines | 0 |
| RSS | first=6681MB last=3873MB |
| Koios epoch at end | 305 |

Verdict: PASS

Raw samples: `samples.tsv`. Node log: `node.log`.

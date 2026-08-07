# preprod steady-state soak

- commit: `b6b4a1a835`
- duration: 60 min at tip (after a gated catch-up)
- database: `/Users/michaelfazio/Source/dugite/db-preprod` (reused, never wiped)
- oracle: Koios preprod `https://preprod.koios.rest/api/v1`

| metric | value |
|---|---|
| usable samples | 12 (+0 failed reads) |
| worst tip delta vs Koios | 8 slots |
| minimum peers | 8 |
| ERROR/panic lines | 0 |
| LedgerSeq incoherent | 0 |
| genesis-range declines | 0 |
| RSS | first=3873MB last=4107MB |
| Koios epoch at end | 305 |

Verdict: PASS

Raw samples: `samples.tsv`. Node log: `node.log`.

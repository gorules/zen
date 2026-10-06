# Last performance baseline

Commit: `ef9e853200e81f4ae3c4715e315aeb9bd6879a16` (LANE_VM: log keyed lane evaluation)
Recorded: 2026-10-06 02:15 CEST

Settled 40-fixture set (thin LTO, 1024 rows, 3-run minimum): lane geomean 135.7× / median 134.8× vs old IR 78.0× / 75.4×; faster than the old IR on 35 of 40.

Exploration result (2026-10-06, HEAD `0514f288`, same bench in one sitting): settled 187.2× / 165.4× (baseline sources re-measured at 136.7× / 135.7×), faster than the old IR on 39 of 40; all 107 fixtures 0.78× the baseline time (geomean), none slower.

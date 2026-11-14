# Movement gates (tools/fidelity)

The movement score of a place is `movement_compare.py`'s: rustudio's trace against a Studio capture, per lane.
A change to the character must not lower these. Run: `fidelity.py run --places <place> --skip-studio --skip-perf`
with `RUSTUDIO_FIXED_DT=60 RUSTUDIO_GAME_CLOCK=1 RUSTUDIO_NO_SESSION=1` (and `RS_FIDELITY_TMP` on a disk when
/tmp is short: the Place B project copy is 7 GB).

| place | gate | measured 2026-11-13 (the dynamic Humanoid, the only path) | note |
|---|---|---|---|
| movement-r15 | ≥ 93.4 | 95.3 | |
| movement-r6 | ≥ 93.3 | 95.5 | |
| movement-Place B | ≥ 89.5 | 91.6 (6 of 8 lanes ran in the capture's time on a loaded machine) | |
| movement-nova | ≥ 89.5 | 100.0 | |
| movement-wall-r15 | mean over the three Studio runs within 1.0 of Studio's own mean (86.6) | 85.6 (81.6 / 89.2 / 86.1) | passes |
| movement-wall-r6 | mean over the three Studio runs ≥ 77.4 (**known difference**: Studio's own mean is 80.9) | 77.4 (73.4 / 81.2 / 77.7) | regression guard only; to be raised to "within 1.0 of 80.9" when the wall bounce is closed |

## The wall places

Their lanes (wall_jump, wall_jump_diag, wall_slide, corner) are a character pressed against a wall, which bounces
in Studio and differently in every run. One Studio capture is therefore not a reference a faithful engine can
score 90 against: Studio's three captures (`checks/compare/movement-wall-*/studio`, `studio-r26a`, `studio-r26b`)
score against each other

| | run 1 vs 2 | 1 vs 3 | 2 vs 3 | mean |
|---|---|---|---|---|
| movement-wall-r15 | 83.7 | 85.9 | 90.3 | 86.6 |
| movement-wall-r6 | 85.0 | 74.8 | 82.9 | 80.9 |

The gate (lead's decision, 2026-11-13): rustudio's mean score over the three captures against that mean, within
1.0 point. Computed with `movement_compare.compare(parse_log(<capture>/output.log), parse_log(rustudio/output.log))`
for each of the three.

## Known differences behind these numbers (to do, not signed off)

`checks/humanoid-physics/RESULTS.md` "Open" and `PROGRESS.md`: the wall bounce (R6 77.4 against 80.9), T11
(pushing a server-owned character), T14 (Studio's contact rules and 60 Hz stepping for bodies that are not
characters), T10 (a pushed block lags its pusher by one tick of friction).

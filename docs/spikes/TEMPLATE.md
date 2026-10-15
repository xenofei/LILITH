# <nn> <Spike name>

Date: YYYY-MM-DD

## 1. Question

The rule IDs this spike settles (e.g. ARCH-115, QA-078), and the question in one or two sentences.

## 2. Setup

Exact versions: OS build, GPU and driver, Rust, CMake, Ninja, MSVC, Qt and Qt WebEngine, ComfyUI, any other tool. Commit SHA of the spike code.

## 3. Method

What was built and what was measured, and how to re-run it (exact commands).

## 4. Pass criteria

Copied from the plan before running. Do not edit after the run.

## 5. Measurements

| Metric | n | p50 | p95 | p99 | max | Unit |
|---|---|---|---|---|---|---|

Raw data: written to `spikes/<name>/results/` by the measurement script. Only images are committed; cite other raw output as "(raw output kept privately)".

## 6. Verdict

PASS / FAIL / PARTIAL, with one paragraph of reasoning against the pass criteria.

## 7. Decision

The rule ID and the chosen value (e.g. `QA-078: aqtinstall 3.x`).

## 8. Follow-ups

Spec or plan changes this result requires.

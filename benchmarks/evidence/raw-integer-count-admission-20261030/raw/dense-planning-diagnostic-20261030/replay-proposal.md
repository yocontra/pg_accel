# Fixed-module replay

Replay the approved run1 diagnostic exactly, using unchanged probe.py and its
INT2/INT8 fixtures, five warmups, ten balanced pairs, full parity, cached generic
plan proof, physical limits, and counters. The only candidate change is the
new separately built production18 module containing approved hook-local neutral
input reuse; old baseline-ce24 is unchanged. New output run2 and owned JIT cache;
no mutation of run1 or original failed no-slowdown evidence. New source/module
seals come from production18; all old/root artifacts remain protected.

Original proposal.md describes measurement boundaries and interpretation.
Compare all blocks and stage medians against run1. Do not remove outliers, refit
costs, add iterations, or label this diagnostic as passing original full gates.
Run only after final focused correctness/source review and successful sealed
production build. No performance work may overlap any task compilation/tests.

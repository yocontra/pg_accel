# Fixture-only maintenance control after preserved run2 failure

Run2 stopped in old baseline INT2 warmup0: the prepared result query increased
generic_plans by one but also replanned (upper_group_agg calls +1, 553us). It
retains two warm samples, zero measured samples, terminal1, successful artifact
seals and stopped/no-PID cleanup. No candidate performance conclusion exists.

Matching PG18 source defines pgstat.stat header0x01a5bcb7, hash marker S, kind2
relation key (uint32 kind, uint32 database OID, uint64 object OID), followed by
27 int64 relation counters. Preserved run2/block1-old plans identify relation
16420; its unique serialized key (database5) is at offset60146 in the shutdown
stats file. Decoding that exact entry gives autovacuum_count1 and last_autovacuum
2026-10-31T02:25:39.374387Z; manual ANALYZE02:24:40.576132Z, autoanalyze_count0.
Server log shows old direct proof dispatch ending02:25:36.865Z and prepared
proof ending02:26:29.803Z. Autovacuum therefore occurred during the prepared
proof before the first warmup replan. PG18 vacuum.c vac_update_relstats updates
pg_class visibility/frozen fields through inplace catalog invalidation, and
plancache.c invalidates dependent generic plans on relation invalidations.
This strongly supports normal autovacuum invalidation as cause; it is not a
captured invalidation callback trace nor attribution of original performance
regressions. Preserve original probe.py and run1/run2 unchanged.

New run3 uses a new probe-stable-fixture.py. The sole setup policy change is
CREATE TABLE ... WITH (autovacuum_enabled=false), applied identically to both
old/new arms' owned immutable fixtures. No global/autovacuum GUC, module, query,
planner setting, cost, sample count/order, warmup count, or measured/cached-plan
guard changes. Explicit manual ANALYZE remains before pin/PREPARE/proof.
The fixture is entirely fixed-width INT/BOOL; assert reltoastrelid=0 rather than
invent an unnecessary toast setting. Record table options/identity and flushed
setup-backend insert/update/delete/hot-update/autovacuum/autoanalyze counters
before and after all samples. Require exactly2,000,017 inserts, no updates or
deletes, no autonomous maintenance, and identical before/after corpus state.
Full65row native parity and exact GPU/native/generic-plan guards remain intact.
All maintenance observer SQL runs on the disabled native oracle backend outside
timing and never interleaves the measured backend's counter interval.

This is a diagnostic stable-corpus control, not production guidance to disable
autovacuum and not a replacement for original no-slowdown gates. New owned output
and JIT cache; retain all failures/samples. Execute only once after exact review
approval, successful frozen build, and no overlapping task runtime/compilation.

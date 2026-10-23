# Production candidate3 PG18 terminal result

Session87885 returned exit1. Fresh-derived motivating64MiB passed; unchanged
SQL12164 exited3; SQL121128/256/default and fixed-owner32 negative passed.
`qualification/pg18-candidate3/stop.log` confirms stop and `final-status.log`
confirms no server. Preserve every raw log and installed source/module identity.

## Separate construction pressure failure

SQL12164 line71 failed creating the actual result for:

    SELECT g,h,k,count(*) n,sum(v) s,avg(v) a,count(v) c
    FROM _integer_where_agg WHERE b GROUP BY g,h,k

Requested13,000,221 bytes =12,000,204 key-code bytes +1,000,017 Boolean-mask
bytes. Live55,104,909 =36,000,693 pinned raw +19,104,216 execution bytes retained
from the earlier shape. This attribution is source-backed and reconciles with
the successful fresh64 execution receipt; add direct pool receipt in regression.
Raw plus new artifact49,000,914 fits67,108,864. The failure is before execution
selection, in exact derived construction reservation, not a minimum-workspace
impossibility. Existing reservation only evicts local STORE derived/raw entries;
it cannot release idle grouped workspace/output owners held outside STORE.

Proposed amendment (not implemented by this record): only the cached/ephemeral
host-device artifact builders use a pressure adapter. Establish an exclusive
STORE borrow then drop it before initial reservation. Preserve current reserve
and LRU order. On typed BudgetExceeded only, if exact locally owned idle grouped
charges are positive, release only idle pools and retry the identical protected
reservation once. Do not change raw/transform/global ledger reservation paths.
No hit/success/nonbudget/zero-idle cleanup; active/foreign/ephemeral live owners
remain charged. Actual ledger reservation remains authoritative after cleanup.

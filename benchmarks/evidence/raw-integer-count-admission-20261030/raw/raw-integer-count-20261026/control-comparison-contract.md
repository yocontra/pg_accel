# Predeclared low-cardinality compatibility comparison

Compare detached ce24baf79bf2ebc9b7f2c1189aaaec66ac4be2c9 to the final4
production build using the identical current benchmark binary and exact
arithmetic-v1 dense_int{2,4,8}_count_control fixtures. Each fixture has64
non-NULL keys plus NULL; both original scales1000017 and2000017 are mandatory.

Execute blocks1,2,3,4 in that order: old,new,new,old. Within each block run
INT2,INT4,INT8 in that order. Every invocation uses the existing two-arm
Accel-versus-native-parallel-PG harness with5warmups and10balanced raw samples
at each scale. Thus each of the six cells retains20samples per extension
version. This is block-balanced version comparison, not per-sample paired
old/new timing. Do not present an old/new paired significance test.

Use fresh owned clusters, identical realistic observed settings, default
candidate admission, complete native parity and strict expected-module hashes.
Old and new source/module manifests remain separate; current benchmark identity
is identical in all blocks. No H3 extension, cost override, CPU fallback,
outlier removal, best-block selection, or extra repeat-on-failure is allowed.

Report pooled old-accel-median / new-accel-median separately from each
Accel/native-PG comparison. Predeclared requirement is >=1 in every one of
the six cells, with CV and all blocks retained. A loss, tiny/noisy result,
missing sample, parity/provenance failure, or unexpected routing is diagnostic
failure requiring investigation, not permission to discard/relabel it.

The main22-cell matrix remains FAILED historically and is rerun at final4;
this comparison does not itself amend the checker or establish overall PASS.

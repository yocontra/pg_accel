# Post-run toolchain identity

Recorded after GPU2 completed, not a task-start seal. Read-only verification of
the earlier native-count-foundation qualification1/toolchain.sha256 passed for
all three overlapping files: /usr/bin/clang++, the acpp driver and libacpp-rt.
No toolchain changes were performed by this task. The current symlink resolves
to /Users/contra/Projects/pg_accel/.pgaccel/acpp/metal.

SHA256 values (repository-relative except /usr/bin/clang++):

```
26025fbe8f5bf6f7adf2b2006c03fd45bcfff6689f3c66892b0bc1bc8fa68c06  .acpp-version
34129c71a01a74f7f3b2443521519b2e5447553fa187f5fcafaaf8c42cc192e2  /usr/bin/clang++
6a409354f39d2f462d13d1d2bae59ed891f6a1a641f44f6d8c11a2bd45beb63b  .pgaccel/acpp/current/bin/acpp
e9904144d542debe5e638d6ebd243a97baa8c378577cdd208a02c5c5cf61de13  .pgaccel/acpp/current/lib/libacpp-rt.dylib
```

Native builds retain their configure/build logs and exact commands in the
build-contract runners. Excluded binaries and JIT caches are not reconstructible
from this identity note alone; replay requires the recorded toolchain and source.

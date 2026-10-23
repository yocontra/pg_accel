"""Execute the actual pre-change ordered patch block on a local pinned fixture."""
from pathlib import Path
from scripts.tests.test_setup_acpp_replay import SetupReplayTests

SetupReplayTests.setUpClass()
test = SetupReplayTests("test_fresh_and_repeated_full_dispatcher")
test.setUp()
try:
    source = Path(__file__).with_name("setup_acpp.before.sh").read_text()
    start = "apply_metal_cpp_compat_patch() {\n"
    end = '\nif [ ! -d "$SOFT_FP64_SRC/.git" ]; then\n'
    assert source.count(start) == source.count(end) == 1
    test.body = source[source.index(start):source.index(end)]
    result = test.invoke()
    print(result.stdout, flush=True)
    print("old coverage-only bytes:", test.target_bytes() == test.coverage, flush=True)
    assert test.target_bytes() == test.combined, "OLD_DISPATCHER_OMITS_REPLAY_PATCH"
finally:
    test.doCleanups()

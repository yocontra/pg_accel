#!/usr/bin/env python3
"""Fail closed on omitted, extra, ignored, failed or zero-match pgrx tests."""
import pathlib
import re
import sys


def require(condition, detail):
    if not condition:
        raise AssertionError(detail)


def validate_suite(text, names):
    require(names and len(names) == len(set(names)), "empty/duplicate expected tests")
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    actual = re.findall(r"^test (\S+) \.\.\.", text, re.M)
    require(sorted(actual) == sorted(names), ("test-name mismatch", actual, names))
    starts = re.findall(r"^running (\d+) tests?$", text, re.M)
    require(starts == [str(len(names))], ("test-start count", starts))
    summaries = re.findall(r"^test result: (.+)$", text, re.M)
    require(len(summaries) == 1, ("summary count", summaries))
    require(re.fullmatch(
        rf"ok\. {len(names)} passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out; finished in .+",
        summaries[0],
    ), ("nonpassing summary", summaries))


def validate(text, names):
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    runners = list(re.finditer(r"^[ \t]*Running (?!command\b)(.+)$", text, re.M))
    require(len(runners) in (1, 2), ("runner count", len(runners)))
    require(re.fullmatch(r"unittests src/lib\.rs \(target/debug/deps/pg_accel-[0-9a-f]+\)", runners[0][1]), "unexpected main runner")
    prefix = text[:runners[0].start()]
    require(not re.search(r"^(?:running \d+ tests?|test result:|test \S+ \.\.\.)", prefix, re.M), "test records before main runner")
    end = runners[1].start() if len(runners) == 2 else len(text)
    validate_suite(text[runners[0].end():end], names)
    if len(runners) == 2:
        require(re.fullmatch(r"tests/correctness_tests\.rs \(target/debug/deps/correctness_tests-[0-9a-f]+\)", runners[1][1]), "unexpected auxiliary runner")
        auxiliary = text[runners[1].end():]
        # This fixed binary's 55 tests do not match our qualified lib filters.
        # No arbitrary zero-suite, ignored test or additional runner is allowed.
        require(re.fullmatch(r"\s*running 0 tests\s+test result: ok\. 0 passed; 0 failed; 0 ignored; 0 measured; 55 filtered out; finished in [0-9.]+s\s*", auxiliary), "unexpected auxiliary content")


def selftest():
    names = ["engine::one", "engine::two"]
    header = "     Running unittests src/lib.rs (target/debug/deps/pg_accel-abc123)\n"
    good = header + "running 2 tests\ntest engine::one ... setup\nok\ntest engine::two ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out; finished in 1.00s\n"
    auxiliary = "     Running tests/correctness_tests.rs (target/debug/deps/correctness_tests-def456)\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 55 filtered out; finished in 0.00s\n"
    validate(good, names)
    validate(good.replace("setup", "\x1b[32msetup\x1b[0m"), names)
    validate(good + auxiliary, names)
    bad = [
        good.replace("running 2", "running 0"),
        good.replace("engine::two", "engine::other"),
        good.replace("test engine::two ... ok\n", ""),
        good + "test engine::two ... ok\n",
        good.replace("0 ignored", "1 ignored"),
        good.replace("0 failed", "1 failed"),
        good.replace("2 passed", "0 passed"),
        good.replace("test result: ok.", "test result: FAILED."),
        good + good,
        "running 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 99 filtered out; finished in 0.00s\n",
        good.removeprefix(header),
        good + auxiliary + auxiliary,
        auxiliary + good,
        good + auxiliary.replace("correctness_tests.rs", "unknown.rs"),
        good + auxiliary.replace("55 filtered", "54 filtered"),
        good + auxiliary.replace("0 failed", "1 failed"),
        good + auxiliary.replace("0 ignored", "1 ignored"),
        good + auxiliary.replace("0 measured", "1 measured"),
        good + auxiliary.replace("0 passed", "1 passed"),
        good + auxiliary.replace("running 0", "running 1"),
        good + auxiliary + "test unexpected ... ok\n",
        good + auxiliary.split("     Running", 1)[1],
        good + auxiliary.replace("     Running", "     Missing"),
        good + auxiliary + "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 55 filtered out; finished in 0.00s\n",
        good.replace("unittests src/lib.rs", "unittests other.rs"),
    ]
    for text in bad:
        try:
            validate(text, names)
        except AssertionError:
            continue
        raise AssertionError("invalid receipt accepted")
    for names in [[], ["engine::one", "engine::one"]]:
        try:
            validate(good, names)
        except AssertionError:
            continue
        raise AssertionError("invalid inventory accepted")
    print(f"{len(bad) + 5} checker cases passed")


if __name__ == "__main__":
    if sys.argv[1:] == ["--selftest"]:
        selftest()
    else:
        names = pathlib.Path(sys.argv[2]).read_text().splitlines()
        validate(pathlib.Path(sys.argv[1]).read_text(), names)
        print(f"exact named test qualification: {len(names)} passed, none omitted/ignored")

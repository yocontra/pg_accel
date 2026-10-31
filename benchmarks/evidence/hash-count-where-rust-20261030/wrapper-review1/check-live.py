#!/usr/bin/env python3
"""Fail closed on omitted, extra, ignored, failed or zero-match pgrx tests."""
import pathlib
import re
import sys


def validate(text, names):
    assert names and len(names) == len(set(names)), "empty/duplicate expected tests"
    text = re.sub(r"\x1b\[[0-9;]*m", "", text)
    actual = re.findall(r"^test (\S+) \.\.\.", text, re.M)
    assert sorted(actual) == sorted(names), ("test-name mismatch", actual, names)
    starts = re.findall(r"^running (\d+) tests?$", text, re.M)
    assert starts == [str(len(names))], ("test-start count", starts)
    summaries = re.findall(r"^test result: (.+)$", text, re.M)
    assert len(summaries) == 1, ("summary count", summaries)
    assert re.fullmatch(
        rf"ok\. {len(names)} passed; 0 failed; 0 ignored; 0 measured; \d+ filtered out; finished in .+",
        summaries[0],
    ), ("nonpassing summary", summaries)


def selftest():
    names = ["engine::one", "engine::two"]
    good = "running 2 tests\ntest engine::one ... setup\nok\ntest engine::two ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out; finished in 1.00s\n"
    validate(good, names)
    validate(good.replace("setup", "\x1b[32msetup\x1b[0m"), names)
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
    print("14 checker cases passed")


if __name__ == "__main__":
    if sys.argv[1:] == ["--selftest"]:
        selftest()
    else:
        names = pathlib.Path(sys.argv[2]).read_text().splitlines()
        validate(pathlib.Path(sys.argv[1]).read_text(), names)
        print(f"exact named test qualification: {len(names)} passed, none omitted/ignored")

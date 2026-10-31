# Checker optimization regression

The never-executed wrapper/checker from the first review are retained under
`wrapper-review1/`. They were reconstructed by reversing only the isolation and
explicit-validation changes; hashes exactly reproduce original reviewed bytes:
wrapper `d9605a80aa4a4d16acee23aab19cc176f6d76db92dcf0f4ff42a3c55272b3c0b`,
checker `43c5c2442bcda14d69128210261894158c82dbb069bf231d820d87559015d202`.

Actual tool output, transcribed (not an original redirected process log):

```
PYTHONOPTIMIZE=1 /usr/bin/python3 -I -O check-live.py --selftest
14 checker cases passed
exit 0 (tool chunk fc24a7)
```

The same optimized command against `wrapper-review1/check-live.py` exits 1 with
`AssertionError: invalid receipt accepted`, a negative control demonstrating the
old assert-only checker loses its validation. The fixed checker uses explicit
exceptions even under `-O`; the wrapper additionally invokes Python with `-I`.
Both valid cases and all twelve invalid receipt/inventory cases are retained in
the self-test source. No PostgreSQL run used the older wrapper/checker.

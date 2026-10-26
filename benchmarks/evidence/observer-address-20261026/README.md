# Live observer address correction

## Result and scope

The integration-test connection observer now requests
`pg_catalog.host(inet_server_addr())`, not `inet_server_addr()::text`.
PostgreSQL's explicit inet-to-text conversion includes the network suffix;
the observer deliberately accepts only a numeric IP address, without DNS.
Strict parsing and Unix-socket/port handling remain unchanged.

Current-main hosted CI run 40283805906, macOS PG18 job 121442975804,
failed first with `server address is not numeric: invalid IP address syntax`.
The cleanup latch then caused 48 further live-test failures. The PG19 job
skipped that integration step, so its green result did not cover this issue.
This local qualification is not a claim that hosted CI has passed after the fix.

## Verification

The focused test
`integration_connection::lifetime::tests::observer_registration_uses_numeric_host_and_preserves_socket_identity`
passed against actual IPv4 TCP, IPv6 TCP and Unix-socket connections on each
of PostgreSQL 18 and 19: six runs, one passing test per run. The regression
checks actual client registration, observer reconnection and owned-backend
drain, PostgreSQL IPv4/IPv6 formatting, rejection of CIDR/DNS strings, and
preservation of the socket directory and port. These are fresh vanilla
clusters; no pg_accel installation or GPU execution is required by this test.

Both run terminal receipts are zero. Both servers were stopped and had no
remaining PID file; the captured status reports no server running. Cluster
data directories are intentionally not included in this evidence package.
The archived wrapper is the actual bounded local recipe, with environment-
specific paths retained rather than presented as a portable installer.

Strict integration-enabled benchmark Clippy passed (`bench-clippy14-transport.log`).
It also passed on the final formatted source (`bench-clippy15-transport-final.log`).
The original formatting check failed (`fmt15-observer.log`); a subsequent
line-wrap-only correction passed a fresh check (`fmt16-observer.log`).
Source review and receipt review were independent of the implementation.

## Exact source boundary

The six live runs and initial Clippy tested integration_connection.rs SHA-256
`9da9a6d523d9aea41d2c996811c251a94cf8f1c41ec8ef73e8b2824359962510`.
The final formatting-only source has SHA-256
`667c7a6d1886bf9d00b5e0cd80c768e1b0ec20adde7260ed941032f46fa6f145`.
The reviewed change between these snapshots only wraps the observer binding;
the live tests were not rerun after that whitespace-only change.

Input manifests retain the tested lockfile, package, source, wrapper and
PostgreSQL executable identities, with after-run verification receipts.
Other uncommitted OLAP work existed in the shared checkout; these focused
checks do not establish its performance or complete-suite qualification.
Only the address fix and its regression are part of this milestone.

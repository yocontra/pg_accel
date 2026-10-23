# Cancellation diagnostic boundaries

The caught test error was SQLSTATE P0001, `bounded cancellation unexpectedly completed`.
The observed planner reason was `generic_cost_not_competitive`: planning the fresh query with the one-row test hook includes its very large predicted lifecycle-call cost.

The raw notice's `kernels_since_test_reset=7` label is inaccurate. `pg_accel_reset_stats()` does not reset the native kernel counter. Seven is a cumulative snapshot including setup/warm work, not a measured cancellation-attempt delta. This diagnostic does not establish zero cancellation dispatches or seven cancellation calls.

The diagnostic SQL succeeded (exit 0). Its original wrapper subsequently exited 1 because `status` is read-only in zsh. The PostgreSQL shutdown had already succeeded. The retained recovery receipts independently confirm `pg_ctl status` 3, absent postmaster PID, unchanged installed module and source identities, and removal of the empty owned socket directory.

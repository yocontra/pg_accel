//! Shared live-PostgreSQL connection defaults for opt-in integration tests.

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Return the libpq connection string used by live integration tests.
///
/// `PG_ACCEL_TEST_CONNECTION` is the explicit override. Without it, use the
/// pgrx-managed local cluster for `PG_ACCEL_TEST_PG_MAJOR`, defaulting to the
/// repository's current supported major.
pub fn test_connection() -> String {
    std::env::var("PG_ACCEL_TEST_CONNECTION").unwrap_or_else(|_| {
        let pg_major = std::env::var("PG_ACCEL_TEST_PG_MAJOR")
            .or_else(|_| std::env::var("PG_ACCEL_DEFAULT_PG_MAJOR"))
            .unwrap_or_else(|_| "18".to_owned());
        format!("host=localhost port=288{pg_major} dbname=postgres")
    })
}

/// Serialize live PostgreSQL integration tests inside this test process.
///
/// The integration suite intentionally exercises the same permanent benchmark
/// fixture names and per-backend pg_accel counters as the real harness. Running
/// those tests concurrently through Rust's default test scheduler can make one
/// test drop/truncate fixtures or reset stats while another is asserting a plan
/// or counter delta. Holding this guard preserves the exact benchmark SQL while
/// keeping the default `cargo test --features integration_tests` path stable.
#[cfg(not(test))]
pub fn live_pg_test_lock() -> MutexGuard<'static, ()> {
    static LIVE_PG_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    LIVE_PG_TEST_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub use lifetime::*;

/// Test-only server lifetime tracking; never linked into the benchmark binary.
#[cfg(test)]
mod lifetime {
    use super::{Mutex, MutexGuard, OnceLock};
    use std::cell::Cell;
    use std::net::IpAddr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    const EXIT_TIMEOUT: Duration = Duration::from_secs(15);
    static SERIAL: Mutex<()> = Mutex::new(());
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);
    thread_local! { static TOKEN: Cell<Option<u64>> = const { Cell::new(None) }; }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct BackendIdentity {
        pub pid: i32,
        pub started: String,
        pub database: String,
        pub postmaster: String,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum State {
        IdentityOnly,
        LedgerActive,
        Failed { ledger_active: bool },
    }

    impl State {
        const fn ledger_active(self) -> bool {
            matches!(
                self,
                Self::LedgerActive
                    | Self::Failed {
                        ledger_active: true
                    }
            )
        }

        fn fail(&mut self) {
            *self = Self::Failed {
                ledger_active: self.ledger_active(),
            };
        }
    }

    #[derive(Clone)]
    struct Scope {
        token: u64,
        state: State,
        identities: Vec<BackendIdentity>,
        // Contains authentication material: deliberately never Debug/printed.
        observer: Option<tokio_postgres::Config>,
    }

    #[derive(Default)]
    struct Registry {
        scope: Option<Scope>,
        failure: Option<String>,
    }

    fn registry() -> MutexGuard<'static, Registry> {
        REGISTRY
            .get_or_init(|| Mutex::new(Registry::default()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Explicit token captured on the guarded caller before transferring clients.
    pub fn live_pg_scope() -> Result<u64, String> {
        TOKEN
            .get()
            .ok_or_else(|| "live PostgreSQL connection has no owning scope".to_owned())
    }

    fn snapshot(token: u64) -> Result<Scope, String> {
        let registry = registry();
        snapshot_from(&registry, token)
    }

    fn snapshot_from(registry: &Registry, token: u64) -> Result<Scope, String> {
        let scope = registry
            .scope
            .as_ref()
            .ok_or("missing live PostgreSQL scope")?;
        if scope.token != token {
            return Err("live PostgreSQL scope token mismatch".to_owned());
        }
        Ok(scope.clone())
    }

    fn latch(error: &str) {
        let mut registry = registry();
        record_failure(&mut registry, error);
    }

    fn record_failure(registry: &mut Registry, error: &str) {
        if let Some(scope) = &mut registry.scope {
            scope.state.fail();
        }
        registry.failure.get_or_insert_with(|| error.to_owned());
    }

    pub struct LivePgTestGuard {
        token: u64,
        // Held until Drop's observation and failure latch have completed.
        _serial: MutexGuard<'static, ()>,
    }

    pub fn live_pg_test_lock() -> LivePgTestGuard {
        let serial = SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let token = NEXT_SCOPE.fetch_add(1, Ordering::Relaxed);
        {
            let mut registry = registry();
            begin_scope(&mut registry, token).expect("acquire clean live-test scope");
        }
        TOKEN.set(Some(token));
        LivePgTestGuard {
            token,
            _serial: serial,
        }
    }

    fn begin_scope(registry: &mut Registry, token: u64) -> Result<(), String> {
        if let Some(error) = &registry.failure {
            return Err(format!("prior live-test cleanup failed: {error}"));
        }
        if registry.scope.is_some() {
            return Err("live-test scope still registered".to_owned());
        }
        registry.scope = Some(Scope {
            token,
            state: State::IdentityOnly,
            identities: Vec::new(),
            observer: None,
        });
        Ok(())
    }

    /// Record a successful connection before extension work. Only the original
    /// synchronous client is used here; its existing connection policy is unchanged.
    pub fn register_live_pg_client(
        client: &mut postgres::Client,
        connection: &str,
        token: u64,
    ) -> Result<BackendIdentity, String> {
        let result = register_inner(client, connection, token);
        if let Err(error) = &result {
            latch(error);
        }
        result
    }

    fn register_inner(
        client: &mut postgres::Client,
        connection: &str,
        token: u64,
    ) -> Result<BackendIdentity, String> {
        let previous = snapshot(token)?;
        if matches!(previous.state, State::Failed { .. }) {
            return Err("live-test scope already failed".to_owned());
        }
        let row = client
            .query_one(
                "SELECT pg_backend_pid(), extract(epoch FROM backend_start)::text,
                    current_database(), extract(epoch FROM pg_postmaster_start_time())::text,
                    pg_catalog.host(inet_server_addr()), inet_server_port(), current_user::text
             FROM pg_stat_activity WHERE pid=pg_backend_pid()",
                &[],
            )
            .map_err(|error| format!("capture owned backend identity: {error}"))?;
        let identity = BackendIdentity {
            pid: row
                .try_get(0)
                .map_err(|error| format!("decode owned PID: {error}"))?,
            started: row
                .try_get(1)
                .map_err(|error| format!("decode owned start: {error}"))?,
            database: row
                .try_get(2)
                .map_err(|error| format!("decode owned database: {error}"))?,
            postmaster: row
                .try_get(3)
                .map_err(|error| format!("decode owned postmaster: {error}"))?,
        };
        if previous.identities.first().is_some_and(|first| {
            first.postmaster != identity.postmaster || first.database != identity.database
        }) {
            return Err("owned backend connected to a different server/database".to_owned());
        }
        // Record ownership before any subsequent configuration can fail.
        {
            let mut registry = registry();
            let scope = registry
                .scope
                .as_mut()
                .ok_or("scope disappeared during registration")?;
            if scope.token != token {
                return Err("scope changed during registration".to_owned());
            }
            scope.identities.push(identity.clone());
            drop(registry);
        }
        if previous.observer.is_none() {
            let original: postgres::Config = connection
                .parse()
                .map_err(|error| format!("parse observer authentication: {error}"))?;
            let mut config = tokio_postgres::Config::new();
            let current_user: String = row
                .try_get(6)
                .map_err(|error| format!("decode observer role: {error}"))?;
            config
                .user(original.get_user().unwrap_or(&current_user))
                .dbname(&identity.database);
            if let Some(password) = original.get_password() {
                config.password(password);
            }
            if let Some(address) = row
                .try_get::<_, Option<String>>(4)
                .map_err(|error| format!("decode server address: {error}"))?
            {
                let address: IpAddr = address
                    .parse()
                    .map_err(|error| format!("server address is not numeric: {error}"))?;
                config.hostaddr(address).port(
                    u16::try_from(
                        row.try_get::<_, i32>(5)
                            .map_err(|error| format!("decode server port: {error}"))?,
                    )
                    .map_err(|error| format!("server port: {error}"))?,
                );
            } else {
                #[cfg(unix)]
                match original.get_hosts() {
                    [postgres::config::Host::Unix(path)] => {
                        config.host_path(path);
                        config.port(original.get_ports().first().copied().unwrap_or(5432));
                    }
                    _ => {
                        return Err(
                            "Unix observer requires one explicit original socket directory"
                                .to_owned(),
                        );
                    }
                }
                #[cfg(not(unix))]
                return Err("Unix observer unavailable on this platform".to_owned());
            }
            config.connect_timeout(EXIT_TIMEOUT).options("-c statement_timeout=10000 -c lock_timeout=10000 -c pg_accel.gpu_enabled=off -c pg_accel.enabled=off");
            let mut registry = registry();
            let scope = registry
                .scope
                .as_mut()
                .ok_or("scope disappeared configuring observer")?;
            if scope.token != token {
                return Err("scope changed configuring observer".to_owned());
            }
            scope.observer = Some(config);
            drop(registry);
        }
        Ok(identity)
    }

    pub fn activate_live_pg_ledger(token: u64) -> Result<(), String> {
        let result = (|| {
            let scope = snapshot(token)?;
            if !activation_needed(scope.state)? {
                return Ok(());
            }
            observe(&scope, &[], Some(0), "ledger activation")?;
            let mut registry = registry();
            let current = registry
                .scope
                .as_mut()
                .ok_or("scope disappeared during activation")?;
            if current.token != token || current.state != State::IdentityOnly {
                return Err("scope changed during activation".to_owned());
            }
            current.state = State::LedgerActive;
            drop(registry);
            Ok(())
        })();
        if let Err(error) = &result {
            latch(error);
        }
        result
    }

    fn activation_needed(state: State) -> Result<bool, String> {
        match state {
            State::IdentityOnly => Ok(true),
            State::LedgerActive => Ok(false),
            State::Failed { .. } => Err("cannot activate failed live-test scope".to_owned()),
        }
    }

    #[cfg(feature = "integration_tests")]
    pub fn require_live_pg_ledger() -> Result<(), String> {
        let registry = registry();
        if registry.failure.is_some()
            || !registry
                .scope
                .as_ref()
                .is_some_and(|scope| scope.state == State::LedgerActive)
        {
            return Err(
                "GPU-capable test work requires an active, healthy ledger scope".to_owned(),
            );
        }
        drop(registry);
        Ok(())
    }

    /// Exact recorded identity lookup for existing per-phase teardown controls.
    #[cfg(feature = "integration_tests")]
    pub fn owned_live_pg_identities(pids: &[i32]) -> Result<Vec<BackendIdentity>, String> {
        let scope = snapshot(live_pg_scope()?)?;
        pids.iter()
            .map(|pid| {
                scope
                    .identities
                    .iter()
                    .rev()
                    .find(|identity| identity.pid == *pid)
                    .cloned()
                    .ok_or_else(|| format!("PID {pid} was not registered by this test"))
            })
            .collect()
    }

    #[cfg(feature = "integration_tests")]
    pub fn wait_for_owned_live_pg_exit(
        pids: &[i32],
        expected_bytes: i64,
        phase: &str,
    ) -> Result<(), String> {
        let result = (|| {
            let scope = snapshot(live_pg_scope()?)?;
            let identities = owned_live_pg_identities(pids)?;
            observe(&scope, &identities, Some(expected_bytes), phase)
        })();
        if let Err(error) = &result {
            latch(error);
        }
        result
    }

    fn observation_complete(
        expected: &BackendIdentity,
        postmaster: &str,
        recovery: bool,
        active: &[BackendIdentity],
        ledger: Option<i64>,
        expected_bytes: Option<i64>,
    ) -> Result<bool, String> {
        if expected.postmaster != postmaster || recovery {
            return Err("watchdog observed postmaster change or recovery".to_owned());
        }
        if expected_bytes.is_some() && ledger.is_none() {
            return Err("active ledger observation missing".to_owned());
        }
        Ok(active.is_empty() && ledger == expected_bytes)
    }

    fn activity_matches(
        identity: &BackendIdentity,
        pid: i32,
        started: Option<&str>,
        database: Option<&str>,
    ) -> Result<bool, String> {
        if pid != identity.pid {
            return Ok(false);
        }
        let started = started.ok_or_else(|| {
            format!("PID {pid}: backend_start hidden or NULL; cannot prove owned exit")
        })?;
        if started != identity.started {
            return Ok(false);
        }
        let database = database.ok_or_else(|| {
            format!("PID {pid}: matching backend_start but database hidden or NULL")
        })?;
        Ok(database == identity.database)
    }

    fn cleanup_safely(cleanup: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup)).unwrap_or_else(|_| {
            Err("live-test cleanup panicked; original unwind preserved".to_owned())
        })
    }

    fn observe(
        scope: &Scope,
        identities: &[BackendIdentity],
        expected_bytes: Option<i64>,
        phase: &str,
    ) -> Result<(), String> {
        observe_with_timeout(scope, identities, expected_bytes, phase, EXIT_TIMEOUT)
    }

    fn observe_with_timeout(
        scope: &Scope,
        identities: &[BackendIdentity],
        expected_bytes: Option<i64>,
        phase: &str,
        timeout: Duration,
    ) -> Result<(), String> {
        if scope.identities.is_empty() {
            return if expected_bytes.is_none() {
                Ok(())
            } else {
                Err("cannot activate ledger without registered backend".to_owned())
            };
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("observer runtime: {error}"))?;
        let deadline = tokio::time::Instant::now() + timeout;
        let result = runtime.block_on(observe_async(
            scope,
            identities,
            expected_bytes,
            phase,
            deadline,
        ));
        // No DNS/spawn_blocking: hostaddr or explicit Unix socket only.
        runtime.shutdown_timeout(deadline.saturating_duration_since(tokio::time::Instant::now()));
        result
    }

    async fn observe_async(
        scope: &Scope,
        identities: &[BackendIdentity],
        expected_bytes: Option<i64>,
        phase: &str,
        deadline: tokio::time::Instant,
    ) -> Result<(), String> {
        let config = scope
            .observer
            .as_ref()
            .ok_or("registered backend has no bounded observer")?;
        let (client, connection) =
            tokio::time::timeout_at(deadline, config.connect(tokio_postgres::NoTls))
                .await
                .map_err(|error| format!("{phase}: observer connect deadline: {error}"))?
                .map_err(|error| format!("{phase}: observer connect: {error}"))?;
        let task = tokio::spawn(connection);
        let operation = async {
            loop {
                let state = client.query_one("SELECT extract(epoch FROM pg_postmaster_start_time())::text, pg_is_in_recovery(), current_database()", &[]).await.map_err(|error| format!("{phase}: server identity: {error}"))?;
                let first = &scope.identities[0];
                let database: String = state
                    .try_get(2)
                    .map_err(|error| format!("{phase}: decode observer database: {error}"))?;
                if database != first.database {
                    return Err(format!("{phase}: wrong observer database"));
                }
                let postmaster: String = state
                    .try_get(0)
                    .map_err(|error| format!("{phase}: decode postmaster: {error}"))?;
                let recovery: bool = state
                    .try_get(1)
                    .map_err(|error| format!("{phase}: decode recovery: {error}"))?;
                observation_complete(first, &postmaster, recovery, &[], None, None)?;
                let pids: Vec<_> = identities.iter().map(|identity| identity.pid).collect();
                let rows = client.query("SELECT pid, extract(epoch FROM backend_start)::text, datname FROM pg_stat_activity WHERE pid=ANY($1)", &[&pids]).await.map_err(|error| format!("{phase}: owned activity: {error}"))?;
                let mut active = Vec::new();
                for row in rows {
                    let pid: i32 = row
                        .try_get(0)
                        .map_err(|error| format!("{phase}: decode activity PID: {error}"))?;
                    let started: Option<String> = row
                        .try_get(1)
                        .map_err(|error| format!("{phase}: decode activity start: {error}"))?;
                    let database: Option<String> = row
                        .try_get(2)
                        .map_err(|error| format!("{phase}: decode activity database: {error}"))?;
                    for identity in identities {
                        if activity_matches(identity, pid, started.as_deref(), database.as_deref())?
                        {
                            active.push(identity.clone());
                        }
                    }
                }
                let ledger = if expected_bytes.is_some() {
                    Some(
                        client
                            .query_one("SELECT pg_accel_resident_live_bytes()", &[])
                            .await
                            .map_err(|error| format!("{phase}: ledger: {error}"))?
                            .try_get::<_, i64>(0)
                            .map_err(|error| format!("{phase}: decode ledger: {error}"))?,
                    )
                } else {
                    None
                };
                if observation_complete(
                    first,
                    &postmaster,
                    recovery,
                    &active,
                    ledger,
                    expected_bytes,
                )? {
                    return Ok(());
                }
                // Initial activation is an assertion, not a drain of external bytes.
                if identities.is_empty() {
                    return Err(format!(
                        "{phase}: expected exact ledger {expected_bytes:?}, observed {ledger:?}"
                    ));
                }
                if tokio::time::Instant::now() + Duration::from_millis(25) >= deadline {
                    return Err(format!(
                        "{phase}: exit deadline: active={active:?}, ledger={ledger:?}, expected={expected_bytes:?}"
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        };
        let result = tokio::time::timeout_at(deadline, operation)
            .await
            .map_err(|error| format!("{phase}: observer query deadline: {error}"))
            .and_then(std::convert::identity);
        drop(client);
        task.abort();
        // Same deadline, including abort/drain. Runtime owns the socket and
        // drops it even when the deadline leaves no time to poll the task.
        let drained = tokio::time::timeout_at(deadline, task).await;
        if drained.is_err() {
            return Err(format!("{phase}: observer task drain deadline; {result:?}"));
        }
        result
    }

    impl Drop for LivePgTestGuard {
        fn drop(&mut self) {
            let result = cleanup_safely(|| {
                snapshot(self.token).and_then(|scope| {
                    observe(
                        &scope,
                        &scope.identities,
                        scope.state.ledger_active().then_some(0),
                        "test scope exit",
                    )
                })
            });
            if let Err(error) = &result {
                latch(error);
            }
            let failure = {
                let mut registry = registry();
                registry.scope = None;
                registry.failure.clone()
            };
            TOKEN.set(None);
            if let Some(error) = failure {
                if std::thread::panicking() {
                    use std::io::Write as _;
                    let _ = writeln!(
                        std::io::stderr(),
                        "live-test cleanup failed during unwind: {error}"
                    );
                } else {
                    panic!("live-test cleanup failed: {error}");
                }
            }
        }
    }

    mod tests {
        use super::*;

        fn identity() -> BackendIdentity {
            BackendIdentity {
                pid: 42,
                started: "1000.000001".to_owned(),
                database: "test".to_owned(),
                postmaster: "900.000001".to_owned(),
            }
        }

        fn scope() -> Scope {
            Scope {
                token: 7,
                state: State::IdentityOnly,
                identities: Vec::new(),
                observer: None,
            }
        }

        #[test]
        fn lazy_empty_scope_never_requires_connection_or_extension() {
            assert!(observe(&scope(), &[], None, "no client").is_ok());
            assert!(observe(&scope(), &[], Some(0), "activation without client").is_err());
            let guard = live_pg_test_lock();
            assert!(
                snapshot(guard.token)
                    .expect("lazy registry")
                    .observer
                    .is_none()
            );
            drop(guard);
        }

        #[cfg(feature = "integration_tests")]
        #[test]
        fn observer_registration_uses_numeric_host_and_preserves_socket_identity() {
            let _guard = live_pg_test_lock();
            let connection = crate::integration_connection::test_connection();
            let mut client = postgres::Client::connect(&connection, postgres::NoTls)
                .expect("connect observer registration fixture");
            let token = live_pg_scope().expect("owning live-test scope");
            register_live_pg_client(&mut client, &connection, token)
                .expect("register bounded observer without a network suffix");

            // Explicit inet::text retains /32 or /128, unlike host(inet).
            // Exercise PostgreSQL's real formatter for both address families;
            // accepting host names or stripping arbitrary suffixes in Rust is
            // not a substitute for the strict numeric observer endpoint.
            for row in client
                .query(
                    "SELECT address::text, pg_catalog.host(address)
                     FROM (VALUES ('127.0.0.1'::inet), ('::1'::inet),
                                  ('2001:db8::1/64'::inet)) AS fixtures(address)",
                    &[],
                )
                .expect("real PostgreSQL address formatting")
            {
                let legacy: String = row.get(0);
                let host: String = row.get(1);
                assert!(legacy.contains('/'), "explicit inet cast must retain mask");
                assert!(legacy.parse::<IpAddr>().is_err());
                assert!(host.parse::<IpAddr>().is_ok());
            }
            for invalid in ["localhost", "127.0.0.1/32", "::1/128", "[::1]", ""] {
                assert!(invalid.parse::<IpAddr>().is_err());
            }

            let address: Option<String> = client
                .query_one("SELECT pg_catalog.host(inet_server_addr())", &[])
                .expect("actual connected server address")
                .get(0);
            let scope = snapshot(token).expect("registered identity-only scope");
            let observer = scope
                .observer
                .as_ref()
                .expect("bounded observer configured");
            if let Some(address) = address {
                let numeric: IpAddr = address.parse().expect("bare actual server IP");
                assert_eq!(observer.get_hostaddrs(), &[numeric]);
                assert!(observer.get_hosts().is_empty(), "observer must not use DNS");
            } else {
                #[cfg(unix)]
                {
                    let original: postgres::Config = connection.parse().expect("socket config");
                    let [postgres::config::Host::Unix(expected)] = original.get_hosts() else {
                        panic!("Unix fixture requires one explicit socket directory");
                    };
                    let [tokio_postgres::config::Host::Unix(actual)] = observer.get_hosts() else {
                        panic!("observer must preserve the explicit socket directory");
                    };
                    assert_eq!(actual, expected);
                    assert!(observer.get_hostaddrs().is_empty());
                    assert_eq!(
                        observer.get_ports(),
                        &[original.get_ports().first().copied().unwrap_or(5432)]
                    );
                }
                #[cfg(not(unix))]
                panic!("Unix fixture unavailable on this platform");
            }
            observe(&scope, &[], None, "numeric or socket observer connection")
                .expect("observer reaches the same server without extension activation");
            drop(client);
            observe(&scope, &scope.identities, None, "registered fixture exit")
                .expect("exact owned backend exits before guard cleanup");
        }

        #[test]
        fn exact_identity_requires_known_start_and_handles_reused_pid_null_database() {
            let owned = identity();
            assert!(
                activity_matches(&owned, 42, Some(&owned.started), Some("test"))
                    .expect("same identity")
            );
            assert!(
                !activity_matches(&owned, 42, Some("2000.0"), None)
                    .expect("PID reused by background worker")
            );
            assert!(!activity_matches(&owned, 43, None, None).expect("different PID"));
            assert!(activity_matches(&owned, 42, None, None).is_err());
            assert!(activity_matches(&owned, 42, Some(&owned.started), None).is_err());
        }

        #[test]
        fn exit_requires_identity_gone_and_exact_ledger_with_unchanged_server() {
            let owned = identity();
            assert!(
                !observation_complete(
                    &owned,
                    &owned.postmaster,
                    false,
                    std::slice::from_ref(&owned),
                    Some(0),
                    Some(0)
                )
                .expect("live backend")
            );
            assert!(
                !observation_complete(&owned, &owned.postmaster, false, &[], Some(1), Some(0))
                    .expect("leaked byte")
            );
            assert!(
                observation_complete(&owned, &owned.postmaster, false, &[], Some(0), Some(0))
                    .expect("clean exit")
            );
            assert!(observation_complete(&owned, "changed", false, &[], Some(0), Some(0)).is_err());
            assert!(
                observation_complete(&owned, &owned.postmaster, true, &[], Some(0), Some(0))
                    .is_err()
            );
            assert!(
                observation_complete(&owned, &owned.postmaster, false, &[], None, Some(0)).is_err()
            );
            assert!(
                observation_complete(&owned, &owned.postmaster, false, &[], None, None)
                    .expect("stock-only needs no ledger")
            );
        }

        #[test]
        fn failed_scope_latches_first_error_and_preserves_ledger_requirement() {
            let mut owned = scope();
            owned.state = State::LedgerActive;
            let mut registry = Registry {
                scope: Some(owned),
                failure: None,
            };
            assert!(snapshot_from(&registry, 8).is_err());
            record_failure(&mut registry, "query failed");
            record_failure(&mut registry, "subsequent cleanup failure");
            assert_eq!(registry.failure.as_deref(), Some("query failed"));
            assert_eq!(
                snapshot_from(&registry, 7).expect("registered").state,
                State::Failed {
                    ledger_active: true
                }
            );
            let mut state = State::IdentityOnly;
            state.fail();
            assert_eq!(
                state,
                State::Failed {
                    ledger_active: false
                }
            );
            assert!(activation_needed(State::IdentityOnly).expect("first activation"));
            assert!(
                !activation_needed(State::LedgerActive)
                    .expect("no second baseline with live workers")
            );
            assert!(activation_needed(state).is_err());
            assert!(
                activation_needed(State::Failed {
                    ledger_active: true
                })
                .is_err()
            );
        }

        #[test]
        fn persistent_failure_refuses_next_guard_without_resetting_baseline() {
            let mut registry = Registry::default();
            begin_scope(&mut registry, 1).expect("initial guard");
            record_failure(&mut registry, "owned worker did not exit");
            // Production Drop removes ownership after cleanup but retains the
            // failure. This isolated test never resets global state or a ledger.
            registry.scope = None;
            let error = begin_scope(&mut registry, 2).expect_err("next guard must refuse");
            assert!(error.contains("owned worker did not exit"));
            assert!(registry.scope.is_none());
            assert_eq!(
                registry.failure.as_deref(),
                Some("owned worker did not exit")
            );
        }

        #[test]
        fn cleanup_panic_is_an_error_and_original_assertion_unwind_survives() {
            struct Cleanup;
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let result = cleanup_safely(|| panic!("cleanup panic"));
                    assert!(result.is_err());
                }
            }
            let panic = std::panic::catch_unwind(|| {
                let _cleanup = Cleanup;
                panic!("primary assertion");
            })
            .expect_err("primary panic retained");
            assert_eq!(panic.downcast_ref::<&str>(), Some(&"primary assertion"));
        }

        #[test]
        fn registered_clients_drop_before_scope_on_normal_and_panic_paths() {
            use std::cell::RefCell;
            struct Record<'a>(&'a RefCell<Vec<&'static str>>, &'static str);
            impl Drop for Record<'_> {
                fn drop(&mut self) {
                    self.0.borrow_mut().push(self.1);
                }
            }
            for panic in [false, true] {
                let events = RefCell::new(Vec::new());
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _guard = Record(&events, "guard");
                    let _client = Record(&events, "client");
                    assert!(!panic, "controlled assertion");
                }));
                assert_eq!(*events.borrow(), ["client", "guard"]);
            }
        }

        #[test]
        fn connected_observer_stalled_query_obeys_deadline_and_drains_task() {
            use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime");
            let start = std::time::Instant::now();
            runtime.block_on(async {
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                    .await
                    .expect("numeric test endpoint");
                let address = listener.local_addr().expect("test endpoint");
                let queried = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                let server_queried = queried.clone();
                let server = tokio::spawn(async move {
                    let (mut stream, _) = listener.accept().await.expect("accept watchdog");
                    let size = stream.read_u32().await.expect("startup packet length");
                    assert!((8..=65536).contains(&size));
                    let mut startup = vec![0; usize::try_from(size - 4).expect("startup length")];
                    stream.read_exact(&mut startup).await.expect("startup body");
                    // AuthenticationOk + ReadyForQuery completes connection.
                    stream
                        .write_all(&[b'R', 0, 0, 0, 8, 0, 0, 0, 0, b'Z', 0, 0, 0, 5, b'I'])
                        .await
                        .expect("startup response");
                    let message = stream.read_u8().await.expect("post-connect query");
                    assert!(matches!(message, b'P' | b'Q'));
                    server_queried.store(true, Ordering::Release);
                    // Keep the socket alive but deliberately never answer SQL.
                    std::future::pending::<()>().await;
                    drop(stream);
                });
                let mut config = tokio_postgres::Config::new();
                config
                    .hostaddr(address.ip())
                    .port(address.port())
                    .user("test")
                    .dbname("test")
                    .ssl_mode(tokio_postgres::config::SslMode::Disable);
                let mut owned = scope();
                owned.identities.push(identity());
                owned.observer = Some(config);
                let deadline = tokio::time::Instant::now() + Duration::from_millis(250);
                let error = observe_async(&owned, &owned.identities, None, "stalled SQL", deadline)
                    .await
                    .expect_err("query deadline");
                assert!(
                    queried.load(Ordering::Acquire),
                    "must reach SQL after successful handshake: {error}"
                );
                assert!(error.contains("observer query deadline"), "{error}");
                server.abort();
                let _ = tokio::time::timeout_at(
                    tokio::time::Instant::now() + Duration::from_millis(100),
                    server,
                )
                .await
                .expect("mock server drains");
            });
            runtime.shutdown_timeout(Duration::ZERO);
            assert!(start.elapsed() < Duration::from_secs(2));
        }

        #[test]
        fn numeric_observer_stalled_handshake_obeys_overall_deadline() {
            // Kernel accepts TCP without an application reading the startup packet.
            // No DNS, server thread, or detached work is involved.
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("local listener");
            let address = listener.local_addr().expect("listener endpoint");
            let mut config = tokio_postgres::Config::new();
            config
                .hostaddr(address.ip())
                .port(address.port())
                .user("test")
                .dbname("test");
            let mut owned = scope();
            owned.identities.push(identity());
            owned.observer = Some(config);
            let start = std::time::Instant::now();
            let error = observe_with_timeout(
                &owned,
                &owned.identities,
                None,
                "stalled handshake",
                Duration::from_millis(30),
            )
            .expect_err("handshake must time out");
            assert!(error.contains("deadline"), "{error}");
            assert!(start.elapsed() < Duration::from_secs(2));
        }
    }
}

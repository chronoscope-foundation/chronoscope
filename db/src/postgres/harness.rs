//! Ephemeral Postgres+PostGIS cluster harness — test support for the backend.
//!
//! One process-shared cluster is `initdb`'d and started on a unix socket once
//! (behind a [`OnceCell`]), a migrated **template** database is built in it, and
//! every test clones that template with `CREATE DATABASE ... TEMPLATE` — far
//! cheaper than a per-test `initdb` + `CREATE EXTENSION postgis`. Each clone
//! backs a [`PostgresFactStore`] the conformance suite stamps against.
//!
//! **Teardown.** The cluster lives in a process static, which never runs `Drop`
//! (libtest exits via `process::exit`), and the postmaster daemonizes, so it
//! would otherwise outlive the test process. A detached `sh` **watchdog** polls
//! the parent pid and stops the cluster + removes its directories once the test
//! process exits. `libc::atexit` would be the tidier hook, but the workspace
//! denies `unsafe_code`; a poll-the-parent watchdog needs no unsafe and no new
//! dependency.
//!
//! The one non-obvious trap is the socket path length: a unix socket address is
//! capped at `sizeof(sockaddr_un.sun_path)` (104 bytes on macOS), and nix build
//! directories are deep, so the socket directory must be short — pulled out to a
//! short base (`PG_SOCKET_BASE`, default `/tmp`).

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::{Connection, PgConnection};
use tokio::sync::OnceCell;

use super::PostgresFactStore;

/// macOS caps `sockaddr_un.sun_path` at 104 bytes including the NUL terminator,
/// so the bind path must be at most 103 bytes. Linux allows 108; take the
/// tighter bound so the check is portable.
const SUN_PATH_LIMIT: usize = 104;

/// Postgres binds `<socket_dir>/.s.PGSQL.<port>`; the harness never overrides the
/// default port 5432, so the suffix length is fixed.
const SOCKET_SUFFIX: &str = "/.s.PGSQL.5432";

/// The migrated database every per-test database clones from.
const TEMPLATE_DB: &str = "cf_template";

/// Failures bringing up the shared cluster. Every arm carries the operation and,
/// for a failed subprocess, its captured output — a legible failure, not a bare
/// exit code.
#[derive(Debug, thiserror::Error)]
pub(super) enum PgHarnessError {
    #[error("failed to create the {what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to spawn `{tool}` (is it on PATH?): {source}")]
    Spawn {
        tool: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("`{tool}` exited with {status}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}")]
    Command {
        tool: &'static str,
        status: std::process::ExitStatus,
        stdout: String,
        stderr: String,
    },
    #[error(
        "socket path would be {len} bytes (dir {dir:?} + {SOCKET_SUFFIX}), over the \
         {SUN_PATH_LIMIT}-byte sockaddr_un limit — set PG_SOCKET_BASE to a shorter directory"
    )]
    SocketPathTooLong { dir: String, len: usize },
    #[error("sqlx failure while {context}: {source}")]
    Sqlx {
        context: &'static str,
        #[source]
        source: sqlx::Error,
    },
    #[error("migrating the template database: {source}")]
    Migrate {
        #[source]
        source: sqlx::migrate::MigrateError,
    },
}

/// Run a subprocess to completion, capturing its output and turning a non-zero
/// exit into a context-rich error.
fn run(tool: &'static str, cmd: &mut std::process::Command) -> Result<(), PgHarnessError> {
    let output = cmd
        .output()
        .map_err(|source| PgHarnessError::Spawn { tool, source })?;
    if output.status.success() {
        return Ok(());
    }
    Err(PgHarnessError::Command {
        tool,
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Start the postmaster in wait mode, sending both `pg_ctl`'s own output and the
/// server's to a log **file** rather than a captured pipe.
///
/// `pg_ctl start` daemonizes the postmaster, which inherits `pg_ctl`'s stdout and
/// stderr and holds them for its whole lifetime. A captured pipe would then never
/// reach EOF, so `Command::output()` would block forever even though `pg_ctl` has
/// already exited. Redirecting to a file makes the inherited descriptors
/// harmless, and `status()` waits only on `pg_ctl`.
fn start_postmaster(data_dir: &Path, server_opts: &str) -> Result<(), PgHarnessError> {
    let log_path = data_dir.join("pg_start.log");
    let log = std::fs::File::create(&log_path).map_err(|source| PgHarnessError::Io {
        what: "pg_ctl start log file",
        source,
    })?;
    let log_err = log.try_clone().map_err(|source| PgHarnessError::Io {
        what: "pg_ctl start log handle",
        source,
    })?;

    let status = std::process::Command::new("pg_ctl")
        .arg("-D")
        .arg(data_dir)
        .arg("-o")
        .arg(server_opts)
        .arg("-w")
        .arg("-t")
        .arg("60")
        .arg("start")
        .stdout(log)
        .stderr(log_err)
        .status()
        .map_err(|source| PgHarnessError::Spawn {
            tool: "pg_ctl start",
            source,
        })?;
    if status.success() {
        return Ok(());
    }
    Err(PgHarnessError::Command {
        tool: "pg_ctl start",
        status,
        stdout: String::new(),
        stderr: std::fs::read_to_string(&log_path).unwrap_or_default(),
    })
}

/// Spawn a detached watchdog that stops the cluster and removes its directories
/// once this process exits. It polls the parent pid — after the test process is
/// gone, one more loop iteration stops the postmaster (`-m immediate`) and
/// `rm -rf`s both directories, then exits.
fn spawn_watchdog(data_dir: &Path, socket_dir: &Path) -> Result<(), PgHarnessError> {
    // $1 parent pid, $2 data dir, $3 socket dir — all passed as positional args
    // so no path is interpolated into the script text.
    let script = "\
        parent=\"$1\"; data=\"$2\"; sock=\"$3\"; \
        while kill -0 \"$parent\" 2>/dev/null; do sleep 1; done; \
        pg_ctl -D \"$data\" -m immediate stop >/dev/null 2>&1; \
        rm -rf \"$data\" \"$sock\"";
    std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("sh")
        .arg(std::process::id().to_string())
        .arg(data_dir)
        .arg(socket_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|source| PgHarnessError::Spawn {
            tool: "cluster watchdog",
            source,
        })?;
    Ok(())
}

/// Connection options for `db` over the cluster's unix socket.
fn conn_opts(socket: &Path, db: &str) -> PgConnectOptions {
    PgConnectOptions::new()
        .socket(socket)
        .username("postgres")
        .database(db)
}

/// A single connection to `db` — the admin work (CREATE DATABASE, template
/// migration) runs on one of these rather than a pool, because a pool spawns a
/// background task bound to the runtime that built it, and every `#[tokio::test]`
/// brings its own runtime. A lone `PgConnection` has no such task, so it is safe
/// to open on the current runtime and drop.
async fn connect_one(socket: &Path, db: &str) -> Result<PgConnection, PgHarnessError> {
    PgConnection::connect_with(&conn_opts(socket, db))
        .await
        .map_err(|source| PgHarnessError::Sqlx {
            context: "connecting to the cluster",
            source,
        })
}

/// A pool to `db`. Only ever built for a single test's store, so it lives and
/// dies inside that test's runtime.
async fn connect_pool(socket: &Path, db: &str, max: u32) -> Result<PgPool, PgHarnessError> {
    PgPoolOptions::new()
        .max_connections(max)
        .connect_with(conn_opts(socket, db))
        .await
        .map_err(|source| PgHarnessError::Sqlx {
            context: "connecting to the cluster",
            source,
        })
}

/// A live single-node cluster shared across the whole test process. It holds
/// only OS-level, runtime-independent state — the unix socket path — because a
/// sqlx pool stored here would belong to whichever `#[tokio::test]` runtime
/// built it and be dead for every other test. The template database name is a
/// constant ([`TEMPLATE_DB`]).
struct SharedCluster {
    socket_path: std::path::PathBuf,
}

static SHARED: OnceCell<SharedCluster> = OnceCell::const_new();
static DB_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `initdb` a fresh cluster, start it on a unix socket, spawn the teardown
/// watchdog, then create and migrate the template database.
async fn build_shared_cluster() -> Result<SharedCluster, PgHarnessError> {
    let data_dir = tempfile::tempdir().map_err(|source| PgHarnessError::Io {
        what: "data tempdir",
        source,
    })?;

    // The socket dir must be short (see the sun_path limit); the base is
    // overridable because the default `/tmp` may not be the right writable short
    // path in every build environment.
    let socket_base = std::env::var_os("PG_SOCKET_BASE").map_or_else(
        || std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from,
    );
    let socket_dir = tempfile::Builder::new()
        .prefix("pgs")
        .tempdir_in(&socket_base)
        .map_err(|source| PgHarnessError::Io {
            what: "socket tempdir",
            source,
        })?;

    let sock_len = socket_dir.path().as_os_str().len() + SOCKET_SUFFIX.len();
    if sock_len >= SUN_PATH_LIMIT {
        return Err(PgHarnessError::SocketPathTooLong {
            dir: socket_dir.path().display().to_string(),
            len: sock_len,
        });
    }

    run(
        "initdb",
        std::process::Command::new("initdb")
            .arg("-D")
            .arg(data_dir.path())
            .arg("-U")
            .arg("postgres")
            .arg("--auth=trust")
            .arg("--no-sync")
            .arg("-E")
            .arg("UTF8"),
    )?;

    // pg_ctl runs the server via `/bin/sh -c`, so the -o string is shell parsed:
    // `''` becomes an empty listen_addresses (unix socket only). autovacuum=off
    // keeps background workers from connecting to the template during a clone
    // (`CREATE DATABASE ... TEMPLATE` refuses a source with other sessions); the
    // fsync/durability knobs are the standard ephemeral-cluster speedups.
    let server_opts = format!(
        "-k '{}' -c listen_addresses='' -c autovacuum=off -c fsync=off \
         -c synchronous_commit=off -c full_page_writes=off -c max_connections=500",
        socket_dir.path().display()
    );
    start_postmaster(data_dir.path(), &server_opts)?;
    spawn_watchdog(data_dir.path(), socket_dir.path())?;

    // Hand both temp dirs to the watchdog the instant it is up: `keep()` consumes
    // each `TempDir` so its `Drop` no longer `rm -rf`s the directory. Any failure
    // below (CREATE DATABASE, migrate) then leaves the watchdog as sole owner of a
    // still-running postmaster and its PGDATA, stopping the one before removing the
    // other at process exit.
    let socket_path = socket_dir.keep();
    let _data_path = data_dir.keep();

    // Build the template on single connections (no pool): create the database,
    // migrate it, then close both connections so the template has no live
    // sessions when a test clones it.
    let mut admin = connect_one(&socket_path, "postgres").await?;
    sqlx::query(&format!("CREATE DATABASE \"{TEMPLATE_DB}\""))
        .execute(&mut admin)
        .await
        .map_err(|source| PgHarnessError::Sqlx {
            context: "creating the template database",
            source,
        })?;
    close(admin).await;

    let mut template = connect_one(&socket_path, TEMPLATE_DB).await?;
    sqlx::migrate!("./migrations-postgres/facts")
        .run(&mut template)
        .await
        .map_err(|source| PgHarnessError::Migrate { source })?;
    close(template).await;

    Ok(SharedCluster { socket_path })
}

/// Best-effort close of a single connection — a failed close only leaks a
/// connection the cluster teardown reclaims, so it is not worth surfacing.
async fn close(conn: PgConnection) {
    let _ = conn.close().await;
}

/// The process-shared cluster, built on first use.
async fn shared_cluster() -> Result<&'static SharedCluster, PgHarnessError> {
    SHARED.get_or_try_init(build_shared_cluster).await
}

/// A fresh, empty [`PostgresFactStore`] over a clone of the migrated template —
/// the builder the conformance suite stamps against. The returned `()` is the
/// suite's held-alive context slot; the store owns its pool, and the per-test
/// database is reclaimed when the whole cluster tears down at process exit.
pub(crate) async fn fresh_pg_store() -> Result<(PostgresFactStore, ()), Box<dyn std::error::Error>>
{
    fresh_pg_store_with(None).await
}

/// Like [`fresh_pg_store`], but the per-test database carries a session default
/// `default_transaction_isolation` of `iso`, so every connection the pool opens
/// begins its transactions there unless a statement overrides them. Lets a test
/// prove the write path pins its own isolation rather than riding the default.
pub(crate) async fn fresh_pg_store_at_default_isolation(
    iso: &str,
) -> Result<(PostgresFactStore, ()), Box<dyn std::error::Error>> {
    fresh_pg_store_with(Some(iso)).await
}

/// Clone the migrated template into a fresh per-test database, optionally pin its
/// session-default isolation, then build a store over a pool to the clone.
async fn fresh_pg_store_with(
    default_isolation: Option<&str>,
) -> Result<(PostgresFactStore, ()), Box<dyn std::error::Error>> {
    let cluster = shared_cluster().await?;
    let n = DB_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dbname = format!("cf_test_{n}");
    // A short-lived admin connection on the *current* runtime clones the
    // template; a pool stored in the shared cluster would belong to the runtime
    // that built it and be dead here.
    let mut admin = connect_one(&cluster.socket_path, "postgres").await?;
    sqlx::query(&format!(
        "CREATE DATABASE \"{dbname}\" TEMPLATE \"{TEMPLATE_DB}\""
    ))
    .execute(&mut admin)
    .await
    .map_err(|source| PgHarnessError::Sqlx {
        context: "cloning the template database",
        source,
    })?;
    // `ALTER DATABASE ... SET` bakes the default into the clone so every new
    // session inherits it — the startup `options` packet can't carry it, because
    // a value like `repeatable read` has a space the `-c` parser splits on.
    if let Some(iso) = default_isolation {
        sqlx::query(&format!(
            "ALTER DATABASE \"{dbname}\" SET default_transaction_isolation = '{iso}'"
        ))
        .execute(&mut admin)
        .await
        .map_err(|source| PgHarnessError::Sqlx {
            context: "setting the per-test database isolation default",
            source,
        })?;
    }
    close(admin).await;
    let pool = connect_pool(&cluster.socket_path, &dbname, 5).await?;
    Ok((PostgresFactStore::new(pool), ()))
}

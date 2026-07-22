//! Ephemeral Postgres+PostGIS cluster harness — an infra de-risking spike.
//!
//! This exists to answer one question: can an ephemeral `postgres` cluster be
//! `initdb`'d, started over a unix socket, and reached with `sqlx` from inside
//! the hermetic `nix flake check` build environment on darwin? It is **not**
//! the Postgres backend — no `FactStore` impl, no schema, just enough to prove
//! the cluster comes up and PostGIS loads. Test-only for now; we promote the
//! harness to shared test-support once a real backend needs it.
//!
//! The one non-obvious trap is the socket path length: a unix socket address is
//! capped at `sizeof(sockaddr_un.sun_path)` (104 bytes on macOS), and nix build
//! directories are deep, so the socket directory must be short. The data
//! directory has no such limit, so only the socket dir is pulled out to a short
//! base (`PG_SOCKET_BASE`, default `/tmp`).

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};

/// macOS caps `sockaddr_un.sun_path` at 104 bytes including the NUL terminator,
/// so the bind path must be at most 103 bytes. Linux allows 108; take the
/// tighter bound so the check is portable.
const SUN_PATH_LIMIT: usize = 104;

/// Postgres binds `<socket_dir>/.s.PGSQL.<port>`; the harness never overrides
/// the default port 5432, so the suffix length is fixed.
const SOCKET_SUFFIX: &str = "/.s.PGSQL.5432";

/// Failures bringing up or tearing down the ephemeral cluster. Every arm
/// carries the operation and, for a failed subprocess, its captured output —
/// the whole point of a spike is a legible failure, not a bare exit code.
#[derive(Debug, thiserror::Error)]
enum PgHarnessError {
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
}

/// A live single-node cluster: its data + socket directories (removed on drop)
/// and a pool connected over the unix socket.
struct PgCluster {
    // Field order is drop order. `Drop::drop` stops the server while both
    // TempDirs are still live, then these fields drop — removing the
    // directories — in the order written here.
    data_dir: tempfile::TempDir,
    #[expect(
        dead_code,
        reason = "held for RAII: its TempDir Drop removes the socket directory when the cluster tears down"
    )]
    socket_dir: tempfile::TempDir,
    pool: PgPool,
}

/// Run a subprocess to completion, capturing its output and turning a non-zero
/// exit into a context-rich error. `cmd` is passed already-configured so the
/// caller reads as a flat argument list.
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

/// Start the postmaster in wait mode, sending both pg_ctl's own output and the
/// server's to a log **file** rather than a captured pipe.
///
/// `pg_ctl start` daemonizes the postmaster, which inherits pg_ctl's stdout and
/// stderr and holds them for its whole lifetime. A captured pipe would then
/// never reach EOF, so `Command::output()` would block forever even though
/// pg_ctl itself has already exited. Redirecting to a file makes the inherited
/// descriptors harmless, and `status()` waits only on pg_ctl.
fn start_postmaster(data_dir: &std::path::Path, server_opts: &str) -> Result<(), PgHarnessError> {
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

impl PgCluster {
    /// `initdb` a fresh cluster in a tempdir, start it listening on a unix
    /// socket only (no TCP), and hand back a connected pool.
    async fn start() -> Result<Self, PgHarnessError> {
        let data_dir = tempfile::tempdir().map_err(|source| PgHarnessError::Io {
            what: "data tempdir",
            source,
        })?;

        // The socket dir must be short (see the sun_path limit); the base is
        // overridable because the default `/tmp` may not be the right writable
        // short path in every build environment.
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

        // Guard the sun_path limit before postgres does, so the failure names
        // the cause instead of surfacing as an opaque bind error deep in startup.
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

        // pg_ctl runs the server via `/bin/sh -c`, so the -o string is shell
        // parsed: `''` becomes an empty listen_addresses (unix socket only) and
        // the quoted socket dir tolerates any path chars.
        let server_opts = format!(
            "-k '{}' -c listen_addresses=''",
            socket_dir.path().display()
        );
        start_postmaster(data_dir.path(), &server_opts)?;

        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect_with(
                PgConnectOptions::new()
                    .socket(socket_dir.path())
                    .username("postgres")
                    .database("postgres"),
            )
            .await
            .map_err(|source| PgHarnessError::Sqlx {
                context: "connecting to the ephemeral cluster",
                source,
            })?;

        Ok(Self {
            data_dir,
            socket_dir,
            pool,
        })
    }
}

impl Drop for PgCluster {
    fn drop(&mut self) {
        // Best-effort teardown. A failure here leaks an ephemeral cluster the
        // OS reclaims anyway; panicking in Drop would poison unrelated test
        // teardown, so report and move on.
        match std::process::Command::new("pg_ctl")
            .arg("-D")
            .arg(self.data_dir.path())
            .arg("-m")
            .arg("immediate")
            .arg("stop")
            .output()
        {
            Ok(output) if !output.status.success() => {
                eprintln!(
                    "PgCluster teardown: pg_ctl stop exited with {}:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Ok(_) => {}
            Err(e) => eprintln!("PgCluster teardown: could not spawn pg_ctl stop: {e}"),
        }
    }
}

/// The spike's whole answer: bring up the cluster, load PostGIS, and prove the
/// extension is live by reading its version and a WKT round-trip.
#[tokio::test]
async fn ephemeral_cluster_loads_postgis_over_unix_socket() -> Result<(), PgHarnessError> {
    let cluster = PgCluster::start().await?;

    sqlx::query("CREATE EXTENSION IF NOT EXISTS postgis")
        .execute(&cluster.pool)
        .await
        .map_err(|source| PgHarnessError::Sqlx {
            context: "creating the postgis extension",
            source,
        })?;

    let (lib_version,): (String,) = sqlx::query_as("SELECT postgis_lib_version()")
        .fetch_one(&cluster.pool)
        .await
        .map_err(|source| PgHarnessError::Sqlx {
            context: "reading postgis_lib_version()",
            source,
        })?;
    assert!(
        !lib_version.trim().is_empty(),
        "postgis_lib_version() returned empty"
    );

    let (wkt,): (String,) = sqlx::query_as("SELECT ST_AsText(ST_MakePoint(1, 2))")
        .fetch_one(&cluster.pool)
        .await
        .map_err(|source| PgHarnessError::Sqlx {
            context: "reading ST_AsText(ST_MakePoint(1, 2))",
            source,
        })?;
    assert_eq!(wkt, "POINT(1 2)");

    Ok(())
}

use lvtd_dev as app;

use app_core::jobs::Role;
use app_core::{Config, db};
use std::net::SocketAddr;
use tokio::net::TcpListener;

fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();

    #[cfg(feature = "sentry")]
    let sentry_config = app::sentry::Config::from_env()?;
    #[cfg(feature = "sentry-server")]
    let _sentry_guard = sentry_config.as_ref().map(|config| sentry::init(config.client_options()));
    tokio::runtime::Builder::new_multi_thread().enable_all().build()?.block_on(start(
        #[cfg(feature = "sentry")]
        sentry_config,
    ))
}

async fn start(#[cfg(feature = "sentry")] sentry_config: Option<app::sentry::Config>) -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    #[cfg(feature = "posthog")]
    let analytics = app::analytics::Config::from_env()?;
    #[cfg(any(feature = "posthog", feature = "sentry-server"))]
    let telemetry = app::observability::Telemetry::init(
        #[cfg(feature = "posthog")]
        analytics.clone(),
    )
    .await?;
    #[cfg(feature = "posthog-server")]
    if let Some(config) = &analytics {
        posthog_rs::init_global(config.client_options()?).await?;
    }
    #[cfg(not(any(feature = "posthog", feature = "sentry-server")))]
    app_core::telemetry::init(&format!(
        "{}=info,{}=info,{}=info",
        env!("CARGO_PKG_NAME").replace('-', "_"),
        app_core::LOG_TARGET,
        app_auth::LOG_TARGET
    ));
    let result = run(
        cfg,
        #[cfg(feature = "posthog")]
        analytics.clone(),
        #[cfg(feature = "sentry")]
        sentry_config,
    )
    .await;
    if let Err(error) = &result {
        tracing::error!(%error, "application stopped with an error");
        #[cfg(feature = "sentry-errors")]
        sentry::integrations::anyhow::capture_anyhow(error);
    }
    // shutdown flushes the bounded SDK queue, even if serving returned an error.
    #[cfg(feature = "posthog-server")]
    posthog_rs::shutdown().await;
    #[cfg(any(feature = "posthog", feature = "sentry-server"))]
    telemetry.shutdown().await;
    result
}

#[cfg(feature = "auth")]
const USAGE: &str =
    "commands: (none) serve | worker | migrate | admin <grant|revoke> <email> | jobs stats | jobs retry <id>";
#[cfg(not(feature = "auth"))]
const USAGE: &str = "commands: (none) serve | worker | migrate | jobs stats | jobs retry <id>";

async fn run(
    cfg: Config,
    #[cfg(feature = "posthog")] analytics: Option<app::analytics::Config>,
    #[cfg(feature = "sentry")] sentry_config: Option<app::sentry::Config>,
) -> anyhow::Result<()> {
    let mut jobs_config = app_core::jobs::Config::from_env()?;
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("migrate") => {
            db::connect(&cfg.database_url, &app::MIGRATOR).await?;
            println!("migrations applied");
            return Ok(());
        }
        #[cfg(feature = "auth")]
        Some("admin") => {
            let enabled = match args.next().as_deref() {
                Some("grant") => true,
                Some("revoke") => false,
                _ => anyhow::bail!("usage: admin <grant|revoke> <email>"),
            };
            let email = args.next().ok_or_else(|| anyhow::anyhow!("usage: admin <grant|revoke> <email>"))?;
            anyhow::ensure!(args.next().is_none(), "usage: admin <grant|revoke> <email>");
            let pool = db::connect(&cfg.database_url, &app::MIGRATOR).await?;
            app::admin::set_admin(&pool, &email, enabled).await?;
            println!("Admin access {}.", if enabled { "granted" } else { "revoked" });
            return Ok(());
        }
        Some("jobs") => {
            let pool = db::connect(&cfg.database_url, &app::MIGRATOR).await?;
            return jobs_command(&pool, args).await;
        }
        // Same binary, worker role: claims jobs and answers /healthz, serves nothing else.
        Some("worker") => jobs_config.role = Role::Worker,
        Some(other) => anyhow::bail!("unknown command {other:?}; {USAGE}"),
        None => {}
    }
    anyhow::ensure!(args.next().is_none(), "unexpected argument; {USAGE}");

    let pool = db::connect(&cfg.database_url, &app::MIGRATOR).await?;
    #[cfg(feature = "stripe")]
    let stripe = if let Some(config) = app::billing::Config::from_env()? {
        let client = app::billing::StripeClient::new(config)?;
        client.validate().await?;
        Some(client)
    } else {
        None
    };
    #[cfg(feature = "storage")]
    let storage = app_core::storage::Storage::from_env()?;
    #[cfg(feature = "storage")]
    tracing::info!("uploads go to {}", storage.describe());
    #[cfg(feature = "qdrant")]
    let vectors = if let Some(config) = app::vectors::Config::from_env()? {
        let client = app::vectors::Client::new(config)?;
        client.ensure_collection().await?;
        Some(client)
    } else {
        None
    };
    #[cfg_attr(not(feature = "auth"), allow(unused_mut))]
    let (mut state, app) = app::build_configured(
        cfg,
        pool,
        #[cfg(feature = "posthog")]
        analytics,
        #[cfg(feature = "sentry")]
        sentry_config,
        #[cfg(feature = "stripe")]
        stripe,
        #[cfg(feature = "storage")]
        storage,
        #[cfg(feature = "qdrant")]
        vectors,
    );
    drop(app);
    #[cfg(feature = "auth")]
    {
        let email = app::verification::Client::from_env(&state.cfg.site_url)?;
        std::sync::Arc::get_mut(&mut state).expect("unique state").email = Some(email);
    }
    #[cfg(feature = "social-login")]
    {
        let social_login = app::social_login::Providers::from_env(&state.cfg.site_url)?;
        std::sync::Arc::get_mut(&mut state).expect("unique state").social_login = social_login;
    }
    let role = jobs_config.role;

    // Background work follows the worker role: a web-only process (JOBS_WORKER=false)
    // leaves jobs and periodic cleanup to the worker process.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let worker = role.runs_worker().then(|| {
        let worker =
            app_core::jobs::Worker::new(state.pool.clone(), state.clone(), app::jobs::registry(), jobs_config.clone());
        tokio::spawn(worker.run(shutdown_rx))
    });
    #[cfg(feature = "auth")]
    let cleanups = role.runs_worker().then(|| {
        let tasks = vec![
            tokio::spawn(app_auth::sessions::cleanup_expired(state.pool.clone())),
            tokio::spawn(app::verification::cleanup(state.pool.clone())),
        ];
        #[cfg(feature = "api")]
        let tasks = {
            let mut tasks = tasks;
            tasks.push(tokio::spawn(app_auth::oauth_server::cleanup_expired(state.pool.clone())));
            tasks
        };
        tasks
    });

    let (app, indexnow) = if role.serves_http() {
        let app = app::routes::router(state.clone());
        app_core::indexnow::attach(
            app,
            state.pool.clone(),
            &state.cfg.site_url,
            app::indexnow_paths(&state),
            env!("INDEXNOW_REVISION"),
            app_core::indexnow::Config::from_env()?,
        )
        .await?
    } else {
        (app::worker_router(), None)
    };
    let addr: SocketAddr = state.cfg.bind_addr.parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!(?role, "listening on http://{addr} (site_url={})", state.cfg.site_url);
    let result = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await;
    // Stop claiming jobs and drain running ones; see app_core::jobs::Config::shutdown.
    let _ = shutdown_tx.send(true);
    if let Some(worker) = worker {
        let _ = worker.await;
    }
    if let Some(task) = indexnow {
        task.abort();
    }
    #[cfg(feature = "auth")]
    if let Some(cleanups) = cleanups {
        for task in &cleanups {
            task.abort();
        }
        for task in cleanups {
            let _ = task.await;
        }
    }
    result?;
    Ok(())
}

/// Operator commands for the job queue: `jobs stats`, `jobs retry <id>`.
async fn jobs_command(pool: &app_core::Pool, mut args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    match args.next().as_deref() {
        Some("stats") => {
            let rows = app_core::jobs::stats(pool).await?;
            if rows.is_empty() {
                println!("no jobs");
            }
            for row in rows {
                let oldest = row.oldest_run_at.map(|t| t.to_string()).unwrap_or_default();
                println!("{:<8} {:<32} {:>8}  {oldest}", row.state, row.kind, row.count);
            }
            Ok(())
        }
        Some("retry") => {
            let id = args.next().ok_or_else(|| anyhow::anyhow!("usage: jobs retry <id>"))?;
            let id: uuid::Uuid = id.parse().map_err(|_| anyhow::anyhow!("not a job id: {id}"))?;
            if app_core::jobs::retry_dead(pool, id).await? {
                println!("job {id} queued again");
            } else {
                anyhow::bail!("job {id} is not a dead job");
            }
            Ok(())
        }
        _ => anyhow::bail!("usage: jobs stats | jobs retry <id>"),
    }
}

async fn shutdown_signal() {
    let ctrl_c = async { tokio::signal::ctrl_c().await.ok() };
    #[cfg(unix)]
    let terminate =
        async { tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap().recv().await };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<Option<()>>();
    tokio::select! { _ = ctrl_c => {}, _ = terminate => {} }
    tracing::info!("shutting down");
}

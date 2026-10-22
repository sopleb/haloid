//! Starting the server: bootstrap configuration from the environment, the
//! database checked and migrated, the service assembled, and the HTTP
//! server run until it is told to stop. A library function, so the Pro
//! build's binary can call it with what it adds.

use crate::config::Config;
use crate::router::{Frontend, app_router_with};
use anyhow::Context;
use haloid_media::{MediaConfig, MediaToolkit};
use haloid_release::Edition;
use haloid_service::app::{App, AppConfig, ProConfig};
use haloid_service::auth::human::AltchaConfig;
use haloid_service::settings::{RELOAD_PERIOD, env_from_process};
use haloid_storage::backend::StorageBackend;
use rand::Rng;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

const USAGE: &str = "\
Usage: haloid-server [command]

With no command, serve. Configuration comes from the environment
(docs/deploy.md); the commands below use the same DATABASE_URL.

Commands:
  admin list                 Show who is an administrator
  admin promote <user>       Make a user (by username or email) an administrator,
                             and lift any ban or lockout
  admin password <user>      Set a user's password, read from the terminal or
                             standard input, and sign them out everywhere
  instance                   Print this instance's id, for a licence key
  import --from <mysql url>  Bring in a Chevereto v4 database (see docs/deploy.md)
         [--prefix chv_] [--files-dir <path>] [--files-url <url>] [--dry-run]
  rollback                   Go back to the binary an update replaced (the
                             database is not rolled back)
  version                    Print the version, edition and platform
  help                       This
";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Command {
    Serve,
    AdminList,
    AdminPromote(String),
    AdminPassword(String),
    Instance,
    Import(ImportArgs),
    Version,
    Rollback,
    Help,
}

#[derive(Debug, PartialEq, Eq, Default)]
struct ImportArgs {
    from: String,
    prefix: Option<String>,
    files_dir: Option<String>,
    files_url: Option<String>,
    dry_run: bool,
}

fn parse_import(args: &[&str]) -> Result<ImportArgs, String> {
    let mut out = ImportArgs::default();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let mut value = || {
            rest.next()
                .map(|v| (*v).to_string())
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match *arg {
            "--from" => out.from = value()?,
            "--prefix" => out.prefix = Some(value()?),
            "--files-dir" => out.files_dir = Some(value()?),
            "--files-url" => out.files_url = Some(value()?),
            "--dry-run" => out.dry_run = true,
            other => return Err(format!("import does not take {other}")),
        }
    }
    if out.from.is_empty() {
        return Err("import needs --from <the Chevereto database's mysql:// URL>".into());
    }
    Ok(out)
}

fn parse(args: &[String]) -> Result<Command, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    Ok(match args.as_slice() {
        [] | ["serve"] => Command::Serve,
        ["admin", "list"] => Command::AdminList,
        ["admin", "promote", who] => Command::AdminPromote((*who).to_string()),
        ["admin", "password", who] => Command::AdminPassword((*who).to_string()),
        ["instance"] => Command::Instance,
        ["import", rest @ ..] => {
            Command::Import(parse_import(rest).map_err(|e| format!("{e}\n\n{USAGE}"))?)
        }
        ["version" | "--version" | "-V"] => Command::Version,
        ["rollback"] => Command::Rollback,
        ["help" | "-h" | "--help"] => Command::Help,
        _ => return Err(format!("unknown command: {}\n\n{USAGE}", args.join(" "))),
    })
}

/// What a binary is: its edition, and the kinds of storage it adds beyond
/// local (none in the Community build; the Pro build passes its own).
pub struct Build {
    pub edition: Edition,
    pub storage_backends: Vec<Arc<dyn StorageBackend>>,
}

impl Build {
    pub fn community() -> Self {
        Self {
            edition: Edition::Community,
            storage_backends: Vec::new(),
        }
    }

    pub fn pro(storage_backends: Vec<Arc<dyn StorageBackend>>) -> Self {
        Self {
            edition: Edition::Pro,
            storage_backends,
        }
    }
}

/// Run what the command line asks for: by default the server, until it is
/// told to stop.
pub async fn run(build: Build) -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = parse(&args).map_err(anyhow::Error::msg)?;
    // Found now: once an update has renamed a new binary over this one,
    // /proc/self/exe names the old, deleted file.
    let exe = std::env::current_exe().ok();
    match command {
        Command::Help => {
            print!("{USAGE}");
            return Ok(());
        }
        // The updater runs a new binary with this before swapping it in,
        // and expects exactly this shape.
        Command::Version => {
            println!(
                "haloid-server {} {} {}",
                env!("CARGO_PKG_VERSION"),
                build.edition.as_str(),
                haloid_release::BUILD_TARGET
            );
            return Ok(());
        }
        Command::Rollback => {
            let exe = exe.context("the running binary could not be found")?;
            haloid_service::updates::roll_back(&exe).map_err(anyhow::Error::msg)?;
            println!(
                "{} is the previous binary again. Restart the service to run it.",
                exe.display()
            );
            return Ok(());
        }
        _ => {}
    }
    let storage_backends = build.storage_backends;
    let config = Config::from_env(|key| std::env::var(key).ok()).map_err(anyhow::Error::msg)?;
    if command != Command::Serve {
        return rescue(&config, command).await;
    }
    init_tracing(&config);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "haloid starting");

    let db = haloid_db::connect(&config.database_url, 20)
        .await
        .context("connecting to the database")?;
    haloid_db::preflight(&db).await?;
    haloid_db::migrate(&db)
        .await
        .context("applying migrations")?;

    let altcha_secret = config.altcha_secret.clone().unwrap_or_else(|| {
        tracing::warn!(
            "HALOID_ALTCHA_SECRET is not set; using a random one. Challenges \
             issued before a restart will fail after it."
        );
        hex::encode(rand::rng().random::<[u8; 32]>())
    });
    let altcha = config.altcha_cost.map(|cost| {
        tracing::warn!(
            cost,
            "HALOID_ALTCHA_COST lowers the proof-of-work; for tests only"
        );
        AltchaConfig {
            cost,
            ..AltchaConfig::with_secret(altcha_secret.clone())
        }
    });
    let media = Arc::new(MediaToolkit::new(MediaConfig::default()));
    let app = Arc::new(
        App::start(
            db,
            media,
            AppConfig {
                data_dir: config.data_dir.clone(),
                altcha_secret,
                pinned_settings: env_from_process(),
                pro: ProConfig {
                    storage_backends,
                    licence_key: std::env::var("HALOID_LICENCE_KEY")
                        .ok()
                        .filter(|k| !k.trim().is_empty()),
                    ..ProConfig::default()
                },
                altcha,
                updates: Some(updates_config(build.edition, exe.clone())),
            },
        )
        .await
        .context("assembling the service")?,
    );
    tracing::info!(instance = app.licensing.instance_id(), "instance");
    // Online licence checks, only when pointed at a licence service: once at
    // start, then daily. Offline checking needs nothing.
    let _online = std::env::var("HALOID_LICENCE_SERVER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|server| {
            tracing::info!(%server, "checking the licence with the licence service daily");
            app.licensing
                .spawn_online_checks(server, std::time::Duration::from_secs(24 * 3600))
        });
    let _reload = app.settings.spawn_reload_ticker(RELOAD_PERIOD);
    let restart = app.updates.restart_requested();
    let _updates = app.updates.spawn();
    if let Some(token) = std::env::var("HALOID_METRICS_TOKEN")
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        app.metrics.set_token(token.trim().to_string());
    }
    let (stop_jobs, jobs_stopped) = tokio::sync::watch::channel(false);
    let jobs = if config.disable_jobs {
        tracing::info!("background jobs disabled by HALOID_DISABLE_JOBS");
        None
    } else {
        Some(haloid_jobs::Runner::new(app.clone()).spawn(jobs_stopped))
    };

    // A built frontend on disk wins, so a developer's `pnpm build` is what
    // they see; otherwise the copy compiled in, if this binary has one.
    let on_disk = config.frontend_dir.join("index.html").exists();
    let frontend = if on_disk {
        tracing::info!(dir = %config.frontend_dir.display(), "serving the frontend from disk");
        Frontend::Dir(config.frontend_dir.as_path())
    } else if crate::router::has_embedded_frontend() {
        tracing::info!("serving the frontend compiled into this binary");
        Frontend::Embedded
    } else {
        tracing::warn!(
            dir = %config.frontend_dir.display(),
            "no built frontend there and none compiled in; serving the API only (run `pnpm build` in frontend/)"
        );
        Frontend::None
    };

    if !config.trusted_proxies.is_empty() {
        tracing::info!(proxies = ?config.trusted_proxies, "taking client addresses from X-Forwarded-For");
    }
    let trusted = std::sync::Arc::new(config.trusted_proxies.clone());
    let listener = tokio::net::TcpListener::bind(config.bind)
        .await
        .with_context(|| format!("binding {}", config.bind))?;
    tracing::info!(address = %config.bind, "listening");
    axum::serve(
        listener,
        app_router_with(app, frontend)
            .layer(axum::middleware::from_fn(move |request, next| {
                crate::proxy::real_client(trusted.clone(), request, next)
            }))
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown(restart.clone()))
    .await?;
    // Let a running job finish its step rather than cutting it off; what
    // it had not started stays queued for the next start.
    let _ = stop_jobs.send(true);
    if let Some(jobs) = jobs {
        let _ = jobs.await;
    }
    if *restart.borrow() {
        return restart_into(exe, &args);
    }
    tracing::info!("stopped");
    Ok(())
}

/// How this binary updates itself: in place, or in a container through the
/// updater sidecar, if one is configured.
fn updates_config(
    edition: Edition,
    exe: Option<std::path::PathBuf>,
) -> haloid_service::updates::UpdatesConfig {
    use haloid_service::updates::{GITHUB_API, Installer, Source, UpdatesConfig};
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    // HALOID_IN_CONTAINER says so either way (`0` for a binary that runs in
    // a container but owns its own file); otherwise Docker's marker file.
    let in_container = match var("HALOID_IN_CONTAINER").as_deref() {
        Some("0" | "false") => false,
        Some(_) => true,
        None => std::path::Path::new("/.dockerenv").exists(),
    };
    let installer = match (
        in_container,
        var("HALOID_UPDATER_URL"),
        var("HALOID_UPDATER_TOKEN"),
    ) {
        (false, _, _) => Installer::Binary { exe },
        (true, Some(url), Some(token)) => Installer::Sidecar { url, token },
        (true, _, _) => Installer::Container,
    };
    let source = match edition {
        Edition::Community => Source::Github {
            api: var("HALOID_RELEASES_URL").unwrap_or_else(|| GITHUB_API.into()),
        },
        Edition::Pro => Source::LicenceService {
            server: var("HALOID_LICENCE_SERVER"),
        },
    };
    UpdatesConfig {
        edition,
        target: haloid_release::BUILD_TARGET.into(),
        current: env!("CARGO_PKG_VERSION").into(),
        installer,
        source,
        trusted: haloid_release::RELEASE_KEYS.to_vec(),
        disabled: None,
    }
}

/// Become the new binary, in this process, with the same arguments and
/// environment. Only returns if that fails.
fn restart_into(exe: Option<std::path::PathBuf>, args: &[String]) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = exe.context("the running binary could not be found")?;
    tracing::info!(exe = %exe.display(), "restarting into the updated binary");
    let error = std::process::Command::new(&exe).args(args).exec();
    Err(anyhow::Error::new(error).context(format!("starting {}", exe.display())))
}

/// The operator's commands: no server, no tracing noise, plain output.
async fn rescue(config: &Config, command: Command) -> anyhow::Result<()> {
    use haloid_service::rescue;
    let db = haloid_db::connect(&config.database_url, 2)
        .await
        .context("connecting to the database")?;
    haloid_db::migrate(&db)
        .await
        .context("applying migrations")?;
    match command {
        Command::AdminList => {
            let admins = rescue::admins(&db).await?;
            if admins.is_empty() {
                println!("No administrators. Make one with: haloid-server admin promote <user>");
            }
            for user in admins {
                println!(
                    "{}\t{}\t{}",
                    user.username,
                    user.email.as_deref().unwrap_or("-"),
                    user.status.as_str()
                );
            }
        }
        Command::AdminPromote(who) => {
            let user = rescue::promote(&db, &who).await?;
            println!("{} is now an administrator.", user.username);
        }
        Command::AdminPassword(who) => {
            let password = read_password(&who)?;
            let user = rescue::set_password(&db, &who, &password).await?;
            println!(
                "Password set for {}; they are signed out everywhere.",
                user.username
            );
        }
        Command::Instance => {
            // Only read: the id is made by the server at its first start.
            match haloid_db::instance::get(&db).await? {
                Some(row) => println!("{}", row.instance_id),
                None => {
                    anyhow::bail!("this instance has not started yet; run the server once first")
                }
            }
        }
        Command::Import(args) => {
            let source = sqlx::mysql::MySqlPoolOptions::new()
                .max_connections(2)
                .connect(&args.from)
                .await
                .context("connecting to the Chevereto database")?;
            let options = haloid_import::Options {
                prefix: args.prefix.unwrap_or_else(|| "chv_".into()),
                files_dir: args.files_dir.map(Into::into),
                files_url: args.files_url,
                dry_run: args.dry_run,
            };
            let report = haloid_import::run(&source, &db, &options).await?;
            println!(
                "{} {} users, {} albums, {} files, {} tags, {} likes, {} categories, {} storages{}",
                if args.dry_run {
                    "Would import"
                } else {
                    "Imported"
                },
                report.users,
                report.albums,
                report.files,
                report.tags,
                report.likes,
                report.categories,
                report.storages,
                if report.already > 0 {
                    format!("; {} were already here", report.already)
                } else {
                    String::new()
                }
            );
            for skipped in &report.skipped {
                println!("skipped: {skipped}");
            }
            for todo in &report.todo {
                println!("to do: {todo}");
            }
        }
        Command::Serve | Command::Help | Command::Version | Command::Rollback => {
            unreachable!("handled by run")
        }
    }
    Ok(())
}

/// From the terminal without echo, or a line of standard input when it is
/// piped.
fn read_password(who: &str) -> anyhow::Result<String> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        let first = rpassword::prompt_password(format!("New password for {who}: "))?;
        let again = rpassword::prompt_password("Again: ")?;
        if first != again {
            anyhow::bail!("the two passwords differ; nothing changed");
        }
        Ok(first)
    } else {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        Ok(line.trim_end_matches(['\r', '\n']).to_string())
    }
}

fn init_tracing(config: &Config) {
    let filter = EnvFilter::try_new(&config.log_filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if config.log_json {
        builder.json().init();
    } else {
        builder.init();
    }
}

/// Ctrl-C, or SIGTERM from systemd or a container runtime. In-flight
/// requests finish; new connections are refused.
async fn shutdown(mut restart: tokio::sync::watch::Receiver<bool>) {
    let updated = async move {
        let _ = restart.wait_for(|r| *r).await;
    };
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
        () = updated => {}
    }
    tracing::info!("shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn the_command_line_is_read() {
        assert_eq!(parse(&args("")), Ok(Command::Serve));
        assert_eq!(parse(&args("admin list")), Ok(Command::AdminList));
        assert_eq!(
            parse(&args("admin promote mira")),
            Ok(Command::AdminPromote("mira".into()))
        );
        assert_eq!(
            parse(&args("admin password mira@example.com")),
            Ok(Command::AdminPassword("mira@example.com".into()))
        );
        assert_eq!(parse(&args("instance")), Ok(Command::Instance));
        assert_eq!(parse(&args("--help")), Ok(Command::Help));
        assert_eq!(parse(&args("--version")), Ok(Command::Version));
        assert_eq!(parse(&args("version")), Ok(Command::Version));
        assert_eq!(parse(&args("rollback")), Ok(Command::Rollback));
        assert!(parse(&args("admin promote")).is_err());
        assert!(parse(&args("frobnicate")).is_err());
        assert_eq!(
            parse(&args(
                "import --from mysql://u:p@h/chv --files-dir /srv/images --dry-run"
            )),
            Ok(Command::Import(ImportArgs {
                from: "mysql://u:p@h/chv".into(),
                files_dir: Some("/srv/images".into()),
                dry_run: true,
                ..ImportArgs::default()
            }))
        );
        assert!(
            parse(&args("import --files-dir /srv")).is_err(),
            "needs --from"
        );
        assert!(parse(&args("import --from")).is_err());
    }
}

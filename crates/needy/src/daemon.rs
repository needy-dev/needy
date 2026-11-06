use crate::{
    docker::Docker,
    model::{self, Input, Run, Session, StartRun},
    store::{self, Client, Disk, Record},
    workspace,
};
use anyhow::{Context, Result, ensure};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::{
    io::BufReader,
    sync::{Mutex, watch},
};

#[derive(Clone, clap::Args)]
pub struct Config {
    #[arg(long, default_value = ".needy/rust-host")]
    pub data_dir: PathBuf,
    #[arg(long, default_value = "127.0.0.1:7331")]
    pub listen: std::net::SocketAddr,
    #[arg(long, default_value = "needy-runtime:rust")]
    pub image: String,
    #[arg(long, default_value_t = 2)]
    pub max_concurrent: usize,
    #[arg(long)]
    pub host_root: Option<PathBuf>,
    #[arg(long, value_delimiter = ',')]
    pub allow_host_exec: Vec<String>,
    #[arg(long, value_delimiter = ',', default_value = "TERM,LANG,LC_ALL")]
    pub host_env: Vec<String>,
    #[arg(long)]
    pub no_docker: bool,
    #[arg(long, requires = "tls_key")]
    pub tls_cert: Option<PathBuf>,
    #[arg(long, requires = "tls_cert")]
    pub tls_key: Option<PathBuf>,
}

pub struct Active {
    pub owner: String,
    pub cancel: watch::Sender<bool>,
    pub native: bool,
}
pub struct Server {
    pub config: Config,
    pub disk: Mutex<Disk>,
    pub docker: Docker,
    pub active: Mutex<HashMap<String, Active>>,
    pub running: AtomicBool,
    locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
    pub host_commands: BTreeMap<String, PathBuf>,
}
pub type Shared = Arc<Server>;

pub struct ApiError(pub StatusCode, pub String);
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        eprintln!("needy operation failed: {error:#}");
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "operation failed; inspect daemon diagnostics".into(),
        )
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
pub type Api<T> = std::result::Result<T, ApiError>;
pub fn problem(code: StatusCode, message: &str) -> ApiError {
    ApiError(code, message.into())
}

impl Server {
    pub async fn authorize(&self, headers: &HeaderMap) -> Api<String> {
        let token = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| problem(StatusCode::UNAUTHORIZED, "client authorization required"))?;
        let hash = store::digest(token);
        if self
            .disk
            .lock()
            .await
            .clients
            .values()
            .any(|client| client.token_hash == hash)
        {
            Ok(hash)
        } else {
            Err(problem(
                StatusCode::UNAUTHORIZED,
                "client authorization revoked or invalid",
            ))
        }
    }
    pub fn session_lock(&self, id: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .unwrap()
            .entry(id.into())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
    pub async fn finish(
        &self,
        id: &str,
        state: &str,
        code: Option<i32>,
        error: Option<String>,
    ) -> Result<()> {
        let mut disk = self.disk.lock().await;
        let record = disk.runs.get_mut(id).context("run not found")?;
        record.run.state = state.into();
        record.run.exit_code = code;
        record.run.error = error;
        let run = record.run.clone();
        disk.sessions
            .get_mut(&run.session_id)
            .context("session missing")?
            .active_run_id = None;
        disk.event(id, "state", serde_json::to_value(run)?, None)
    }
    pub async fn set_state(&self, id: &str, state: &str) -> Result<()> {
        let mut disk = self.disk.lock().await;
        let record = disk.runs.get_mut(id).context("run not found")?;
        record.run.state = if record.stop_requested {
            "cancelling"
        } else {
            state
        }
        .into();
        let run = record.run.clone();
        disk.event(id, "state", serde_json::to_value(run)?, None)
    }
    pub async fn capacity(&self, disk: &Disk) -> Api<()> {
        let native = self
            .active
            .lock()
            .await
            .values()
            .filter(|active| active.native)
            .count();
        if disk
            .runs
            .values()
            .filter(|record| !record.run.terminal())
            .count()
            + native
            >= self.config.max_concurrent
        {
            return Err(problem(
                StatusCode::CONFLICT,
                "host concurrency limit reached",
            ));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Pairing {
    hash: String,
    expires_at: i64,
}

pub fn pairing_code(directory: &std::path::Path, output: &std::path::Path) -> Result<()> {
    ensure!(
        directory.join("rust-state.json").exists(),
        "start the Rust daemon before generating a pairing code"
    );
    let code = store::token();
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)?;
    writeln!(file, "{code}")?;
    file.sync_all()?;
    store::atomic_json(
        &directory.join("pairing.json"),
        &Pairing {
            hash: store::digest(&code),
            expires_at: chrono::Utc::now().timestamp() + 300,
        },
    )?;
    Ok(())
}

pub async fn serve(mut config: Config) -> Result<()> {
    ensure!(
        config.tls_cert.is_some() == config.tls_key.is_some(),
        "provide TLS certificate and key together"
    );
    ensure!(
        config.listen.ip().is_loopback() || config.tls_cert.is_some(),
        "non-loopback listeners require TLS"
    );
    ensure!(config.max_concurrent > 0, "concurrency must be positive");
    store::private_directory(&config.data_dir)?;
    config.data_dir = std::fs::canonicalize(&config.data_dir)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(config.data_dir.join("daemon.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("another daemon owns this state directory")?;
    let disk = Disk::open(&config.data_dir)?;
    let docker = Docker {
        instance: disk.instance_id.clone(),
        image: config.image.clone(),
    };
    if !config.no_docker {
        docker.check().await?;
    }
    let mut host_commands = BTreeMap::new();
    if let Some(root) = &config.host_root {
        config.host_root = Some(std::fs::canonicalize(root)?);
    }
    ensure!(
        config.allow_host_exec.is_empty() || config.host_root.is_some(),
        "host execution requires --host-root"
    );
    for name in &config.allow_host_exec {
        ensure!(
            !name.contains('/') && !name.is_empty(),
            "allow host commands by logical name"
        );
        let path = std::env::split_paths(&std::env::var_os("PATH").context("PATH unset")?)
            .map(|p| p.join(name))
            .find(|p| p.is_file())
            .context("allowed host command not found")?;
        host_commands.insert(name.clone(), std::fs::canonicalize(path)?);
    }
    let server = Arc::new(Server {
        config,
        disk: Mutex::new(disk),
        docker,
        active: Mutex::new(HashMap::new()),
        running: AtomicBool::new(true),
        locks: StdMutex::new(HashMap::new()),
        host_commands,
    });
    recover(server.clone()).await?;
    let router = Router::new()
        .route("/v1/pair", post(pair))
        .route("/v1/info", get(info))
        .route("/v1/clients", get(clients))
        .route("/v1/clients/{id}", axum::routing::delete(revoke))
        .route("/v1/sessions", get(sessions).post(create_session))
        .route("/v1/sessions/{id}", get(session).delete(delete_session))
        .route("/v1/sessions/{id}/workspace", get(download).put(upload))
        .route("/v1/sessions/{id}/runs", post(start_run))
        .route("/v1/exec", post(crate::tunnel::exec_upgrade))
        .route("/v1/runs/{id}", get(run))
        .route("/v1/runs/{id}/stop", post(stop))
        .route("/v1/runs/{id}/events", get(events))
        .layer(DefaultBodyLimit::max(workspace::MAX))
        .with_state(server.clone());
    let listener = std::net::TcpListener::bind(server.config.listen)?;
    listener.set_nonblocking(true)?;
    eprintln!("Needy Rust daemon listening on {}", listener.local_addr()?);
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    let shutdown_server = server.clone();
    let shutdown = tokio::spawn(async move {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("register SIGTERM");
        tokio::select! { _ = tokio::signal::ctrl_c() => (), _ = terminate.recv() => () }
        shutdown_server.running.store(false, Ordering::Relaxed);
        for active in shutdown_server.active.lock().await.values() {
            let _ = active.cancel.send(true);
        }
        shutdown_handle.graceful_shutdown(Some(std::time::Duration::from_secs(10)));
    });
    if let (Some(cert), Some(key)) = (&server.config.tls_cert, &server.config.tls_key) {
        if rustls::crypto::CryptoProvider::get_default().is_none() {
            rustls::crypto::ring::default_provider()
                .install_default()
                .map_err(|_| anyhow::anyhow!("cannot install TLS crypto provider"))?;
        }
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
        axum_server::from_tcp_rustls(listener, tls)?
            .handle(handle)
            .serve(router.into_make_service())
            .await?;
    } else {
        axum_server::from_tcp(listener)?
            .handle(handle)
            .serve(router.into_make_service())
            .await?;
    }
    shutdown.abort();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !server.active.lock().await.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Ok(())
}

async fn pair(State(s): State<Shared>, Json(request): Json<Value>) -> Api<Json<Value>> {
    let code = request
        .get("code")
        .and_then(Value::as_str)
        .ok_or_else(|| problem(StatusCode::BAD_REQUEST, "pairing code required"))?;
    let mut disk = s.disk.lock().await;
    let path = s.config.data_dir.join("pairing.json");
    if !path.exists() {
        return Err(problem(StatusCode::UNAUTHORIZED, "pairing unavailable"));
    }
    let pairing: Pairing =
        serde_json::from_reader(std::fs::File::open(path).map_err(anyhow::Error::from)?)
            .map_err(anyhow::Error::from)?;
    if pairing.expires_at < chrono::Utc::now().timestamp()
        || store::digest(code) != pairing.hash
        || disk.consumed_pairing_hash.as_ref() == Some(&pairing.hash)
        || disk.consumed_pairings.contains_key(&pairing.hash)
    {
        return Err(problem(
            StatusCode::UNAUTHORIZED,
            "pairing code invalid or consumed",
        ));
    }
    let token = store::token();
    let id = model::id();
    disk.clients.insert(
        id.clone(),
        Client {
            id: id.clone(),
            name: request
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("client")
                .into(),
            created_at: model::now(),
            token_hash: store::digest(&token),
        },
    );
    disk.consumed_pairings
        .retain(|_, expiry| *expiry >= chrono::Utc::now().timestamp());
    disk.consumed_pairings
        .insert(pairing.hash, pairing.expires_at);
    disk.save()?;
    Ok(Json(json!({"token":token,"client_id":id})))
}
async fn info(State(s): State<Shared>, headers: HeaderMap) -> Api<Json<Value>> {
    s.authorize(&headers).await?;
    let disk = s.disk.lock().await;
    let native = s.active.lock().await.values().filter(|a| a.native).count();
    Ok(Json(
        json!({"instance_id":disk.instance_id,"image":s.config.image,"max_concurrent":s.config.max_concurrent,"active_runs":disk.runs.values().filter(|r|!r.run.terminal()).count()+native,"active_native_runs":native,"host_execution":!s.host_commands.is_empty()}),
    ))
}
async fn clients(State(s): State<Shared>, headers: HeaderMap) -> Api<Json<Value>> {
    s.authorize(&headers).await?;
    Ok(Json(json!(
        s.disk
            .lock()
            .await
            .clients
            .values()
            .map(|v| json!({"id":v.id,"name":v.name,"created_at":v.created_at}))
            .collect::<Vec<_>>()
    )))
}
async fn revoke(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Api<Json<Value>> {
    s.authorize(&headers).await?;
    let owner = {
        let mut disk = s.disk.lock().await;
        let client = disk
            .clients
            .remove(&id)
            .ok_or_else(|| problem(StatusCode::NOT_FOUND, "client not found"))?;
        disk.save()?;
        client.token_hash
    };
    for active in s
        .active
        .lock()
        .await
        .values()
        .filter(|active| active.owner == owner)
    {
        let _ = active.cancel.send(true);
    }
    Ok(Json(json!({"revoked":id})))
}
async fn sessions(State(s): State<Shared>, headers: HeaderMap) -> Api<Json<Value>> {
    s.authorize(&headers).await?;
    Ok(Json(json!(
        s.disk.lock().await.sessions.values().collect::<Vec<_>>()
    )))
}
async fn session(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Api<Json<Session>> {
    s.authorize(&headers).await?;
    Ok(Json(
        s.disk
            .lock()
            .await
            .sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| problem(StatusCode::NOT_FOUND, "session not found"))?,
    ))
}
async fn create_session(
    State(s): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Api<Json<Session>> {
    s.authorize(&headers).await?;
    if s.config.no_docker {
        return Err(problem(StatusCode::CONFLICT, "Docker backend disabled"));
    }
    let session = Session {
        id: model::id(),
        name: request
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .into(),
        created_at: model::now(),
        active_run_id: None,
    };
    s.docker.create_session(&session.id).await?;
    let mut disk = s.disk.lock().await;
    disk.sessions.insert(session.id.clone(), session.clone());
    disk.save()?;
    Ok(Json(session))
}
async fn delete_session(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Api<Json<Value>> {
    s.authorize(&headers).await?;
    let _guard = s
        .session_lock(&id)
        .try_lock_owned()
        .map_err(|_| problem(StatusCode::CONFLICT, "workspace busy"))?;
    let records = {
        let disk = s.disk.lock().await;
        let session = disk
            .sessions
            .get(&id)
            .ok_or_else(|| problem(StatusCode::NOT_FOUND, "session not found"))?;
        if session.active_run_id.is_some() {
            return Err(problem(StatusCode::CONFLICT, "session active"));
        }
        disk.runs
            .values()
            .filter(|r| r.run.session_id == id)
            .cloned()
            .collect::<Vec<_>>()
    };
    for record in &records {
        s.docker.remove(&record.container).await?;
    }
    s.docker.delete_session(&id).await?;
    let mut disk = s.disk.lock().await;
    disk.sessions.remove(&id);
    disk.runs.retain(|_, r| r.run.session_id != id);
    disk.save()?;
    for record in records {
        std::fs::remove_dir_all(s.config.data_dir.join("runs").join(record.run.id))
            .map_err(anyhow::Error::from)?;
    }
    Ok(Json(json!({"deleted":id})))
}
async fn upload(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Api<Json<Value>> {
    s.authorize(&headers).await?;
    let _guard = s
        .session_lock(&id)
        .try_lock_owned()
        .map_err(|_| problem(StatusCode::CONFLICT, "workspace busy"))?;
    {
        let disk = s.disk.lock().await;
        if !disk.sessions.contains_key(&id) {
            return Err(problem(StatusCode::NOT_FOUND, "session not found"));
        }
        if disk.runs.values().any(|r| r.run.session_id == id) {
            return Err(problem(
                StatusCode::CONFLICT,
                "upload must precede the first run",
            ));
        }
    }
    workspace::validate(&bytes).map_err(|e| problem(StatusCode::BAD_REQUEST, &e.to_string()))?;
    s.docker.import(&id, &bytes).await?;
    Ok(Json(json!({"uploaded":id})))
}
async fn download(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Api<Response> {
    s.authorize(&headers).await?;
    let _guard = s
        .session_lock(&id)
        .try_lock_owned()
        .map_err(|_| problem(StatusCode::CONFLICT, "workspace busy"))?;
    {
        let disk = s.disk.lock().await;
        let session = disk
            .sessions
            .get(&id)
            .ok_or_else(|| problem(StatusCode::NOT_FOUND, "session not found"))?;
        if session.active_run_id.is_some() {
            return Err(problem(
                StatusCode::CONFLICT,
                "stop active work before download",
            ));
        }
    }
    let bytes = s.docker.export(&id).await?;
    workspace::validate(&bytes)?;
    Ok(([("content-type", "application/x-tar")], bytes).into_response())
}

pub fn validate_command(command: &[String]) -> Result<()> {
    ensure!(
        !command.is_empty()
            && command.len() <= 256
            && !command[0].is_empty()
            && command.iter().all(|v| !v.contains('\0'))
            && command.iter().map(String::len).sum::<usize>() <= 65536,
        "invalid command"
    );
    Ok(())
}
async fn start_run(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(sid): Path<String>,
    Json(request): Json<StartRun>,
) -> Api<Json<Run>> {
    s.authorize(&headers).await?;
    let _guard = s
        .session_lock(&sid)
        .try_lock_owned()
        .map_err(|_| problem(StatusCode::CONFLICT, "workspace busy"))?;
    let key = headers
        .get("idempotency-key")
        .and_then(|v| v.to_str().ok())
        .filter(|v| (8..=128).contains(&v.len()))
        .ok_or_else(|| problem(StatusCode::BAD_REQUEST, "Idempotency-Key required"))?
        .to_owned();
    let hash =
        store::digest(serde_json::to_vec(&json!([sid, request])).map_err(anyhow::Error::from)?);
    let mut disk = s.disk.lock().await;
    if let Some(record) = disk.runs.values().find(|r| r.request_key == key) {
        if record.request_hash != hash {
            return Err(problem(
                StatusCode::CONFLICT,
                "Idempotency-Key reused with different request",
            ));
        }
        return Ok(Json(record.run.clone()));
    }
    let session = disk
        .sessions
        .get(&sid)
        .cloned()
        .ok_or_else(|| problem(StatusCode::NOT_FOUND, "session not found"))?;
    if session.active_run_id.is_some() {
        return Err(problem(StatusCode::CONFLICT, "session busy"));
    }
    s.capacity(&disk).await?;
    let id = model::id();
    let directory = s.config.data_dir.join("runs").join(&id);
    store::private_directory(&directory)?;
    let input_directory = directory.join("input");
    std::fs::create_dir(&input_directory).map_err(anyhow::Error::from)?;
    std::fs::set_permissions(&input_directory, std::fs::Permissions::from_mode(0o755))
        .map_err(anyhow::Error::from)?;
    validate_command(&request.command)
        .map_err(|e| problem(StatusCode::BAD_REQUEST, &e.to_string()))?;
    input_file(
        &input_directory.join("input.json"),
        &Input {
            command: request.command,
            stdin: request.stdin,
        },
    )?;
    let run = Run {
        id: id.clone(),
        session_id: sid.clone(),
        state: "creating".into(),
        created_at: model::now(),
        exit_code: None,
        error: None,
    };
    disk.sessions.get_mut(&sid).unwrap().active_run_id = Some(id.clone());
    disk.runs.insert(
        id.clone(),
        Record {
            run: run.clone(),
            container: s.docker.container(&id),
            request_key: key,
            request_hash: hash,
            stop_requested: false,
            events: Vec::new(),
            source_line: 0,
        },
    );
    disk.event(
        &id,
        "state",
        serde_json::to_value(&run).map_err(anyhow::Error::from)?,
        None,
    )?;
    drop(disk);
    tokio::spawn(async move {
        let result = async {
            s.docker.create_run(&sid, &id, &input_directory).await?;
            if s.disk.lock().await.runs[&id].stop_requested {
                s.docker.remove(&s.docker.container(&id)).await?;
                s.finish(&id, "cancelled", None, None).await?;
                return Ok::<_, anyhow::Error>(());
            }
            s.set_state(&id, "starting").await?;
            s.docker.start(&s.docker.container(&id)).await?;
            if s.disk.lock().await.runs[&id].stop_requested {
                s.docker.stop(&s.docker.container(&id)).await?;
            }
            s.set_state(&id, "running").await?;
            watch(s.clone(), &id).await
        }
        .await;
        if let Err(error) = result {
            eprintln!("batch observation failed: {error:#}");
            if let Err(error) = watch(s.clone(), &id).await {
                eprintln!("run reservation retained after supervision error: {error:#}");
            }
        }
    });
    Ok(Json(run))
}

fn input_file(path: &std::path::Path, value: &impl Serialize) -> Result<()> {
    store::atomic_json(path, value)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o444))?;
    Ok(())
}
async fn run(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Api<Json<Run>> {
    s.authorize(&headers).await?;
    Ok(Json(
        s.disk
            .lock()
            .await
            .runs
            .get(&id)
            .ok_or_else(|| problem(StatusCode::NOT_FOUND, "run not found"))?
            .run
            .clone(),
    ))
}
async fn stop(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Api<Json<Run>> {
    s.authorize(&headers).await?;
    let container = {
        let mut disk = s.disk.lock().await;
        let r = disk
            .runs
            .get_mut(&id)
            .ok_or_else(|| problem(StatusCode::NOT_FOUND, "run not found"))?;
        if r.run.terminal() {
            return Ok(Json(r.run.clone()));
        }
        r.stop_requested = true;
        let container = r.container.clone();
        disk.save()?;
        container
    };
    s.set_state(&id, "cancelling").await?;
    s.docker.stop(&container).await?;
    Ok(Json(s.disk.lock().await.runs[&id].run.clone()))
}
#[derive(Deserialize)]
struct EventQuery {
    #[serde(default)]
    after: u64,
    #[serde(default)]
    follow: bool,
}
async fn events(
    State(s): State<Shared>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<EventQuery>,
) -> Api<Response> {
    let owner = s.authorize(&headers).await?;
    if !s.disk.lock().await.runs.contains_key(&id) {
        return Err(problem(StatusCode::NOT_FOUND, "run not found"));
    }
    let stream = futures_util::stream::try_unfold(
        (s, id, owner, query.after, false),
        move |(s, id, owner, after, done)| async move {
            if done {
                return Ok::<_, std::io::Error>(None);
            }
            loop {
                let disk = s.disk.lock().await;
                if !disk.clients.values().any(|c| c.token_hash == owner) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "authorization revoked",
                    ));
                }
                let record = disk
                    .runs
                    .get(&id)
                    .ok_or_else(|| std::io::Error::other("run removed"))?;
                let mut bytes = Vec::new();
                let mut last = after;
                for event in record.events.iter().filter(|e| e.id > after) {
                    let mut event = event.clone();
                    last = event.id;
                    event.source_line = None;
                    serde_json::to_writer(&mut bytes, &event)?;
                    bytes.push(b'\n');
                }
                let done = record.run.terminal() || !query.follow;
                drop(disk);
                if !bytes.is_empty() {
                    return Ok(Some((Bytes::from(bytes), (s, id, owner, last, done))));
                }
                if done {
                    return Ok(None);
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        },
    );
    Ok((
        [("content-type", "application/x-ndjson")],
        Body::from_stream(stream),
    )
        .into_response())
}

async fn observe(s: Shared, id: &str) -> Result<()> {
    let container = s.disk.lock().await.runs[id].container.clone();
    let mut logs = s.docker.logs(&container).await?;
    let mut lines = BufReader::new(logs.stdout.take().unwrap());
    let mut source = 0u64;
    let mut total = 0usize;
    loop {
        tokio::select! {
            line=crate::wire::read_line(&mut lines,16<<20)=>{
                let line=match line{
                    Ok(Some(line))=>line,Ok(None)=>break,
                    Err(error) if error.kind()==std::io::ErrorKind::InvalidData=>{s.docker.stop(&container).await?;s.finish(id,"failed",None,Some("invalid or oversized runner output".into())).await?;return Ok(());}
                    Err(error)=>return Err(error.into()),
                };
                source+=1;total+=line.len()+1;
                if total>16<<20 {s.docker.stop(&container).await?;s.finish(id,"failed",None,Some("output limit exceeded".into())).await?;return Ok(());}
                let value:Value=match serde_json::from_slice::<Value>(&line){
                    Ok(value) if value["line"].is_string() && matches!(value["stream"].as_str(),Some("stdout"|"stderr"))=>value,
                    _=>{s.docker.stop(&container).await?;s.finish(id,"failed",None,Some("invalid runner envelope".into())).await?;return Ok(());}
                };
                let mut disk=s.disk.lock().await;
                if disk.runs[id].run.error.is_some(){
                    disk.runs.get_mut(id).unwrap().run.error=None;
                    let run=disk.runs[id].run.clone();disk.event(id,"state",serde_json::to_value(run)?,None)?;
                }
                if source<=disk.runs[id].source_line{continue;}
                disk.event(id,"output",value,Some(source))?;
            }
            _=tokio::time::sleep(std::time::Duration::from_millis(100))=>{if !s.running.load(Ordering::Relaxed){return Ok(());}}
        }
    }
    ensure!(
        logs.wait().await?.success(),
        "Docker log observation failed"
    );
    let status = s.docker.inspect(&container).await?.state;
    ensure!(
        matches!(status.status.as_str(), "exited" | "dead"),
        "container still running after log stream ended"
    );
    let stopped = s.disk.lock().await.runs[id].stop_requested;
    let state = if stopped {
        "cancelled"
    } else if status.exit_code == 0 {
        "succeeded"
    } else {
        "failed"
    };
    s.finish(id, state, Some(status.exit_code), None).await
}

async fn watch(s: Shared, id: &str) -> Result<()> {
    loop {
        if !s.running.load(Ordering::Relaxed) {
            return Ok(());
        }
        let record = s
            .disk
            .lock()
            .await
            .runs
            .get(id)
            .cloned()
            .context("run missing during supervision")?;
        if record.run.terminal() {
            return Ok(());
        }
        let result = async {
            if !s.docker.exists(&record.container).await? {
                return s
                    .finish(
                        id,
                        if record.stop_requested {
                            "cancelled"
                        } else {
                            "lost"
                        },
                        None,
                        Some("container unavailable; workload was not restarted".into()),
                    )
                    .await;
            }
            let state = s.docker.inspect(&record.container).await?.state;
            if state.status == "created" {
                s.docker.remove(&record.container).await?;
                return s
                    .finish(
                        id,
                        if record.stop_requested {
                            "cancelled"
                        } else {
                            "lost"
                        },
                        None,
                        Some("execution did not start; workload was not restarted".into()),
                    )
                    .await;
            }
            if record.stop_requested {
                s.docker.stop(&record.container).await?;
            }
            observe(s.clone(), id).await
        }
        .await;
        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                let mut disk = s.disk.lock().await;
                let record = disk
                    .runs
                    .get_mut(id)
                    .context("run missing after observation failure")?;
                if record.run.terminal() {
                    return Err(error);
                }
                let message =
                    "execution observation unavailable; retaining reservation and retrying";
                if record.run.error.as_deref() != Some(message) {
                    eprintln!("batch observation unavailable: {error:#}");
                    record.run.error = Some(message.into());
                    let run = record.run.clone();
                    disk.event(id, "state", serde_json::to_value(run)?, None)?;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn recover(s: Shared) -> Result<()> {
    let records = s
        .disk
        .lock()
        .await
        .runs
        .values()
        .filter(|r| !r.run.terminal())
        .cloned()
        .collect::<Vec<_>>();
    for record in records {
        let s = s.clone();
        tokio::spawn(async move {
            if let Err(error) = watch(s, &record.run.id).await {
                eprintln!("recovery failed: {error:#}");
            }
        });
    }
    Ok(())
}

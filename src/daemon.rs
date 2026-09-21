use crate::{audit, config, reviewer::Assessment, sessions::SessionPool};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        process::CommandExt,
    },
    path::Path,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Mutex, Notify},
};

const LIMIT: usize = 1024 * 1024;
async fn read_line(stream: &mut BufReader<UnixStream>) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = stream.fill_buf().await?;
        if chunk.is_empty() {
            bail!("Connection closed before newline");
        }
        let end = chunk.iter().position(|b| *b == b'\n').map(|n| n + 1);
        let n = end.unwrap_or(chunk.len());
        if bytes.len() + n > LIMIT {
            bail!("IPC message exceeds 1 MiB");
        }
        bytes.extend_from_slice(&chunk[..n]);
        stream.consume(n);
        if end.is_some() {
            return Ok(bytes);
        }
    }
}
pub async fn request(path: &Path, req: &Value, seconds: u64) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(seconds), async {
        let mut stream = UnixStream::connect(path).await?;
        stream.write_all(format!("{req}\n").as_bytes()).await?;
        let bytes = read_line(&mut BufReader::new(stream)).await?;
        Ok(serde_json::from_slice(&bytes)?)
    })
    .await
    .context("Daemon request timed out")?
}
pub async fn start() -> Result<Value> {
    let path = config::socket_path();
    if let Ok(v) = request(&path, &json!({"action":"ping"}), 1).await
        && v["status"] == "pong"
        && v["protocol_version"] == 2
    {
        return Ok(v);
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["daemon", "run"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command.spawn().context("Cannot spawn daemon")?;
    let until = Instant::now() + Duration::from_secs(3);
    while Instant::now() < until {
        if let Ok(v) = request(&path, &json!({"action":"ping"}), 1).await
            && v["status"] == "pong"
            && v["protocol_version"] == 2
        {
            return Ok(v);
        }
        // A competing starter may own the lock but not have bound its socket yet.
        // Reap our losing child and continue probing until the startup deadline.
        let _ = child.try_wait()?;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    bail!("Failed to start daemon within timeout; run `any-auto daemon run` for diagnostics")
}
pub async fn stop() -> Result<()> {
    let path = config::socket_path();
    let response = request(&path, &json!({"action":"stop"}), 1).await?;
    anyhow::ensure!(
        response["status"] == "stopping",
        "Unexpected stop response: {response}"
    );
    for _ in 0..100 {
        if !path.exists() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    bail!("Daemon acknowledged stop but socket still exists");
}

pub async fn restart() -> Result<Value> {
    let path = config::socket_path();
    match UnixStream::connect(&path).await {
        Ok(stream) => {
            drop(stream);
            stop().await?;
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) => {}
        Err(e) => return Err(e).context("Cannot contact daemon for restart"),
    }
    start().await
}

pub async fn review(payload: &Value) -> Result<Assessment> {
    review_traced(payload, &audit::request_id()).await
}
pub async fn review_traced(payload: &Value, id: &str) -> Result<Assessment> {
    start().await?;
    let req = json!({"action":"evaluate", "context":crate::context::RequestContext::capture()?, "mode":config::mode(), "request_id":id, "user_session_id":crate::sessions::user_session_id(payload), "toolCall":payload["toolCall"], "workspacePaths":payload["workspacePaths"], "original_tool":payload.get("original_tool").unwrap_or(&payload["toolCall"]), "authorization":payload["authorization"]});
    let v = request(&config::socket_path(), &req, 25).await?;
    let a: Assessment =
        serde_json::from_value(v["assessment"].clone()).context("Invalid daemon assessment")?;
    Ok(a)
}
struct State {
    instances: std::sync::Mutex<std::collections::BTreeMap<(String, String), SessionPool>>,
    stop: Notify,
    started: Instant,
    last_active: Mutex<Instant>,
    requests: AtomicU64,
    active: AtomicU64,
    idle_timeout: u64,
    session_idle_timeout: u64,
}
async fn handle(stream: UnixStream, state: Arc<State>) {
    let mut stream = BufReader::new(stream);
    let response: Result<Value> = async {
        let bytes = tokio::time::timeout(Duration::from_secs(5), read_line(&mut stream)).await??;
        let req: Value = serde_json::from_slice(&bytes)?;
        *state.last_active.lock().await = Instant::now();
        match req["action"].as_str().unwrap_or("") {
            "ping" | "status" => {
                let instances = state.instances.lock().unwrap();
                let details: Vec<_> = instances.iter().filter(|((agent, instance), _)|
                    req["agent"].as_str().is_none_or(|h| h == agent) && req["instance"].as_str().is_none_or(|i| i == instance))
                    .map(|((agent, instance), pool)| json!({"agent":agent,"instance":instance,"cached_sessions":pool.len()})).collect();
                Ok(json!({"status": if req["action"] == "ping" {"pong"} else {"running"},
                    "protocol_version":2,"pid":std::process::id(), "socket":config::socket_path(), "version":env!("CARGO_PKG_VERSION"),
                    "uptime_seconds":state.started.elapsed().as_secs(), "idle_timeout_seconds":state.idle_timeout,
                    "session_idle_timeout_seconds":state.session_idle_timeout,
                    "instances":details,"cached_sessions":instances.values().map(SessionPool::len).sum::<usize>(),
                    "evaluations":state.requests.load(Ordering::Relaxed), "active_evaluations":state.active.load(Ordering::Relaxed)}))
            }
            "stop" => Ok(json!({"status":"stopping"})),
            "reset" | "evaluate" => {
                let context: crate::context::RequestContext = serde_json::from_value(req["context"].clone()).context("Missing or invalid request context")?;
                context.validate()?;
                crate::context::scope(Arc::new(context), async {
                    let key = (config::mode().agent().to_owned(), config::instance());
                    if req["action"] == "reset" {
                        let mut instances = state.instances.lock().unwrap();
                        let pool = instances.entry(key.clone()).or_insert_with(|| SessionPool::new(config::mode(), config::state_dir()));
                        pool.reset()?;
                        instances.remove(&key);
                        return Ok(json!({"status":"reset", "agent":key.0, "instance":key.1}));
                    }
                    let session = {
                        let mut instances = state.instances.lock().unwrap();
                        instances.entry(key).or_insert_with(|| SessionPool::new(config::mode(), config::state_dir()))
                            .acquire(req["user_session_id"].as_str().filter(|id| !id.trim().is_empty()))?
                    };
                    state.requests.fetch_add(1, Ordering::Relaxed);
                    state.active.fetch_add(1, Ordering::Relaxed);
                    let evaluation = tokio::select! {
                        result = tokio::time::timeout(Duration::from_secs(24), session.evaluate(&req)) => match result {
                            Ok(a) => a,
                            Err(_) => Assessment::deny("Review deadline exceeded"),
                        },
                        _ = stream.read_u8() => Assessment::deny("Approval caller disconnected"),
                    };
                    if let Some(id) = req["request_id"].as_str() {
                        audit::record(id, "daemon_result", json!({"assessment":evaluation}));
                    }
                    state.active.fetch_sub(1, Ordering::Relaxed);
                    *state.last_active.lock().await = Instant::now();
                    Ok(json!({"status":"ok", "assessment":evaluation}))
                }).await
            }
            _ => Ok(json!({"status":"error", "message":"Unknown action"})),
        }
    }.await;
    let v = response.unwrap_or_else(|e| json!({"status":"error", "assessment":Assessment::deny(format!("Daemon internal error: {e}"))}));
    let _ = stream
        .get_mut()
        .write_all(format!("{v}\n").as_bytes())
        .await;
    let _ = stream.get_mut().shutdown().await;
    if v["status"] == "stopping" {
        state.stop.notify_one();
    }
}
struct SocketCleanup(std::path::PathBuf);
impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
pub async fn run(idle_timeout: u64, session_idle_timeout: u64) -> Result<()> {
    let path = config::socket_path();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    // Lifetime lock prevents concurrent starts from replacing a live socket.
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path.with_extension("sock.lock"))?;
    lock.try_lock_exclusive()
        .context("Another daemon owns this socket")?;
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            bail!("Refusing to replace non-socket path {}", path.display());
        }
        if UnixStream::connect(&path).await.is_ok() {
            bail!("A daemon already listens on {}", path.display());
        }
        fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    let _cleanup = SocketCleanup(path.clone());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let state = Arc::new(State {
        instances: std::sync::Mutex::new(std::collections::BTreeMap::new()),
        stop: Notify::new(),
        started: Instant::now(),
        last_active: Mutex::new(Instant::now()),
        requests: AtomicU64::new(0),
        active: AtomicU64::new(0),
        idle_timeout,
        session_idle_timeout,
    });
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut clients = tokio::task::JoinSet::new();
    eprintln!("Approver daemon listening on {}", path.display());
    loop {
        tokio::select! {
            conn = listener.accept() => { let (stream, _) = conn?; clients.spawn(handle(stream, state.clone())); }
            _ = state.stop.notified() => break,
            _ = term.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
            _ = clients.join_next(), if !clients.is_empty() => {},
            _ = tick.tick() => {
                {
                    let mut instances = state.instances.lock().unwrap();
                    for pool in instances.values() { pool.prune(Duration::from_secs(session_idle_timeout)); }
                    instances.retain(|_, pool| !pool.is_empty());
                }
                if idle_timeout > 0 && state.active.load(Ordering::Relaxed) == 0 && state.last_active.lock().await.elapsed().as_secs() >= idle_timeout { break; }
            }
        }
    }
    clients.abort_all();
    while clients.join_next().await.is_some() {}
    drop(state);
    crate::backend::reap_children().await;
    Ok(())
}

/// Compatibility alias for the single daemon status.
pub async fn status_all() -> Result<Value> {
    status(None, None).await
}
pub async fn status(agent: Option<config::Mode>, instance: Option<&str>) -> Result<Value> {
    request(
        &config::socket_path(),
        &json!({"action":"status","agent":agent.map(|m| m.agent()),"instance":instance}),
        1,
    )
    .await
}
pub async fn reset() -> Result<Value> {
    start().await?;
    let response = request(
        &config::socket_path(),
        &json!({"action":"reset", "context":crate::context::RequestContext::capture()?}),
        3,
    )
    .await?;
    anyhow::ensure!(
        response["status"] == "reset",
        "Instance reset failed: {}",
        response["assessment"]["rationale"]
            .as_str()
            .unwrap_or("unexpected response")
    );
    Ok(response)
}

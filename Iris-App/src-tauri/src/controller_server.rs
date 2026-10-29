use super::*;
use axum::{extract::{DefaultBodyLimit, Path as ApiPath, Request, State}, http::{HeaderMap, StatusCode}, middleware::{self, Next}, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::{oneshot, Semaphore};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::sync::atomic::AtomicUsize;

/// DNS-SD service type the Android companion browses for on the LAN.
const MDNS_SERVICE_TYPE: &str = "_iris-companion._tcp.local.";
/// Pairing locks after this many wrong PIN attempts until the PIN is rotated or the server restarts.
const MAX_PAIR_ATTEMPTS: usize = 5;

/// Short, human-typable pairing PIN drawn from a CSPRNG (uuid v4), shown on the desktop only.
fn new_pin() -> String {
    let bytes = uuid::Uuid::new_v4().into_bytes();
    let value = ((bytes[0] as u32) << 16 | (bytes[1] as u32) << 8 | bytes[2] as u32) % 1_000_000;
    format!("{value:06}")
}

fn lan_ipv4() -> Option<std::net::Ipv4Addr> {
    match local_ip_address::local_ip() {
        Ok(std::net::IpAddr::V4(ip)) => Some(ip),
        _ => None,
    }
}

struct MdnsHandle {
    daemon: ServiceDaemon,
    fullname: String,
}

impl MdnsHandle {
    fn stop(self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// Advertise the companion server over mDNS so phones on the same Wi-Fi discover it automatically.
/// The PIN is never advertised — only the service's existence and that pairing is required.
fn start_mdns(port: u16) -> Option<MdnsHandle> {
    let ip = lan_ipv4()?;
    let daemon = ServiceDaemon::new().ok()?;
    let host = format!("iris-companion-{port}.local.");
    let ip_str = ip.to_string();
    let properties = [("api", "v1"), ("requiresPin", "true")];
    let service = ServiceInfo::new(MDNS_SERVICE_TYPE, "Iris Companion", &host, ip_str.as_str(), port, &properties[..]).ok()?;
    let fullname = service.get_fullname().to_string();
    daemon.register(service).ok()?;
    Some(MdnsHandle { daemon, fullname })
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerConfig {
    pub port: u16,
    pub allow_read: bool,
    pub allow_control: bool,
    /// Bind 0.0.0.0 (reachable from the LAN) instead of localhost only.
    #[serde(default)]
    pub bind_lan: bool,
    /// Advertise the server over mDNS for zero-config phone discovery (requires bind_lan).
    #[serde(default)]
    pub discoverable: bool,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ControllerJob {
    id: String,
    tab_id: u32,
    status: String,
    error: Option<String>,
    started_at: i64,
    updated_at: i64,
    #[serde(skip)]
    cancelled: Arc<AtomicBool>,
    #[serde(skip)]
    ready: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
struct ControllerState {
    app: tauri::AppHandle,
    token: Arc<Mutex<String>>,
    config: ControllerConfig,
    jobs: Arc<Mutex<HashMap<String, ControllerJob>>>,
    active: Arc<Semaphore>,
    enabled: Arc<AtomicBool>,
    pin: Arc<Mutex<String>>,
    pair_attempts: Arc<AtomicUsize>,
}

struct RunningServer {
    state: ControllerState,
    shutdown: oneshot::Sender<()>,
    endpoint: String,
    mdns: Option<MdnsHandle>,
}

static SERVER: OnceLock<tokio::sync::Mutex<Option<RunningServer>>> = OnceLock::new();
static START_STOP: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn new_token() -> String { format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple()) }

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    if headers.contains_key(axum::http::header::ORIGIN) { return false; }
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION).and_then(|value|value.to_str().ok()).and_then(|value|value.strip_prefix("Bearer ")) else { return false; };
    value.len() == token.len() && bool::from(value.as_bytes().ct_eq(token.as_bytes()))
}

fn api_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({"error":message.into()}))).into_response()
}

async fn authenticate(State(state): State<ControllerState>, request: Request, next: Next) -> Response {
    if !state.enabled.load(Ordering::SeqCst) { return api_error(StatusCode::SERVICE_UNAVAILABLE,"Controller server is stopped"); }
    let allowed=state.token.lock().map(|token|authorized(request.headers(),&token)).unwrap_or(false);
    if !allowed { return api_error(StatusCode::UNAUTHORIZED,"A valid controller bearer token is required; browser-origin requests are not allowed"); }
    next.run(request).await
}

fn require_scope(state: &ControllerState, control: bool) -> Result<(), Response> {
    if if control { state.config.allow_control } else { state.config.allow_read } { Ok(()) }
    else { Err(api_error(StatusCode::FORBIDDEN,if control {"Control permission is disabled"} else {"Read permission is disabled"})) }
}

async fn health(State(state): State<ControllerState>) -> Json<Value> {
    Json(json!({"service":"Iris Controller","apiVersion":"v1","permissions":{"read":state.config.allow_read,"control":state.config.allow_control},"transport":"loopback HTTP; use a trusted encrypted tunnel for other devices"}))
}

async fn chats(State(state): State<ControllerState>) -> Response {
    if let Err(error)=require_scope(&state,false) { return error; }
    match list_open_tabs(state.app.clone()).await {
        Ok(snapshots)=>Json(json!({"chats":snapshots.into_iter().map(|snapshot|json!({"id":snapshot.tab_id,"title":snapshot.title,"projectId":snapshot.associated_project_id,"updatedAt":snapshot.last_updated})).collect::<Vec<_>>()})).into_response(),
        Err(error)=>api_error(StatusCode::INTERNAL_SERVER_ERROR,error),
    }
}

async fn chat(State(state): State<ControllerState>, ApiPath(tab_id): ApiPath<u32>) -> Response {
    if let Err(error)=require_scope(&state,false) { return error; }
    match read_tab_snapshot(state.app.clone(),ReadTabSnapshotArgs{key:tab_id.to_string()}) {
        Ok(snapshot)=>Json(json!({"chat":snapshot,"task":agent_executor::load_task(&state.app,tab_id).ok().flatten()})).into_response(),
        Err(_)=>api_error(StatusCode::NOT_FOUND,"Chat not found"),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateChat { title: String }

async fn create_chat(State(state): State<ControllerState>, Json(body): Json<CreateChat>) -> Response {
    if let Err(error)=require_scope(&state,true) { return error; }
    let title=body.title.trim();
    if title.is_empty() || title.chars().count()>100 { return api_error(StatusCode::BAD_REQUEST,"Title must contain 1 to 100 characters"); }
    let snapshots=match list_open_tabs(state.app.clone()).await {Ok(value)=>value,Err(error)=>return api_error(StatusCode::INTERNAL_SERVER_ERROR,error)};
    if snapshots.len()>=8 { return api_error(StatusCode::CONFLICT,"The chat limit is reached; close a chat in Iris first"); }
    let directory=match iris_open_tabs_dir(&state.app) {Ok(directory)=>directory,Err(error)=>return api_error(StatusCode::INTERNAL_SERVER_ERROR,error)};
    let tab_id=1_000_000_000 + (uuid::Uuid::new_v4().as_u128() % 1_000_000_000) as u32;
    let path=directory.join(format!("tab_{tab_id}.json"));
    let snapshot=Snapshot { tab_id:Some(tab_id),title:title.into(),last_updated:Some(now_ts()),..Default::default() };
    let result=(||->Result<(),String>{
        use std::io::Write;
        let mut file=fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|error|error.to_string())?;
        file.write_all(serde_json::to_string_pretty(&snapshot).map_err(|error|error.to_string())?.as_bytes()).map_err(|error|error.to_string())
    })();
    match result {
        Ok(())=>{let _=state.app.emit("controller-chat-updated",json!({"tabId":tab_id}));(StatusCode::CREATED,Json(json!({"id":tab_id,"title":title}))).into_response()},
        Err(error)=>api_error(StatusCode::INTERNAL_SERVER_ERROR,error),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct TurnRequest {
    input: String,
    #[serde(default)]
    network_allowed: bool,
    /// Optional attached images as base64 data URLs. The phone resizes/compresses before sending
    /// to keep the Wi-Fi payload small; no raw audio or uncompressed photos cross the network.
    #[serde(default)]
    images: Vec<String>,
}

async fn submit(State(state): State<ControllerState>, ApiPath(tab_id): ApiPath<u32>, Json(body): Json<TurnRequest>) -> Response {
    if let Err(error)=require_scope(&state,true) { return error; }
    if body.input.chars().count()>16000 { return api_error(StatusCode::BAD_REQUEST,"Input must be at most 16000 characters"); }
    if body.input.trim().is_empty() && body.images.is_empty() { return api_error(StatusCode::BAD_REQUEST,"Provide a message or at least one image"); }
    if body.images.len()>6 { return api_error(StatusCode::BAD_REQUEST,"At most 6 images per turn"); }
    if body.images.iter().any(|image|image.len()>8*1024*1024 || !(image.starts_with("data:image/") || image.starts_with("iVBOR") || image.starts_with("/9j/"))) {
        return api_error(StatusCode::BAD_REQUEST,"Images must be base64 (data URL or raw) and under 8 MB each");
    }
    let permit=match state.active.clone().try_acquire_owned() {Ok(permit)=>permit,Err(_)=>return api_error(StatusCode::CONFLICT,"A remote turn is already running")};
    if read_tab_snapshot(state.app.clone(),ReadTabSnapshotArgs{key:tab_id.to_string()}).is_err() { return api_error(StatusCode::NOT_FOUND,"Chat not found"); }
    if agent_executor::is_running(tab_id) { return api_error(StatusCode::CONFLICT,"This chat is already running a turn"); }
    let Some(webview)=state.app.get_webview_window("main") else { return api_error(StatusCode::SERVICE_UNAVAILABLE,"The Iris desktop window is required for approval prompts"); };
    let window=webview.as_ref().window().clone();
    let id=uuid::Uuid::new_v4().to_string();
    let cancelled=Arc::new(AtomicBool::new(false));
    let ready=Arc::new(tokio::sync::Notify::new());
    let job=ControllerJob {id:id.clone(),tab_id,status:"running".into(),error:None,started_at:now_ts(),updated_at:now_ts(),cancelled:cancelled.clone(),ready:ready.clone()};
    if let Ok(mut jobs)=state.jobs.lock() {
        if jobs.len()>=40 {
            if let Some(oldest)=jobs.values().filter(|job|job.status!="running").min_by_key(|job|job.updated_at).map(|job|job.id.clone()) {jobs.remove(&oldest);}
        }
        jobs.insert(id.clone(),job);
    } else { return api_error(StatusCode::INTERNAL_SERVER_ERROR,"Job state unavailable"); }
    let response_id=id.clone();
    tauri::async_runtime::spawn(async move {
        let _permit=permit;
        let _=state.app.emit("controller-turn-started",json!({"tabId":tab_id,"jobId":id,"input":body.input.clone(),"images":body.images.clone()}));
        if tokio::time::timeout(Duration::from_secs(8),ready.notified()).await.is_err() || cancelled.load(Ordering::SeqCst) || !state.enabled.load(Ordering::SeqCst) {
            if let Ok(mut jobs)=state.jobs.lock() {if let Some(job)=jobs.get_mut(&id) {job.status="blocked".into();job.error=Some("The desktop UI did not accept this remote turn, or it was cancelled. No turn was started.".into());job.updated_at=now_ts();}}
            let _=state.app.emit("controller-chat-updated",json!({"tabId":tab_id}));
            return;
        }
        let turn=submit_turn(state.app.clone(),UserTurnPayload {tab_id,input_text:body.input,images:body.images,associated_project_id:None,network_allowed:Some(body.network_allowed),resume_task:false},window);
        tokio::pin!(turn);
        let mut interval=tokio::time::interval(Duration::from_millis(200));
        let result=loop {
            tokio::select! {
                result=&mut turn=>break result,
                _=interval.tick()=>{if cancelled.load(Ordering::SeqCst) || !state.enabled.load(Ordering::SeqCst) {agent_executor::cancel(tab_id);}}
            }
        };
        let task=agent_executor::load_task(&state.app,tab_id).ok().flatten();
        if let Ok(mut jobs)=state.jobs.lock() {
            if let Some(job)=jobs.get_mut(&id) {
                job.status=if cancelled.load(Ordering::SeqCst) {"cancelled".into()} else if result.is_err() {"error".into()} else {task.map(|task|format!("{:?}",task.status).to_lowercase()).unwrap_or_else(||"error".into())};
                job.error=result.err();job.updated_at=now_ts();
            }
        }
        let _=state.app.emit("controller-chat-updated",json!({"tabId":tab_id}));
    });
    (StatusCode::ACCEPTED,Json(json!({"jobId":response_id,"tabId":tab_id,"status":"running","statusUrl":format!("/v1/jobs/{response_id}")}))).into_response()
}

async fn job(State(state): State<ControllerState>, ApiPath(id): ApiPath<String>) -> Response {
    if let Err(error)=require_scope(&state,false) { return error; }
    match state.jobs.lock().ok().and_then(|jobs|jobs.get(&id).cloned()) {
        Some(job)=>Json(json!({"job":job,"task":agent_executor::load_task(&state.app,job.tab_id).ok().flatten()})).into_response(),
        None=>api_error(StatusCode::NOT_FOUND,"Job not found"),
    }
}

async fn cancel_job(State(state): State<ControllerState>, ApiPath(id): ApiPath<String>) -> Response {
    if let Err(error)=require_scope(&state,true) { return error; }
    let job=state.jobs.lock().ok().and_then(|jobs|jobs.get(&id).cloned());
    match job {
        Some(job)=>{job.cancelled.store(true,Ordering::SeqCst);agent_executor::cancel(job.tab_id);Json(json!({"cancelRequested":true})).into_response()},
        None=>api_error(StatusCode::NOT_FOUND,"Job not found"),
    }
}

fn router(state: ControllerState) -> Router {
    let authed = Router::new().route("/v1/health",get(health)).route("/v1/chats",get(chats).post(create_chat))
        .route("/v1/chats/{tab_id}",get(chat)).route("/v1/chats/{tab_id}/turns",post(submit))
        .route("/v1/jobs/{id}",get(job)).route("/v1/jobs/{id}/cancel",post(cancel_job))
        .layer(middleware::from_fn_with_state(state.clone(),authenticate));
    // Public bootstrap routes: discovery ping and PIN pairing (the phone has no token yet).
    let public = Router::new().route("/v1/ping",get(ping)).route("/v1/pair",post(pair));
    authed.merge(public).layer(DefaultBodyLimit::max(16*1024*1024)).with_state(state)
}

/// Unauthenticated discovery probe: lets a freshly-discovered phone confirm this is an Iris host
/// and that a PIN is required, before pairing. Reveals no secrets and rejects browser origins.
async fn ping(State(state): State<ControllerState>, headers: HeaderMap) -> Response {
    if headers.contains_key(axum::http::header::ORIGIN) { return api_error(StatusCode::UNAUTHORIZED,"Browser-origin requests are not allowed"); }
    if !state.enabled.load(Ordering::SeqCst) { return api_error(StatusCode::SERVICE_UNAVAILABLE,"Controller server is stopped"); }
    Json(json!({"service":"Iris Companion","apiVersion":"v1","requiresPin":true,
        "pairingLocked":state.pair_attempts.load(Ordering::SeqCst)>=MAX_PAIR_ATTEMPTS})).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PairRequest { pin: String }

/// Exchange the one-time pairing PIN for the session bearer token. Constant-time comparison and a
/// small attempt budget make LAN brute force infeasible; the PIN can be rotated to unlock.
async fn pair(State(state): State<ControllerState>, headers: HeaderMap, Json(body): Json<PairRequest>) -> Response {
    if headers.contains_key(axum::http::header::ORIGIN) { return api_error(StatusCode::UNAUTHORIZED,"Browser-origin pairing is not allowed"); }
    if !state.enabled.load(Ordering::SeqCst) { return api_error(StatusCode::SERVICE_UNAVAILABLE,"Controller server is stopped"); }
    if !state.config.allow_read { return api_error(StatusCode::FORBIDDEN,"Read access is disabled"); }
    if state.pair_attempts.load(Ordering::SeqCst)>=MAX_PAIR_ATTEMPTS { return api_error(StatusCode::TOO_MANY_REQUESTS,"Pairing is locked after too many attempts. Rotate the PIN in Iris settings."); }
    let expected=state.pin.lock().map(|pin|pin.clone()).unwrap_or_default();
    let candidate=body.pin.trim();
    let matched=!expected.is_empty() && candidate.len()==expected.len() && bool::from(candidate.as_bytes().ct_eq(expected.as_bytes()));
    if !matched {
        state.pair_attempts.fetch_add(1,Ordering::SeqCst);
        return api_error(StatusCode::UNAUTHORIZED,"Incorrect pairing PIN");
    }
    state.pair_attempts.store(0,Ordering::SeqCst);
    let token=state.token.lock().map(|token|token.clone()).unwrap_or_default();
    Json(json!({"token":token,"apiVersion":"v1","permissions":{"read":state.config.allow_read,"control":state.config.allow_control}})).into_response()
}

fn status_value(server: Option<&RunningServer>) -> Value {
    match server {
        Some(server)=>json!({"running":server.state.enabled.load(Ordering::SeqCst),"endpoint":server.endpoint,"config":server.state.config,
            "lanIp":lan_ipv4().map(|ip|ip.to_string()),"discoverable":server.mdns.is_some(),
            "pin":server.state.pin.lock().map(|pin|pin.clone()).ok(),
            "pairingLocked":server.state.pair_attempts.load(Ordering::SeqCst)>=MAX_PAIR_ATTEMPTS,
            "jobs":server.state.jobs.lock().map(|jobs|jobs.values().cloned().collect::<Vec<_>>()).unwrap_or_default(),"apiVersion":"v1"}),
        None=>json!({"running":false,"endpoint":null,"config":{"port":1435,"allowRead":true,"allowControl":false,"bindLan":false,"discoverable":false},
            "lanIp":lan_ipv4().map(|ip|ip.to_string()),"discoverable":false,"pin":null,"pairingLocked":false,"jobs":[],"apiVersion":"v1"}),
    }
}

#[tauri::command]
pub async fn get_controller_status() -> Value { status_value(SERVER.get_or_init(Default::default).lock().await.as_ref()) }

#[tauri::command]
pub async fn start_controller_server(app: tauri::AppHandle, config: ControllerConfig) -> Result<Value,String> {
    let _operation=START_STOP.lock().await;
    if config.port<1024 || !config.allow_read { return Err("Choose port 1024-65535 and enable read access before starting.".into()); }
    if config.discoverable && !config.bind_lan { return Err("Enable LAN access before advertising the companion server for discovery.".into()); }
    let mut server=SERVER.get_or_init(Default::default).lock().await;
    if server.as_ref().is_some_and(|server|server.state.enabled.load(Ordering::SeqCst)) { return Err("Stop the current server before changing its configuration.".into()); }
    let bind_ip=if config.bind_lan { std::net::Ipv4Addr::UNSPECIFIED } else { std::net::Ipv4Addr::LOCALHOST };
    let listener=tokio::net::TcpListener::bind((bind_ip,config.port)).await.map_err(|error|format!("Could not bind controller port: {error}"))?;
    let endpoint=if config.bind_lan {
        format!("http://{}:{}",lan_ipv4().map(|ip|ip.to_string()).unwrap_or_else(||"0.0.0.0".into()),config.port)
    } else { format!("http://127.0.0.1:{}",config.port) };
    let mdns=if config.bind_lan && config.discoverable { start_mdns(config.port) } else { None };
    let state=ControllerState {app,token:Arc::new(Mutex::new(new_token())),config,jobs:Arc::new(Mutex::new(HashMap::new())),active:Arc::new(Semaphore::new(1)),enabled:Arc::new(AtomicBool::new(true)),pin:Arc::new(Mutex::new(new_pin())),pair_attempts:Arc::new(AtomicUsize::new(0))};
    let (shutdown,stopped)=oneshot::channel();
    let serving=state.clone();
    tauri::async_runtime::spawn(async move {
        let _=axum::serve(listener,router(serving.clone())).with_graceful_shutdown(async {let _=stopped.await;}).await;
        serving.enabled.store(false,Ordering::SeqCst);
    });
    *server=Some(RunningServer{state,shutdown,endpoint,mdns});
    Ok(status_value(server.as_ref()))
}

#[tauri::command]
pub async fn stop_controller_server() -> Result<(),String> {
    let _operation=START_STOP.lock().await;
    if let Some(server)=SERVER.get_or_init(Default::default).lock().await.take() {
        server.state.enabled.store(false,Ordering::SeqCst);
        if let Ok(jobs)=server.state.jobs.lock() { for job in jobs.values() {job.cancelled.store(true,Ordering::SeqCst);agent_executor::cancel(job.tab_id);} }
        if let Some(mdns)=server.mdns { mdns.stop(); }
        let _=server.shutdown.send(());
    }
    Ok(())
}

#[tauri::command]
pub async fn controller_frontend_ready(job_id: String) -> Result<(),String> {
    let server=SERVER.get_or_init(Default::default).lock().await;
    let state=&server.as_ref().ok_or("Controller server is stopped")?.state;
    let job=state.jobs.lock().map_err(|_|"Job state unavailable")?.get(&job_id).cloned().ok_or("Job not found")?;
    job.ready.notify_one();
    Ok(())
}

#[tauri::command]
pub async fn get_controller_token() -> Result<String,String> {
    let server=SERVER.get_or_init(Default::default).lock().await;
    server.as_ref().ok_or("Start the controller server first")?.state.token.lock().map(|token|token.clone()).map_err(|_|"Token unavailable".into())
}

#[tauri::command]
pub async fn rotate_controller_token() -> Result<(),String> {
    let server=SERVER.get_or_init(Default::default).lock().await;
    let state=&server.as_ref().ok_or("Start the controller server first")?.state;
    *state.token.lock().map_err(|_|"Token unavailable")?=new_token();
    if let Ok(jobs)=state.jobs.lock() {for job in jobs.values().filter(|job|job.status=="running") {job.cancelled.store(true,Ordering::SeqCst);agent_executor::cancel(job.tab_id);}}
    Ok(())
}

#[tauri::command]
pub async fn rotate_controller_pin() -> Result<String,String> {
    let server=SERVER.get_or_init(Default::default).lock().await;
    let state=&server.as_ref().ok_or("Start the controller server first")?.state;
    let pin=new_pin();
    *state.pin.lock().map_err(|_|"PIN unavailable")?=pin.clone();
    state.pair_attempts.store(0,Ordering::SeqCst);
    Ok(pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_auth_rejects_missing_wrong_and_browser_origin_credentials() {
        let mut headers=HeaderMap::new();
        assert!(!authorized(&headers,"test-token"));
        headers.insert("authorization","Bearer wrong-token".parse().unwrap());
        assert!(!authorized(&headers,"test-token"));
        headers.insert("authorization","Bearer test-token".parse().unwrap());
        assert!(authorized(&headers,"test-token"));
        headers.insert("origin","https://untrusted.example".parse().unwrap());
        assert!(!authorized(&headers,"test-token"));
    }

    #[test]
    fn controller_tokens_rotate_and_requests_default_to_no_network() {
        let first=new_token();let second=new_token();
        assert_eq!(first.len(),64);assert_ne!(first,second);
        let request:TurnRequest=serde_json::from_value(json!({"input":"Hello"})).unwrap();
        assert!(!request.network_allowed);
        assert!(request.images.is_empty());
        let with_images:TurnRequest=serde_json::from_value(json!({"input":"","images":["data:image/jpeg;base64,/9j/abc"]})).unwrap();
        assert_eq!(with_images.images.len(),1);
        assert!(serde_json::from_value::<TurnRequest>(json!({"input":"Hello","approveAll":true})).is_err());
        assert_eq!(status_value(None)["running"],false);
    }

    #[test]
    fn pairing_pins_are_six_digits_and_vary() {
        let a=new_pin();
        assert_eq!(a.len(),6);
        assert!(a.chars().all(|c|c.is_ascii_digit()));
        // Avoid a 1-in-1e6 flake: at least one of two fresh PINs should differ from the first.
        assert!(new_pin()!=a || new_pin()!=a);
    }

    #[test]
    fn config_defaults_keep_lan_and_discovery_off() {
        let config:ControllerConfig=serde_json::from_value(json!({"port":1435,"allowRead":true,"allowControl":false})).unwrap();
        assert!(!config.bind_lan);
        assert!(!config.discoverable);
        // A PIN pair request rejects unknown fields and requires the pin.
        assert!(serde_json::from_value::<PairRequest>(json!({"pin":"123456"})).is_ok());
        assert!(serde_json::from_value::<PairRequest>(json!({"pin":"1","token":"x"})).is_err());
    }

    #[test]
    #[ignore = "Requires LAN multicast; validates mDNS advertising/discovery on-device"]
    fn mdns_service_is_discoverable() {
        use mdns_sd::ServiceEvent;
        let handle=start_mdns(1435).expect("mDNS should start when a LAN interface exists");
        let browser=ServiceDaemon::new().expect("browser daemon");
        let receiver=browser.browse(MDNS_SERVICE_TYPE).expect("browse");
        let deadline=std::time::Instant::now()+Duration::from_secs(8);
        let mut found=false;
        while std::time::Instant::now()<deadline {
            if let Ok(event)=receiver.recv_timeout(Duration::from_millis(500)) {
                match event {
                    ServiceEvent::ServiceResolved(info)=>{ if info.get_fullname().contains("Iris") { found=true; break; } }
                    ServiceEvent::ServiceFound(_,name)=>{ if name.contains("Iris") { found=true; break; } }
                    _=>{}
                }
            }
        }
        let _=browser.shutdown();
        handle.stop();
        assert!(found,"advertised Iris companion service was not discovered over mDNS");
    }
}
use super::*;
use axum::{extract::{DefaultBodyLimit, Path as ApiPath, Request, State}, http::{HeaderMap, StatusCode}, middleware::{self, Next}, response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::{oneshot, Semaphore};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ControllerConfig {
    pub port: u16,
    pub allow_read: bool,
    pub allow_control: bool,
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
}

struct RunningServer {
    state: ControllerState,
    shutdown: oneshot::Sender<()>,
    endpoint: String,
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
}

async fn submit(State(state): State<ControllerState>, ApiPath(tab_id): ApiPath<u32>, Json(body): Json<TurnRequest>) -> Response {
    if let Err(error)=require_scope(&state,true) { return error; }
    if body.input.trim().is_empty() || body.input.chars().count()>16000 { return api_error(StatusCode::BAD_REQUEST,"Input must contain 1 to 16000 characters"); }
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
        let _=state.app.emit("controller-turn-started",json!({"tabId":tab_id,"jobId":id,"input":body.input}));
        if tokio::time::timeout(Duration::from_secs(8),ready.notified()).await.is_err() || cancelled.load(Ordering::SeqCst) || !state.enabled.load(Ordering::SeqCst) {
            if let Ok(mut jobs)=state.jobs.lock() {if let Some(job)=jobs.get_mut(&id) {job.status="blocked".into();job.error=Some("The desktop UI did not accept this remote turn, or it was cancelled. No turn was started.".into());job.updated_at=now_ts();}}
            let _=state.app.emit("controller-chat-updated",json!({"tabId":tab_id}));
            return;
        }
        let turn=submit_turn(state.app.clone(),UserTurnPayload {tab_id,input_text:body.input,images:vec![],associated_project_id:None,network_allowed:Some(body.network_allowed),resume_task:false},window);
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
    Router::new().route("/v1/health",get(health)).route("/v1/chats",get(chats).post(create_chat))
        .route("/v1/chats/{tab_id}",get(chat)).route("/v1/chats/{tab_id}/turns",post(submit))
        .route("/v1/jobs/{id}",get(job)).route("/v1/jobs/{id}/cancel",post(cancel_job))
        .layer(DefaultBodyLimit::max(64*1024)).layer(middleware::from_fn_with_state(state.clone(),authenticate)).with_state(state)
}

fn status_value(server: Option<&RunningServer>) -> Value {
    match server {
        Some(server)=>json!({"running":server.state.enabled.load(Ordering::SeqCst),"endpoint":server.endpoint,"config":server.state.config,
            "jobs":server.state.jobs.lock().map(|jobs|jobs.values().cloned().collect::<Vec<_>>()).unwrap_or_default(),"apiVersion":"v1"}),
        None=>json!({"running":false,"endpoint":null,"config":{"port":1435,"allowRead":true,"allowControl":false},"jobs":[],"apiVersion":"v1"}),
    }
}

#[tauri::command]
pub async fn get_controller_status() -> Value { status_value(SERVER.get_or_init(Default::default).lock().await.as_ref()) }

#[tauri::command]
pub async fn start_controller_server(app: tauri::AppHandle, config: ControllerConfig) -> Result<Value,String> {
    let _operation=START_STOP.lock().await;
    if config.port<1024 || !config.allow_read { return Err("Choose port 1024-65535 and enable read access before starting.".into()); }
    let mut server=SERVER.get_or_init(Default::default).lock().await;
    if server.as_ref().is_some_and(|server|server.state.enabled.load(Ordering::SeqCst)) { return Err("Stop the current server before changing its configuration.".into()); }
    let listener=tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST,config.port)).await.map_err(|error|format!("Could not bind controller port: {error}"))?;
    let endpoint=format!("http://127.0.0.1:{}",config.port);
    let state=ControllerState {app,token:Arc::new(Mutex::new(new_token())),config,jobs:Arc::new(Mutex::new(HashMap::new())),active:Arc::new(Semaphore::new(1)),enabled:Arc::new(AtomicBool::new(true))};
    let (shutdown,stopped)=oneshot::channel();
    let serving=state.clone();
    tauri::async_runtime::spawn(async move {
        let _=axum::serve(listener,router(serving.clone())).with_graceful_shutdown(async {let _=stopped.await;}).await;
        serving.enabled.store(false,Ordering::SeqCst);
    });
    *server=Some(RunningServer{state,shutdown,endpoint});
    Ok(status_value(server.as_ref()))
}

#[tauri::command]
pub async fn stop_controller_server() -> Result<(),String> {
    let _operation=START_STOP.lock().await;
    if let Some(server)=SERVER.get_or_init(Default::default).lock().await.take() {
        server.state.enabled.store(false,Ordering::SeqCst);
        if let Ok(jobs)=server.state.jobs.lock() { for job in jobs.values() {job.cancelled.store(true,Ordering::SeqCst);agent_executor::cancel(job.tab_id);} }
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
        assert!(serde_json::from_value::<TurnRequest>(json!({"input":"Hello","approveAll":true})).is_err());
        assert_eq!(status_value(None)["running"],false);
    }
}
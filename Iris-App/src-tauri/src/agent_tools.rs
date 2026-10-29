use super::*;

pub(super) fn catalog(flags: &SetupFlags) -> Vec<Value> {
    let mut tools = vec![schema("project_connections", "List configured project and connection ids; not proof of connectivity.", serde_json::json!({}), &[])];
    if flags.mcp_enabled {
        tools.push(schema("list_project_tools", "Discover exact MCP schemas. Page with offset or filter by query.", serde_json::json!({"connection_id":{"type":"string"},"query":{"type":"string"},"offset":{"type":"integer","minimum":0}}), &["connection_id"]));
        tools.push(schema("call_project_tool", "Call exact MCP schema. effect classifies unannotated tools; unknown reads still require approval.", serde_json::json!({"connection_id":{"type":"string"},"tool_name":{"type":"string"},"arguments":{"type":"object"},"effect":{"enum":["read","write"]}}), &["connection_id","tool_name","arguments","effect"]));
        tools.push(schema("ssh_action", "Use linked SSH within its root. Mutation/exec needs approval.", serde_json::json!({"connection_id":{"type":"string"},"action":{"enum":["list","read","write","mkdir","move","delete","exec"]},"arguments":{"type":"object"}}), &["connection_id","action","arguments"]));
    }
    if flags.repos_enabled {
        tools.push(schema("project_files", "List/search all scoped files with pagination; read line ranges; write or exact replace with approval.", serde_json::json!({
            "repo_id":{"type":"string"},"action":{"enum":["list","search","read","write","replace","mkdir","move","delete"]},
            "path":{"type":"string"},"query":{"type":"string"},"extension":{"type":"string"},"offset":{"type":"integer","minimum":0},
            "start_line":{"type":"integer","minimum":1},"end_line":{"type":"integer","minimum":1},"content":{"type":"string"},
            "old_text":{"type":"string"},"new_text":{"type":"string"},"to":{"type":"string"},"overwrite":{"type":"boolean"}
        }), &["repo_id","action"]));
        if flags.network_enabled {
            tools.push(schema("clone_repository", "Clone HTTPS repo into a linked root after approval; never runs repo code.", serde_json::json!({"repo_id":{"type":"string"},"url":{"type":"string"}}), &["repo_id","url"]));
        }
    }
    if flags.network_enabled {
        tools.push(schema("read_web_page", "Fetch HTTP(S) page text with source URL; offset reads long documents.", serde_json::json!({"url":{"type":"string"},"offset":{"type":"integer","minimum":0}}), &["url"]));
    }
    if flags.desktop_tools_enabled {
        tools.push(schema("desktop_action", "Inspect windows or launch/focus an application. Focus/launch requires approval.", serde_json::json!({"action":{"enum":["list","focus","launch"]},"target":{"type":"string"},"args":{"type":"array","items":{"type":"string"}}}), &["action"]));
    }
    tools
}

pub(super) fn handles(name: &str) -> bool {
    matches!(name,"project_connections"|"list_project_tools"|"call_project_tool"|"project_files"|"ssh_action"|"clone_repository"|"read_web_page"|"desktop_action")
}

fn schema(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    serde_json::json!({"type":"function","function":{"name":name,"description":description,"parameters":{
        "type":"object","properties":properties,"required":required,"additionalProperties":false
    }}})
}

fn string_arg<'value>(args: &'value Value, key: &str) -> Result<&'value str, String> {
    args.get(key).and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| format!("'{key}' is required"))
}

fn project_for_task(app: &tauri::AppHandle, task: &AgentTask) -> Result<(ProjectDef, RepoProjectStore), String> {
    let store = read_repo_project_store(app);
    let project = store.projects.iter().find(|project| project.enabled && Some(&project.id) == task.project_id.as_ref())
        .cloned().ok_or("This tab has no enabled linked project. Link it in Projects before project actions.")?;
    Ok((project, store))
}

fn linked_mcp(app: &tauri::AppHandle, task: &AgentTask, id: &str) -> Result<McpConnection, String> {
    let (project, store) = project_for_task(app, task)?;
    store.mcps.into_iter().find(|connection| connection.enabled && connection.id == id && project.mcp_ids.contains(&connection.id))
        .ok_or("MCP connection is not enabled and linked to this project".into())
}

fn linked_root(app: &tauri::AppHandle, task: &AgentTask, id: &str) -> Result<String, String> {
    let (project, store) = project_for_task(app, task)?;
    if id == "project" && !project.manipulation_root_path.trim().is_empty() { return Ok(project.manipulation_root_path); }
    store.repos.into_iter().find(|repo| repo.enabled && repo.id == id && project.repo_ids.contains(&repo.id)).map(|repo| repo.path)
        .ok_or("Repository is not enabled and linked to this project".into())
}

async fn blocking_call<T: Send + 'static>(operation: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::time::timeout(Duration::from_secs(90), tokio::task::spawn_blocking(operation)).await
        .map_err(|_| "External action timed out; outcome is unknown. Inspect state before retrying.".to_string())?
        .map_err(|err| err.to_string())?
}

fn paged_text(text: &str, offset: usize, limit: usize) -> Value {
    let total = text.chars().count();
    serde_json::json!({"content":text.chars().skip(offset).take(limit).collect::<String>(),"offset":offset,"totalChars":total,
        "complete":offset.saturating_add(limit)>=total,"nextOffset":if offset.saturating_add(limit)<total {Some(offset+limit)} else {None}})
}

fn local_files(root: &str, args: &Value) -> Result<Value, String> {
    let action = string_arg(args, "action")?;
    let relative = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let path = resolve_within_root(root, relative)?;
    match action {
        "list" | "search" => {
            let extension = args.get("extension").and_then(Value::as_str).unwrap_or("").trim_start_matches('.');
            let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_lowercase();
            if action=="search" && query.is_empty() { return Err("search requires a nonempty literal query".into()); }
            let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
            let mut matches: Vec<Value> = Vec::new();
            let mut errors: Vec<String> = Vec::new();
            let root_path = fs::canonicalize(root).map_err(|err| err.to_string())?;
            let mut scanned = 0usize;
            let mut capped = false;
            for entry in ignore::WalkBuilder::new(&path).hidden(false).follow_links(false)
                .filter_entry(|entry| !matches!(entry.file_name().to_str(),Some(".git"|".godot"|"node_modules"|"target")))
                .sort_by_file_path(|left,right| left.cmp(right)).build() {
                let entry = match entry { Ok(entry) => entry, Err(err) => { errors.push(err.to_string()); continue; } };
                scanned += 1;
                if scanned > 100_000 { capped = true; break; }
                if entry.file_type().is_none_or(|kind| !kind.is_file()) { continue; }
                let file = entry.path();
                let relative = file.strip_prefix(&root_path).unwrap_or(file).to_string_lossy().replace('\\',"/");
                if !extension.is_empty() && !file.extension().and_then(|value| value.to_str()).is_some_and(|value| value.eq_ignore_ascii_case(extension)) { continue; }
                if action == "list" {
                    if query.is_empty() || relative.to_lowercase().contains(&query) { matches.push(serde_json::json!({"path":relative})); }
                } else if is_text_like_file(file) && file.metadata().map(|meta| meta.len() <= 2_000_000).unwrap_or(false) {
                    match fs::read_to_string(file) {
                        Ok(text) => for (index,line) in text.lines().enumerate() {
                            if line.to_lowercase().contains(&query) {
                                matches.push(serde_json::json!({"path":relative,"line":index+1,"text":excerpt(line,500)}));
                                if matches.len() >= 20_000 { capped = true; break; }
                            }
                        },
                        Err(err) => errors.push(format!("{relative}: {err}")),
                    }
                }
                if capped { break; }
            }
            let total = matches.len();
            let page: Vec<_> = matches.into_iter().skip(offset).take(80).collect();
            Ok(serde_json::json!({"entries":page,"totalMatches":total,"inventoryComplete":!capped && errors.is_empty(),
                "scope":"Gitignore-aware files within linked root; excludes generated dependency/build directories, symlinks; content search reads text files <= 2 MB", "errors":errors,
                "nextOffset":if offset.saturating_add(80)<total {Some(offset+80)} else {None}}))
        }
        "read" => {
            if fs::metadata(&path).map_err(|err| err.to_string())?.len()>4_000_000 { return Err("File exceeds 4 MB text-read limit".into()); }
            let text = fs::read_to_string(&path).map_err(|err| err.to_string())?;
            let start = args.get("start_line").and_then(Value::as_u64).unwrap_or(1).max(1) as usize;
            let end = args.get("end_line").and_then(Value::as_u64).unwrap_or((start+159) as u64) as usize;
            if end < start { return Err("end_line must be >= start_line".into()); }
            let lines: Vec<_> = text.lines().collect();
            let actual_end = end.min(start+299).min(lines.len());
            let content = lines.iter().enumerate().skip(start-1).take(actual_end.saturating_sub(start-1))
                .map(|(index,line)| format!("{}: {}",index+1,line)).collect::<Vec<_>>().join("\n");
            Ok(serde_json::json!({"path":relative,"startLine":start,"endLine":actual_end,"totalLines":lines.len(),"complete":start==1 && actual_end==lines.len(),"content":content}))
        }
        "write" => {
            if path.exists() && !args.get("overwrite").and_then(Value::as_bool).unwrap_or(false) { return Err("File exists. Read it and prefer exact replacement, or explicitly set overwrite.".into()); }
            atomic_write_text(&path, string_arg(args,"content")?)?;
            Ok(serde_json::json!({"saved":relative,"requiresReadback":true}))
        }
        "replace" => {
            let original = fs::read_to_string(&path).map_err(|err| err.to_string())?;
            let old = string_arg(args,"old_text")?;
            let replacement = args.get("new_text").and_then(Value::as_str).ok_or("new_text is required")?;
            if original.matches(old).count()!=1 { return Err("old_text must occur exactly once; read the file and use exact surrounding context".into()); }
            atomic_write_text(&path,&original.replacen(old,replacement,1))?;
            Ok(serde_json::json!({"saved":relative,"requiresReadback":true}))
        }
        "mkdir" => { fs::create_dir_all(path).map_err(|err| err.to_string())?; Ok(serde_json::json!({"created":relative})) }
        "move" => { let to = resolve_within_root(root,string_arg(args,"to")?)?; if to.exists() {return Err("Move target already exists".into());} fs::rename(path,to).map_err(|err| err.to_string())?; Ok(serde_json::json!({"moved":relative})) }
        "delete" => { if path==fs::canonicalize(root).map_err(|err|err.to_string())? {return Err("Cannot delete the linked root".into());} if path.is_dir() { fs::remove_dir(path) } else { fs::remove_file(path) }.map_err(|err|err.to_string())?; Ok(serde_json::json!({"deleted":relative})) }
        _ => Err("Unsupported local file action".into()),
    }
}

async fn read_web(url: &str, offset: usize) -> Result<Value, String> {
    let parsed = reqwest::Url::parse(url).map_err(|err| err.to_string())?;
    if !matches!(parsed.scheme(),"https"|"http") || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Only HTTP(S) URLs without embedded credentials are supported".into());
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(25)).redirect(reqwest::redirect::Policy::limited(5)).build().map_err(|err|err.to_string())?;
    let response = client.get(parsed).header("User-Agent","Iris-local-agent/0.3").send().await.map_err(|err|err.to_string())?.error_for_status().map_err(|err|err.to_string())?;
    let final_url = response.url().to_string();
    let content_type = response.headers().get(reqwest::header::CONTENT_TYPE).and_then(|value|value.to_str().ok()).unwrap_or("text/html").to_string();
    if !content_type.starts_with("text/") && !content_type.contains("json") && !content_type.contains("xhtml") {
        return Err(format!("Unsupported page content type: {content_type}. Use an HTML or text source."));
    }
    let body = read_bounded_network_text(response, 2_000_000).await?;
    extract_web_content(&body, &content_type, &final_url, offset)
}

fn extract_web_content(body: &str, content_type: &str, url: &str, offset: usize) -> Result<Value, String> {
    let (text, title, published, links) = if content_type.contains("html") {
        let html = Html::parse_document(body);
        let main_selector=Selector::parse("main, [role=main]").unwrap();
        let article_selector=Selector::parse("article").unwrap();
        let body_selector=Selector::parse("body").unwrap();
        let mut articles=html.select(&article_selector);
        let single_article=articles.next().filter(|_|articles.next().is_none());
        let root=html.select(&main_selector).next().or(single_article).or_else(||html.select(&body_selector).next()).unwrap_or_else(||html.root_element());
        let text = root.descendants().filter_map(|node| {
            let scraper::Node::Text(text) = node.value() else { return None; };
            let hidden = node.ancestors().filter_map(scraper::ElementRef::wrap).any(|ancestor| {
                matches!(ancestor.value().name(), "script"|"style"|"noscript"|"nav"|"header"|"footer"|"aside"|"form"|"svg")
                    || ancestor.value().attr("hidden").is_some() || ancestor.value().attr("aria-hidden") == Some("true")
            });
            if hidden { None } else { Some(text.text.to_string()) }
        }).collect::<Vec<_>>().join(" ");
        let title = html.select(&Selector::parse("title").unwrap()).next().map(|node|node.text().collect::<String>()).unwrap_or_default();
        let published = html.select(&Selector::parse("meta[property='article:published_time'], meta[name='date'], meta[itemprop='datePublished']").unwrap())
            .find_map(|node|node.value().attr("content").or_else(||node.value().attr("datetime"))).map(str::to_string);
        let mut seen=HashSet::new();
        let base=reqwest::Url::parse(url).ok();
        let links=root.select(&Selector::parse("a[href]").unwrap()).filter_map(|anchor| {
            if anchor.ancestors().filter_map(scraper::ElementRef::wrap).any(|ancestor|matches!(ancestor.value().name(),"nav"|"footer"|"header"|"aside")) {return None;}
            let destination=base.as_ref()?.join(anchor.value().attr("href")?).ok()?;
            let title=anchor.text().collect::<Vec<_>>().join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            if !matches!(destination.scheme(),"http"|"https") || !destination.username().is_empty() || destination.password().is_some() || title.len()<8 || !seen.insert(destination.to_string()) {return None;}
            Some(json!({"title":excerpt(&title,180),"url":destination.to_string(),"snippet":"Link from retrieved source page"}))
        }).take(24).collect::<Vec<_>>();
        (text, title, published, links)
    } else { (body.to_string(), String::new(), None, Vec::new()) };
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() { return Err("The page has no readable text; it may require JavaScript or block automated access. Try another source.".into()); }
    Ok(serde_json::json!({"url":url,"title":excerpt(&title,200),"publishedAt":published,"retrievedAt":now_ts(),"evidenceKind":"page_text","page":paged_text(&normalized,offset,6000),"links":links}))
}

pub(super) fn requires_plan(tool: &str, args: &Value) -> bool {
    match tool {
        "project_files" => !matches!(args["action"].as_str(), Some("list" | "search" | "read")),
        "ssh_action" => !matches!(args["action"].as_str(), Some("list" | "read")),
        "desktop_action" => args["action"].as_str() != Some("list"),
        "call_project_tool" => args["effect"].as_str() != Some("read"),
        "clone_repository" => true,
        _ => false,
    }
}

pub(super) async fn execute(
    app: &tauri::AppHandle, window: &tauri::Window, event_name: &str, control: &RunControl,
    task: &AgentTask, tool: &str, args: &Value, policy_read_only: bool,
) -> Result<AgentToolResult, String> {
    if requires_plan(tool,args) && task.steps.is_empty() {
        return Err("Plan small steps with verifiable acceptance criteria before project/desktop work".into());
    }
    let mut attempted_mutation = false;
    let mut verifies_state = !matches!(tool,"project_connections"|"list_project_tools");
    let result: Result<Value,String> = async { Ok(match tool {
        "project_connections" => {
            let (project,store) = project_for_task(app,task)?;
            let mcps = store.mcps.iter().filter(|item| item.enabled && project.mcp_ids.contains(&item.id)).map(|item|serde_json::json!({"id":item.id,"name":item.name})).collect::<Vec<_>>();
            let sshs = store.sshs.iter().filter(|item| item.enabled && project.ssh_ids.contains(&item.id)).map(|item|serde_json::json!({"id":item.id,"name":item.name,"root":item.remote_root})).collect::<Vec<_>>();
            let repositories = store.repos.iter().filter(|item| item.enabled && project.repo_ids.contains(&item.id)).map(|item|serde_json::json!({"id":item.id,"name":item.name})).chain((!project.manipulation_root_path.is_empty()).then(||serde_json::json!({"id":"project","name":"Project root"}))).collect::<Vec<_>>();
            let can_bridge = !mcps.is_empty();
            let can_files = !repositories.is_empty();
            let guidance = if can_bridge {
                "Use list_project_tools then call_project_tool on a linked MCP id to inspect or edit scenes/nodes. Report only what a successful call returns; never invent scene, node, or file names."
            } else if can_files {
                "No MCP bridge is linked, but a repository/project root is available: use project_files to inspect files. Report only what it returns; do not invent names."
            } else {
                "This project has no enabled, linked MCP bridge or repository. You cannot inspect its scenes, nodes, or files. Tell the user to enable and link an MCP bridge or repository in Settings > Projects, and do not invent any scene, node, or file names."
            };
            serde_json::json!({"project":{"id":project.id,"name":project.name,"description":project.description,"cachedEngineVersion":project.godot_version_cached,"cacheIsHintNotFreshInspection":true},
                "mcps":mcps,"sshs":sshs,"repositories":repositories,
                "inspectable":can_bridge||can_files,"guidance":guidance})
        }
        "list_project_tools" | "call_project_tool" => {
            let connection = linked_mcp(app,task,string_arg(args,"connection_id")?)?;
            let catalog = mcp_list_tools(connection.id.clone(),connection.target.clone(),connection.connection_type.clone(),connection.launch_command.clone(),connection.launch_args.clone()).await?;
            if tool=="list_project_tools" {
                let query = args.get("query").and_then(Value::as_str).unwrap_or("").to_lowercase();
                let filtered:Vec<_> = catalog.into_iter().filter(|entry| query.is_empty() || format!("{} {}",entry.name,entry.description).to_lowercase().contains(&query)).collect();
                let total = filtered.len();
                let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
                serde_json::json!({"tools":filtered.into_iter().skip(offset).take(3).collect::<Vec<_>>(),"totalTools":total,"nextOffset":if offset+3<total {Some(offset+3)}else{None}})
            } else {
                let name = string_arg(args,"tool_name")?.to_string();
                let entry = catalog.iter().find(|entry|entry.name==name).ok_or("Tool is not advertised by this MCP server")?;
                let arguments = args.get("arguments").cloned().ok_or("arguments is required")?;
                let validator = jsonschema::validator_for(&entry.input_schema).map_err(|err|format!("Server schema is invalid: {err}"))?;
                if let Err(err) = validator.validate(&arguments) { return Err(format!("MCP arguments failed validation: {err}. Correct them; no call was made.")); }
                let read_only_hint = entry.annotations.get("readOnlyHint").and_then(Value::as_bool);
                let read_only = read_only_hint.unwrap_or(args.get("effect").and_then(Value::as_str)==Some("read"));
                if !read_only && task.steps.is_empty() { return Err("This MCP operation changes state. Create a small verifiable plan before executing it.".into()); }
                if policy_read_only && read_only_hint != Some(true) {return Err("Read-only policy requires an explicitly read-only MCP capability".into());}
                verifies_state = read_only;
                if read_only_hint != Some(true) {
                    approve(window,event_name,control,&format!("{} MCP {} / {}",if read_only {"Read-only request:"}else{"Change request:"},connection.name,name),&arguments.to_string()).await?;
                    attempted_mutation = !read_only;
                }
                let kill_id = connection.id.clone();
                let operation = blocking_call(move||mcp_call_tool(connection.id,connection.target,connection.connection_type,connection.launch_command,connection.launch_args,name,arguments));
                let response = tokio::select! {
                    _ = control.cancelled() => { kill_mcp_by_pid_if_known(&kill_id); Err("Cancelled MCP call; inspect target state before retrying".into()) },
                    response = operation => response,
                };
                if response.is_err() { kill_mcp_by_pid_if_known(&kill_id); }
                response?
            }
        }
        "project_files" => {
            let root = linked_root(app,task,string_arg(args,"repo_id")?)?;
            let action = string_arg(args,"action")?;
            verifies_state = matches!(action,"list"|"search"|"read");
            if !verifies_state {
                if args["repo_id"].as_str()!=Some("project") {return Err("Reference repositories are read-only. Set a project manipulation root to enable approved edits.".into());}
                if policy_read_only {return Err("This turn is read-only; repository changes are disabled".into());}
                approve(window,event_name,control,&format!("Local repository {action}"),&args.to_string()).await?;
                attempted_mutation = true;
            }
            let arguments = args.clone();
            blocking_call(move||local_files(&root,&arguments)).await?
        }
        "ssh_action" => {
            let (project,store) = project_for_task(app,task)?;
            let id = string_arg(args,"connection_id")?;
            let connection = store.sshs.into_iter().find(|connection|connection.enabled && connection.id==id && project.ssh_ids.contains(&connection.id)).ok_or("SSH connection is not linked")?;
            let action = string_arg(args,"action")?.to_string();
            let arguments = args.get("arguments").cloned().ok_or("arguments is required")?;
            verifies_state = matches!(action.as_str(),"list"|"read");
            if !verifies_state {
                if policy_read_only {return Err("This turn is read-only; SSH changes are disabled".into());}
                approve(window,event_name,control,&format!("SSH {} / {}",connection.name,action),&arguments.to_string()).await?;
                attempted_mutation = true;
            }
            blocking_call(move||ssh_tool_call(connection,action,arguments)).await?
        }
        "clone_repository" => {
            if args["repo_id"].as_str()!=Some("project") {return Err("Clone only into the configured project manipulation root".into());}
            if policy_read_only {return Err("This turn is read-only; cloning is disabled".into());}
            let root = linked_root(app,task,string_arg(args,"repo_id")?)?;
            let url = string_arg(args,"url")?.to_string();
            let parsed = reqwest::Url::parse(&url).map_err(|err|err.to_string())?;
            if parsed.scheme()!="https" || !parsed.username().is_empty() || parsed.password().is_some() { return Err("Use HTTPS without embedded credentials".into()); }
            approve(window,event_name,control,"Clone repository",&format!("{url} into {root}")).await?;
            attempted_mutation = true;
            verifies_state = false;
            serde_json::json!({"result":blocking_call(move||clone_repo_into_folder(url,root)).await?})
        }
        "read_web_page" => read_web(string_arg(args,"url")?,args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize).await?,
        "desktop_action" => {
            let action = string_arg(args,"action")?;
            if action=="list" { serde_json::to_value(blocking_call(list_windows).await?).map_err(|err|err.to_string())? }
            else {
                if policy_read_only {return Err("This turn is read-only; desktop actions are disabled".into());}
                let target = string_arg(args,"target")?.to_string();
                approve(window,event_name,control,&format!("Desktop {action}"),&args.to_string()).await?;
                attempted_mutation = true;
                verifies_state = false;
                let arguments = args.get("args").and_then(Value::as_array).map(|items|items.iter().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>());
                serde_json::json!({"result":if action=="focus" {blocking_call(move||focus_window_by_title(target)).await?} else {blocking_call(move||launch_desktop_item(target,arguments)).await?}})
            }
        }
        _ => return Err("Unsupported project tool".into()),
    }) }.await;
    let (value,ok) = match result {
        Ok(value) => { let ok = !mcp_response_has_error(&value); (value,ok) },
        Err(error) => (serde_json::json!({"error":error,"outcomeUnknown":attempted_mutation}),false),
    };
    Ok(AgentToolResult { value, ok, verifies_state, side_effect_attempted: attempted_mutation })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_can_inform_a_plan_but_mutations_still_require_one() {
        for (tool,args) in [
            ("list_project_tools",json!({"connection_id":"godot"})),
            ("project_files",json!({"action":"read"})),
            ("project_files",json!({"action":"search"})),
            ("call_project_tool",json!({"effect":"read"})),
            ("ssh_action",json!({"action":"list"})),
            ("desktop_action",json!({"action":"list"})),
        ] { assert!(!requires_plan(tool,&args),"{tool}"); }
        assert!(requires_plan("project_files",&json!({"action":"write"})));
        assert!(requires_plan("call_project_tool",&json!({"effect":"write"})));
        assert!(requires_plan("ssh_action",&json!({"action":"exec"})));
    }

    #[test]
    fn web_content_prefers_article_and_excludes_hidden_scripts() {
        let page = extract_web_content("<html><head><title>Release notes</title><meta property='article:published_time' content='2026-10-27'></head><body><nav>Navigation noise</nav><article><h1>New series</h1><p>Announced, not yet released.</p><script>hidden instructions</script><div hidden>hidden text</div></article><footer>Legal boilerplate</footer></body></html>", "text/html", "https://example.com/series", 0).unwrap();
        assert_eq!(page["page"]["content"], "New series Announced, not yet released.");
        assert_eq!(page["title"], "Release notes");
        assert_eq!(page["publishedAt"], "2026-10-27");
        assert!(extract_web_content("", "text/plain", "source", 0).is_err());
    }

    #[test]
    fn news_indexes_keep_all_main_articles_and_followable_links() {
        let page=extract_web_content("<main><article><a href='/first'>First dated report</a></article><article><a href='/second'>Second dated report</a></article></main>","text/html","https://example.com/news",0).unwrap();
        assert!(page["page"]["content"].as_str().unwrap().contains("Second dated report"));
        assert_eq!(page["links"][1]["url"],"https://example.com/second");
    }

    #[test]
    fn catalog_schemas_reject_extra_and_missing_fields() {
        for tool in catalog(&SetupFlags::default()) {
            let validator = jsonschema::validator_for(&tool["function"]["parameters"]).unwrap();
            assert!(!validator.is_valid(&serde_json::json!({"invented":true})));
        }
    }

    #[test]
    fn file_inventory_counts_all_scenes_and_reads_ranges() {
        let root = std::env::temp_dir().join(format!("iris_inventory_{}_{}",std::process::id(),NEXT_ID.fetch_add(1,Ordering::Relaxed)));
        fs::create_dir_all(root.join("Scenes")).unwrap();
        for index in 0..6 { fs::write(root.join(format!("Scenes/scene{index}.tscn")), "first\nsecond\nthird").unwrap(); }
        fs::write(root.join("icon.svg"),"icon").unwrap();
        let root_string = root.to_string_lossy();
        let inventory = local_files(&root_string,&serde_json::json!({"action":"list","extension":"tscn"})).unwrap();
        assert_eq!(inventory["totalMatches"],6);
        assert_eq!(inventory["inventoryComplete"],true);
        let read = local_files(&root_string,&serde_json::json!({"action":"read","path":"Scenes/scene0.tscn","start_line":2,"end_line":2})).unwrap();
        assert_eq!(read["content"],"2: second");
        assert_eq!(read["complete"],false);
        assert!(local_files(&root_string,&serde_json::json!({"action":"read","path":"../outside.txt"})).is_err());
        assert!(local_files(&root_string,&serde_json::json!({"action":"write","path":"Scenes/scene0.tscn","content":"oops"})).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tool_error_detection_does_not_reject_source_code() {
        assert!(!mcp_response_has_error(&serde_json::json!({"content":[{"type":"text","text":"if error: print('failed')"}]})));
        assert!(mcp_response_has_error(&serde_json::json!({"isError":true,"content":[]})));
        assert!(mcp_response_has_error(&serde_json::json!({"content":[{"type":"text","text":"{\"ok\":false}"}]})));
    }
}
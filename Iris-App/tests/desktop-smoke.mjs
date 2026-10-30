import assert from "node:assert/strict";

const tabIndex = process.argv.indexOf("--tab");
const tabId = Number(process.argv[tabIndex + 1]);
if (tabIndex < 0 || !Number.isInteger(tabId) || tabId < 1) {
  throw new Error("Supply --tab <id> for a disposable test tab. Start WebView2 with --remote-debugging-port=9225.");
}
const toolTest = process.argv.includes("--tool");
const networkTest = process.argv.includes("--network");
const weatherTest = process.argv.includes("--weather");
const serviceTest = process.argv.includes("--services");
const controllerTest = process.argv.includes("--controller");
const resumeTest = process.argv.includes("--resume");
const promptIndex = process.argv.indexOf("--prompt");
const prompt = promptIndex >= 0 ? process.argv[promptIndex + 1] : "";
const fresh = process.argv.includes("--fresh");
const targets = await (await fetch("http://127.0.0.1:9225/json/list")).json();
const target = targets.find((entry) => entry.url.startsWith("http://localhost:1420"));
if (!target) throw new Error("Native Iris test view not found");
const socket = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((resolve, reject) => {
  socket.addEventListener("open", resolve, { once: true });
  socket.addEventListener("error", reject, { once: true });
});

async function nativeTurn({ tabId: testTab, toolTest: useTool, networkTest: useNetwork, weatherTest: useWeather, serviceTest: checkServices, fresh: createFresh, prompt: customPrompt, resumeTest: testResume }) {
  const invoke = window.__TAURI__.core.invoke;
  if (checkServices) {
    return { server: await invoke("get_server_status"), search: await invoke("test_network_search", { query: "newest Adventure Time series official announcement" }), weather: await invoke("weather_lookup", { location: "Durham, NC", dayOffset: 0 }) };
  }
  const snapshots = await invoke("list_open_tabs");
  if (!snapshots.some((snapshot) => snapshot.tab_id === testTab)) throw new Error("Test tab does not exist");
  if (createFresh) {
    testTab = Math.max(...snapshots.map((snapshot) => snapshot.tab_id)) + 1;
    await invoke("update_snapshot_memory", { args: { tab_id:testTab, snapshot: {
      tab_id:testTab,title:"Iris smoke test",messages:[],associatedProjectId:null,microSummary:"",dialogueBullets:"",summary:"",artifacts:[],promptHistory:[],last_updated:Math.floor(Date.now()/1000),
    } } });
  }
  const started = performance.now();
  let deltas = 0;
  let firstDeltaMs = null;
  let permissions = 0;
  const unlisten = await window.__TAURI__.event.listen(`iris_event_${testTab}`, ({ payload }) => {
    if (payload.type === "Delta") {
      deltas++;
      firstDeltaMs ??= Math.round(performance.now() - started);
    }
    if (payload.type === "PermissionRequest") permissions++;
  });
  try {
    await invoke("submit_turn", { payload: {
      tab_id: testTab,
      input_text: customPrompt || (useNetwork ? "Can you search online to see what the newest Adventure Time series is called? Distinguish released and announced shows, and cite the source."
        : useWeather ? "What is the current weather outside in Durham, NC? Tell me the current temperature, not today's high, with its observation time and source."
        : useTool ? "Use the perform_arithmetic tool to calculate 37 times 19. Then tell me only the result." : "Who are you and what is your purpose? Answer briefly."),
      images: [], associated_project_id: null, network_allowed: useNetwork || useWeather,
    } });
    const task = await invoke("get_agent_task", { tabId: testTab });
    let resumed = null;
    if (testResume && task.status !== "completed") {
      await invoke("submit_turn", {payload:{tab_id:testTab,input_text:"Continue the saved task from its notes; inspect uncertain state before retrying.",images:[],associated_project_id:null,network_allowed:useNetwork||useWeather,resume_task:true}});
      const after=await invoke("get_agent_task", {tabId:testTab});
      resumed={sameTask:after.id===task.id,sameGoal:after.goal===task.goal,actionsAdded:after.totalActions-task.totalActions,status:after.status};
    }
    const proficiency = useNetwork || useWeather ? await invoke("get_agent_proficiency") : null;
    return {
      id: task.id, status: task.status, actions: task.totalActions, steps: task.steps.length,
      deltas, firstDeltaMs, permissions,
      tools: task.observations.map((entry) => ({ tool: entry.tool, ok: entry.ok, summary: entry.summary })),
      answer: task.lastMessage,
      resumed,
      research: proficiency?.research?.find((report) => report.taskId === task.id),
    };
  } finally {
    unlisten();
    if (createFresh) {
      const snapshot = await invoke("read_tab_snapshot", { args: { key:String(testTab) } });
      await invoke("close_tab_and_snapshot", { tabId:testTab,snapshot });
    }
  }
}

let nextMessageId = 0;
async function evaluate(expression) {
  const id = ++nextMessageId;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("Native smoke test timed out")), networkTest || weatherTest ? 360000 : 120000);
    const handler = ({ data }) => {
      const value = JSON.parse(data);
      if (value.id !== id) return;
      clearTimeout(timer);
      socket.removeEventListener("message", handler);
      if (value.error || value.result?.exceptionDetails) {
        reject(new Error(JSON.stringify(value.error || value.result.exceptionDetails)));
      } else resolve(value.result.result.value);
    };
    socket.addEventListener("message", handler);
    socket.send(JSON.stringify({ id, method: "Runtime.evaluate", params: {
      expression,
      awaitPromise: true, returnByValue: true,
    } }));
  });
}

async function controllerSmoke() {
  const invoke = (command, args = {}) => evaluate(`window.__TAURI__.core.invoke(${JSON.stringify(command)},${JSON.stringify(args)})`);
  const initial = await invoke("get_controller_status");
  assert.equal(initial.running, false, "Do not disturb an already-running user controller server");
  let createdTab = null;
  try {
    const server = await invoke("start_controller_server", { config: { port: 1446, allowRead: true, allowControl: false } });
    let token = await invoke("get_controller_token");
    const base = server.endpoint;
    assert.equal((await fetch(`${base}/v1/health`)).status, 401);
    assert.equal((await fetch(`${base}/v1/health`, { headers: { authorization: "Bearer incorrect" } })).status, 401);
    const headers = () => ({ authorization: `Bearer ${token}`, "content-type": "application/json" });
    assert.equal((await fetch(`${base}/v1/health`, { headers: { ...headers(), origin: "https://untrusted.example" } })).status, 401);
    assert.equal((await fetch(`${base}/v1/chats`, { headers: headers() })).status, 200);
    assert.equal((await fetch(`${base}/v1/chats`, { method: "POST", headers: headers(), body: JSON.stringify({ title: "Forbidden test" }) })).status, 403);
    await invoke("rotate_controller_token");
    assert.equal((await fetch(`${base}/v1/health`, { headers: headers() })).status, 401);
    token = await invoke("get_controller_token");
    assert.equal((await fetch(`${base}/v1/health`, { headers: headers() })).status, 200);
    await invoke("stop_controller_server");
    await invoke("start_controller_server", { config: { port: 1447, allowRead: true, allowControl: true } });
    token = await invoke("get_controller_token");
    const controlBase = "http://127.0.0.1:1447";
    const created = await fetch(`${controlBase}/v1/chats`, { method: "POST", headers: headers(), body: JSON.stringify({ title: "Controller smoke test" }) });
    assert.equal(created.status, 201);
    createdTab = (await created.json()).id;
    await evaluate(`window.__controllerSmokeDone = new Promise(resolve => { window.__TAURI__.event.listen('controller-chat-updated', ({payload}) => { if(payload.tabId === ${createdTab}) resolve(true); }).then(unlisten => window.__controllerSmokeUnlisten = unlisten); }); true`);
    const submitted = await fetch(`${controlBase}/v1/chats/${createdTab}/turns`, { method: "POST", headers: headers(), body: JSON.stringify({ input: "Reply with one short greeting.", networkAllowed: false }) });
    assert.equal(submitted.status, 202);
    const accepted = await submitted.json();
    await evaluate("window.__controllerSmokeDone");
    const job = await (await fetch(`${controlBase}${accepted.statusUrl}`, { headers: headers() })).json();
    assert.equal(job.job.status, "completed", JSON.stringify(job));
    const chat = await (await fetch(`${controlBase}/v1/chats/${createdTab}`, { headers: headers() })).json();
    assert.ok(chat.chat.messages.some((message) => message.role === "llm" && message.text));
    return { authenticated: true, browserOriginsRejected: true, readOnlyEnforced: true, oldTokenRevoked: true, remoteTurn: job.job.status, messages: chat.chat.messages.length };
  } finally {
    await invoke("stop_controller_server");
    if (createdTab) {
      await evaluate(`(async () => { window.__controllerSmokeUnlisten?.(); const snapshot=await window.__TAURI__.core.invoke('read_tab_snapshot',{args:{key:'${createdTab}'}}); await window.__TAURI__.core.invoke('close_tab_and_snapshot',{tabId:${createdTab},snapshot}); location.reload(); return true; })()`);
    }
  }
}

try {
  if (controllerTest) {
    console.log(JSON.stringify(await controllerSmoke(), null, 2));
  } else {
  const message = await evaluate(`(${nativeTurn.toString()})(${JSON.stringify({ tabId, toolTest, networkTest, weatherTest, serviceTest, fresh, prompt, resumeTest })})`);
  console.log(JSON.stringify(message, null, 2));
  if (serviceTest) {
    assert.equal(message.server.online, true);
    assert.ok(message.search.hits?.length > 0, message.search.error);
    assert.equal(typeof message.weather.temperatureF, "number");
    assert.ok(message.weather.observedAt);
    assert.ok(message.weather.forecast);
  } else {
  if (resumeTest && message.resumed) {
    assert.equal(message.resumed.sameTask,true);
    assert.equal(message.resumed.sameGoal,true);
    assert.ok(message.resumed.actionsAdded>0);
  } else assert.equal(message.status, "completed");
  assert.equal(message.permissions, 0);
  assert.doesNotMatch(message.answer, /\{\s*"action"\s*:/);
  if (networkTest || weatherTest) {
    const tool = networkTest ? "universal_web_search" : "get_current_weather";
    assert.ok(message.tools.some((entry) => entry.tool === tool && entry.ok), `No successful ${tool} observation`);
    assert.match(message.answer, /https?:\/\//, "Live claims should cite an observed source");
    assert.equal(message.research?.evidenceSupported, true, "Research must have supported findings, not just successful HTTP requests");
  } else if (toolTest) {
    assert.ok(message.tools.some((entry) => entry.tool === "perform_arithmetic" && entry.ok));
    assert.match(message.answer, /703/);
  } else {
    assert.equal(message.actions, 1);
    assert.ok(message.deltas > 1);
  }
  }
  }
} finally {
  socket.close();
}
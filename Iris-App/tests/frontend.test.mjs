import { readFile } from "node:fs/promises";
import { test } from "node:test";
import assert from "node:assert/strict";
import ts from "typescript";

async function loadTypescript(relative) {
  const source = await readFile(new URL(relative, import.meta.url), "utf8");
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ES2022, target: ts.ScriptTarget.ES2022 } }).outputText;
  return import(`data:text/javascript;base64,${Buffer.from(compiled).toString("base64")}`);
}
const { readOllamaStream } = await loadTypescript("../src/hooks/useOllamaStream.ts");
const { themeTokens } = await loadTypescript("../src/lib/theme.ts");
const { advanceTaskClock, intervalMilliseconds, nextRecurringRun, parseTaskOutcome, taskIsDue } = await loadTypescript("../src/lib/automation.ts");
const { patchLastLLMBubble, clearProvisionalReply, reorderTabs } = await loadTypescript("../src/state/tabs.ts");
const { persistSnapshot } = await loadTypescript("../src/state/memory.ts");

test("snapshot updates retain saved reasoning logs", async () => {
  const originalWindow = globalThis.window;
  let saved;
  globalThis.window = { __TAURI__: { core: { invoke: async (command, args) => {
    assert.equal(command, "update_snapshot_memory");
    saved = args.args.snapshot;
  } } } };
  try {
    const log = { workLog: [{ ts: 1, text: "Reviewed actual evidence" }] };
    await persistSnapshot(1, { title: "Renamed", messages: [{ role: "llm", text: "Answer", thinkingLog: log }], artifacts: [], summary: "", microSummary: "", dialogueBullets: "" });
    assert.deepEqual(saved.messages[0].thinkingLog, log);
    assert.equal(saved.title, "Renamed");
  } finally { globalThis.window = originalWindow; }
});

test("tab reordering preserves chat data, utility tabs, and stable identities", () => {
  const tabs = [{ id: 1, type: "chat", messages: [{ text: "saved conversation" }] }, { id: 2, type: "settings" }, { id: 3, type: "chat", title: "Renamed" }];
  const reordered = reorderTabs(tabs, 3, 1);
  assert.deepEqual(reordered.map((tab) => tab.id), [3, 1, 2]);
  assert.equal(reordered[0], tabs[2]);
  assert.equal(reordered[1], tabs[0]);
  assert.equal(reordered[2], tabs[1]);
  assert.deepEqual(tabs.map((tab) => tab.id), [1, 2, 3]);
  assert.equal(reorderTabs(tabs, 99, 1), tabs);
  assert.equal(reorderTabs(tabs, 1, 1), tabs);
});

test("invalid provisional controller text is removed without touching earlier turns", () => {
  const earlier = [{role:"user",text:"hello"},{role:"llm",text:"Hello!"},{role:"user",text:"search"}];
  assert.deepEqual(clearProvisionalReply([...earlier,{role:"llm",text:"Let me try a tool..."}]),earlier);
  assert.deepEqual(clearProvisionalReply(earlier),earlier);
});

test("automation accepts positive integer intervals and rejects unsafe schedules", () => {
  assert.equal(intervalMilliseconds("5", "minutes"), 300000);
  for (const value of ["", "0", "-1", "1.5", "Infinity", "9007199254740991"]) assert.equal(intervalMilliseconds(value, "days"), 0);
});

test("automation timing survives delayed ticks without waking paused or failed tasks", () => {
  const clock = { status: "waiting", scheduleKind: "countdown", remainingMs: 5000, nextRunAt: 0, repeatMs: 5000, updatedAt: 1000 };
  const advanced = advanceTaskClock(clock, 10000);
  assert.equal(advanced.remainingMs, 0);
  assert.equal(taskIsDue(advanced, 10000), true);
  for (const status of ["paused", "running", "error", "idle"]) assert.equal(taskIsDue({ ...advanced, status }, 10000), false);
  assert.equal(nextRecurringRun(6000, 5000, 20000), 21000);
});

test("automation never infers completion from casual done or error wording", () => {
  assert.equal(parseTaskOutcome("The first draft is done, but revision remains.").status, "unknown");
  assert.equal(parseTaskOutcome("No error found.\nAUTONOMOUS_STATUS: INCOMPLETE - Two chapters remain.").status, "incomplete");
  assert.equal(parseTaskOutcome("AUTONOMOUS_STATUS: COMPLETE - All criteria verified.").status, "complete");
  assert.equal(parseTaskOutcome("AUTONOMOUS_STATUS: COMPLETE - quoted\nActually still working.").status, "unknown");
});

test("new replies never append to an earlier conversation turn", () => {
  const before = [{role:"user",text:"one"},{role:"llm",text:"first reply"},{role:"user",text:"two"}];
  const after = patchLastLLMBubble(before,"second reply");
  assert.equal(after.length,4);
  assert.equal(after[1].text,"first reply");
  assert.equal(after[3].text,"second reply");
});

function streamBytes(text) {
  const bytes = new TextEncoder().encode(text);
  return new ReadableStream({ start(controller) {
    for (let index = 0; index < bytes.length; index += 2) controller.enqueue(bytes.slice(index, index + 2));
    controller.close();
  }});
}

test("NDJSON preserves quoted braces and fragmented Unicode", async () => {
  const expected = 'const house = { name: "caf\u00e9", roof: "}" };';
  const input = `${JSON.stringify({message:{content:expected},done:false})}\n${JSON.stringify({done:true})}`;
  let actual = "";
  await readOllamaStream(streamBytes(input), (token) => { actual += token; });
  assert.equal(actual, expected);
});
test("remote errors are not silently swallowed", async () => {
  await assert.rejects(readOllamaStream(streamBytes('{"error":"out of memory"}\n'), () => {}), /out of memory/);
});
test("truncated streams cannot look complete", async () => {
  await assert.rejects(readOllamaStream(streamBytes('{"response":"partial"}\n'), () => {}), /completion record/);
});

function luminance(hex) {
  const values = [1,3,5].map((offset) => parseInt(hex.slice(offset,offset+2),16)/255)
    .map((value) => value <= 0.04045 ? value/12.92 : ((value+0.055)/1.055)**2.4);
  return values[0]*0.2126+values[1]*0.7152+values[2]*0.0722;
}
function contrast(first, second) {
  const values = [luminance(first),luminance(second)].sort((left,right)=>right-left);
  return (values[0]+0.05)/(values[1]+0.05);
}
for (const theme of ["Black","Blue","Pink","Purple","Silver","Grey","Custom"]) {
  for (const light of [false,true]) {
    test(`${theme} ${light ? "light" : "dark"} maintains readable text and accent controls`, () => {
      for (const custom of ["#000000","#ffffff","#ff0000","#0000ff","#ffff00"]) {
        const tokens = themeTokens(theme,custom,light);
        assert.ok(contrast(tokens["--app-text"],tokens["--app-bg"]) >= 7);
        assert.ok(contrast(tokens["--app-muted"],tokens["--app-bg"]) >= 4.5);
        assert.ok(contrast(tokens["--app-accent-ink"],tokens["--app-accent"]) >= 4.5);
        for (const surface of ["--app-surface", "--app-panel-bg", "--app-input-bg", "--app-user-bubble", "--app-gradient-start", "--app-gradient-end"]) {
          assert.ok(contrast(tokens["--app-text"], tokens[surface]) >= 4.5, `${theme}: ${surface}`);
        }
      }
    });
  }
}

test("neutral and blue themes have distinct surfaces in both modes", () => {
  for (const light of [false, true]) {
    const backgrounds = ["Black", "Silver", "Grey", "Blue"].map((theme) => themeTokens(theme, "#000000", light)["--app-bg"]);
    assert.equal(new Set(backgrounds).size, backgrounds.length);
    assert.notEqual(themeTokens("Custom", "#e15580", light)["--app-panel-bg"], themeTokens("Custom", "#2687d8", light)["--app-panel-bg"]);
  }
});

test("light mode surfaces are theme-tinted off-white, never harsh pure white", () => {
  for (const theme of ["Black", "Blue", "Pink", "Purple", "Silver", "Grey"]) {
    for (const surface of ["--app-surface", "--app-panel-bg", "--app-input-bg", "--app-tab-active-bg"]) {
      const value = themeTokens(theme, "#000000", true)[surface].toLowerCase();
      assert.notEqual(value, "#ffffff", `${theme} ${surface} must not be pure white`);
    }
  }
  // Distinct accent themes must produce visibly different light surfaces (tint carries through).
  const pink = themeTokens("Pink", "#000000", true)["--app-surface"];
  const blue = themeTokens("Blue", "#000000", true)["--app-surface"];
  assert.notEqual(pink, blue);
});
import { readFile, mkdir, copyFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { dirname, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import { homedir, platform } from "node:os";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
function option(name, fallback) {
  const index = args.indexOf(name);
  if (index < 0) return fallback;
  if (!args[index + 1] || args[index + 1].startsWith("--")) throw new Error(`${name} needs a value`);
  return args[index + 1];
}
const profile = option("--profile", "balanced");
if (!["balanced", "13b", "14b"].includes(profile)) throw new Error("Profile must be balanced, 13b, or 14b");
const ollama = option("--ollama", "ollama");
const files = [
  ["organizer", `${profile}/modelfile_organizer.txt`],
  ["coder", `${profile}/modelfile_coder.txt`],
  ["summarizer", "modelfile_summarizer.txt"],
  ["vision", "modelfile_vision.txt"],
];
const definitions = await Promise.all(files.map(async ([role, relative]) => {
  const path = join(root, "model_files", "macos", relative);
  const content = await readFile(path, "utf8");
  const base = content.match(/^FROM\s+(\S+)\s*$/m)?.[1];
  if (!base) throw new Error(`Missing FROM in ${relative}`);
  return { role, path, base };
}));
function run(command, commandArgs) {
  return new Promise((resolveRun, reject) => {
    const child = spawn(command, commandArgs, { stdio: "inherit", shell: false, env: { ...process.env, OLLAMA_NUM_PARALLEL: "1", OLLAMA_MAX_LOADED_MODELS: "1" } });
    child.once("error", reject);
    child.once("exit", (code) => code === 0 ? resolveRun() : reject(new Error(`${command} ${commandArgs[0]} exited ${code}`)));
  });
}
if (args.includes("--check")) {
  for (const model of definitions) console.log(`${model.role}: ${model.base}`);
} else {
  const serverReady = async () => {
    try { const response = await fetch("http://127.0.0.1:11434/api/version", { signal: AbortSignal.timeout(2000) }); return response.ok; }
    catch { return false; }
  };
  if (!(await serverReady())) {
    const server = spawn(ollama, ["serve"], { detached: true, stdio: "ignore", env: { ...process.env, OLLAMA_HOST: "127.0.0.1:11434", OLLAMA_NUM_PARALLEL: "1", OLLAMA_MAX_LOADED_MODELS: "1" } });
    await new Promise((resolveStart, reject) => { server.once("spawn", resolveStart); server.once("error", reject); });
    server.unref();
    for (let attempt = 0; attempt < 30 && !(await serverReady()); attempt++) await new Promise((resolveWait) => setTimeout(resolveWait, 1000));
    if (!(await serverReady())) throw new Error("Ollama did not start. Open Ollama and retry; model downloads have not started.");
  }
  for (const model of definitions) {
    await run(ollama, ["pull", model.base]);
    await run(ollama, ["create", `iris-${model.role}:latest`, "-f", model.path]);
  }
  const appData = platform() === "darwin" ? join(homedir(), "Library", "Application Support", "com.pixelcrowgames.iris-app")
    : platform() === "win32" ? join(process.env.APPDATA || join(homedir(), "AppData", "Roaming"), "com.pixelcrowgames.iris-app")
      : join(process.env.XDG_DATA_HOME || join(homedir(), ".local", "share"), "com.pixelcrowgames.iris-app");
  const destination = join(appData, "model_files");
  await mkdir(destination, { recursive: true });
  for (const model of definitions) await copyFile(model.path, join(destination, `modelfile_${model.role}.txt`));
  console.log(`Created Iris model aliases and installed the ${profile} profile in ${destination}`);
}
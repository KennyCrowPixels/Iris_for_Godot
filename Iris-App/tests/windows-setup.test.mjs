import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import test from "node:test";

function runSetup(installExit = 0, checkExit = 0) {
  const directory = mkdtempSync(join(tmpdir(), "iris-setup-test-"));
  const bin = join(directory, "bin");
  const calls = join(directory, "npm-calls.txt");
  mkdirSync(bin);
  writeFileSync(calls, "");
  writeFileSync(join(directory, "setup-windows.bat"), readFileSync(new URL("../setup-windows.bat", import.meta.url)));
  for (const command of ["winget", "git", "node", "cargo", "ollama"]) {
    writeFileSync(join(bin, `${command}.cmd`), "@exit /b 0\r\n");
  }
  writeFileSync(join(directory, "npm.cmd"), [
    "@echo off",
    `echo %*>>"${calls}"`,
    `if "%~1"=="install" exit /b ${installExit}`,
    `if "%~1"=="run" exit /b ${checkExit}`,
    "exit /b 99",
    "",
  ].join("\r\n"));
  try {
    const environment = Object.fromEntries(Object.entries(process.env).filter(([name]) =>
      !["path", "programfiles", "userprofile", "localappdata", "errorlevel"].includes(name.toLowerCase())
    ));
    const result = spawnSync(process.env.ComSpec || "cmd.exe", ["/d", "/c", "setup-windows.bat --dev --yes --skip-buildtools"], {
      cwd: directory,
      encoding: "utf8",
      env: {
        ...environment,
        PATH: `${bin};${process.env.SystemRoot}\\System32`,
        ProgramFiles: directory,
        USERPROFILE: directory,
        LOCALAPPDATA: directory,
      },
    });
    const recordedCalls = readFileSync(calls, "utf8").trim();
    return { ...result, calls: recordedCalls ? recordedCalls.split(/\r?\n/) : [] };
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

test("Windows setup installs incrementally and includes development tools", { skip: process.platform !== "win32" }, () => {
  const result = runSetup();
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stdout + result.stderr);
  assert.deepEqual(result.calls, ["install --include=dev --no-audit --no-fund", "run check"]);
});

test("Windows setup stops before TypeScript when npm returns a negative EPERM code", { skip: process.platform !== "win32" }, () => {
  const result = runSetup(-4048);
  assert.ifError(result.error);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.equal(result.calls.length, 1);
  assert.match(result.stdout, /npm install failed/);
  assert.doesNotMatch(result.stdout, /Validating TypeScript/);
});

test("Windows setup propagates failed TypeScript checks", { skip: process.platform !== "win32" }, () => {
  const result = runSetup(0, -4048);
  assert.ifError(result.error);
  assert.equal(result.status, 1, result.stdout + result.stderr);
  assert.match(result.stdout, /TypeScript check failed/);
});
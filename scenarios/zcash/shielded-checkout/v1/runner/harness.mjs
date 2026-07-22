import { readFile } from "node:fs/promises";
import { stripTypeScriptTypes } from "node:module";
import vm from "node:vm";

import { publicCases } from "./public-tests.mjs";
import { hiddenCases } from "./hidden-tests.mjs";

const PROTOCOL_VERSION = "vibequest-runner-protocol-1.0.0";
const SOURCE_PATH = "/job/starter/src/checkout.ts";
const MAX_SOURCE_BYTES = 64 * 1024;

class HarnessError extends Error {
  constructor(code) {
    super(code);
    this.code = code;
  }
}

function emit(result, exitCode) {
  process.stdout.write(`VQ_RESULT ${JSON.stringify(result)}\n`);
  process.exitCode = exitCode;
}

function classifyLoadError(error) {
  if (error?.code === "ERR_SCRIPT_EXECUTION_TIMEOUT") {
    return { classification: "timeout", exitCode: 124 };
  }
  if (error instanceof HarnessError) {
    return { classification: error.code, exitCode: 2 };
  }
  return { classification: "compile-error", exitCode: 2 };
}

async function loadLearnerModule() {
  const source = await readFile(SOURCE_PATH, "utf8");
  if (Buffer.byteLength(source, "utf8") > MAX_SOURCE_BYTES) {
    throw new HarnessError("source-limit");
  }

  let javascript;
  try {
    javascript = stripTypeScriptTypes(source, {
      mode: "strip",
      sourceUrl: "vibequest-learner.ts",
    });
  } catch {
    throw new HarnessError("compile-error");
  }

  const learnerGlobals = Object.create(null);
  for (const name of ["console", "process", "Buffer", "fetch", "require"]) {
    Object.defineProperty(learnerGlobals, name, {
      value: undefined,
      writable: false,
      configurable: false,
      enumerable: false,
    });
  }
  const context = vm.createContext(learnerGlobals, {
    name: "vibequest-learner",
    codeGeneration: { strings: false, wasm: false },
  });
  const learnerModule = new vm.SourceTextModule(javascript, {
    context,
    identifier: "vibequest-learner.ts",
    initializeImportMeta(meta) {
      Object.defineProperty(meta, "url", {
        value: "vibequest:learner",
        enumerable: true,
      });
    },
    importModuleDynamically() {
      throw new HarnessError("dynamic-import-denied");
    },
  });
  await learnerModule.link(() => {
    throw new HarnessError("import-denied");
  });
  await learnerModule.evaluate({ timeout: 1_000, breakOnSigint: true });
  return learnerModule.namespace;
}

async function runCases(api, cases) {
  const results = [];
  for (const testCase of cases) {
    try {
      await testCase.run(api);
      results.push({ case_id: testCase.caseId, status: "passed" });
    } catch {
      results.push({ case_id: testCase.caseId, status: "failed" });
    }
  }
  return results;
}

try {
  const api = await loadLearnerModule();
  const publicResults = await runCases(api, publicCases);
  const hiddenResults = await runCases(api, hiddenCases);
  const failedPublic = publicResults.filter((result) => result.status === "failed").length;
  const failedHidden = hiddenResults.filter((result) => result.status === "failed").length;
  const passed = failedPublic === 0 && failedHidden === 0;

  emit(
    {
      protocol_version: PROTOCOL_VERSION,
      classification: passed ? "passed" : "failed",
      public_cases: publicResults,
      hidden_summary: {
        passed: hiddenResults.length - failedHidden,
        failed: failedHidden,
      },
    },
    passed ? 0 : 1,
  );
} catch (error) {
  const failure = classifyLoadError(error);
  emit(
    {
      protocol_version: PROTOCOL_VERSION,
      classification: failure.classification,
      public_cases: [],
      hidden_summary: { passed: 0, failed: hiddenCases.length },
    },
    failure.exitCode,
  );
}

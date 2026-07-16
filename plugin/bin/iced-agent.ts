#!/usr/bin/env bun
// iced-agent CLI — one request/response against the loopback bridge.
// Program output goes to stdout (console.log is correct here, not tracing).

import { DEFAULT_APP_ID, send, type Response } from "./client.ts";

const USAGE = `iced-agent <cmd> [args...]  — drive the native iced shell via the loopback bridge

Global flags:
  --app <id>        discovery id (default ${DEFAULT_APP_ID})
  --window <name>   target window (default main)

Commands:
  tree                                   dump the semantic tree
  find [--role R] [--name N] [--text T]  filter nodes (all nullable)
  click <@ref | --x N --y N>             click a node or point
  hover <@ref | --x N --y N>             move the cursor over a target
  scroll <@ref|--x --y> [--dx N --dy N]  wheel-scroll at a target
  drag <@from> <@to> | --from @r --to @r drag between two targets
  type <text...>                         type text into the focused widget
  press <key> [--mod ctrl ...]           press a named key with modifiers
  state [path]                           read a curated state projection (dot path)
  intent <toggle_theme|section|navigate|search> [--name|--url|--query V]
  shot [--out file.png]                  screenshot (PNG base64, or decode to --out)
  logs [--clear]                         read (or clear) the log ring
  wait  [--role R --name N [--absent]] | [--path P --equals JSON] [--timeout MS]
  expect [same cond flags as wait]       assert a condition now
  windows                                list windows and bounds
  a11y                                   dump the AccessKit tree pushed to the OS
  run <recipe.json | ->                  execute a recipe (per-step report; exit = failures)

Examples:
  iced-agent find --role button --name Forge
  iced-agent click @3
  iced-agent type "hello world"
  iced-agent press enter --mod ctrl
  iced-agent state section
  iced-agent shot --out /tmp/app.png
  iced-agent run qa/nav-smoke.json
`;

interface Parsed {
  flags: Record<string, string>;
  mods: string[];
  positionals: string[];
  bools: Set<string>;
}

const BOOL_FLAGS = new Set(["clear", "exists", "absent"]);

function parseArgs(argv: string[]): Parsed {
  const flags: Record<string, string> = {};
  const mods: string[] = [];
  const positionals: string[] = [];
  const bools = new Set<string>();
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--mod") {
      const v = argv[++i];
      if (v !== undefined) mods.push(v);
    } else if (a.startsWith("--")) {
      const key = a.slice(2);
      if (BOOL_FLAGS.has(key)) bools.add(key);
      else flags[key] = argv[++i];
    } else {
      positionals.push(a);
    }
  }
  return { flags, mods, positionals, bools };
}

const num = (v: string | undefined): number | null => (v == null ? null : Number(v));

function target(p: Parsed): { ref: string | null; x: number | null; y: number | null } {
  const ref = p.flags.ref ?? p.positionals.find((t) => t.startsWith("@")) ?? null;
  return { ref, x: num(p.flags.x), y: num(p.flags.y) };
}

function cond(p: Parsed): unknown {
  if (p.flags.path != null) {
    let equals: unknown = null;
    if (p.flags.equals != null) {
      try {
        equals = JSON.parse(p.flags.equals);
      } catch {
        equals = p.flags.equals;
      }
    }
    return { state_path: { path: p.flags.path, equals } };
  }
  return {
    node: {
      role: p.flags.role ?? null,
      name: p.flags.name ?? null,
      exists: !p.bools.has("absent"),
    },
  };
}

function intent(p: Parsed): unknown {
  const kind = p.positionals[0];
  switch (kind) {
    case "toggle_theme":
      return "toggle_theme";
    case "section":
      return { section: { name: p.flags.name } };
    case "navigate":
      return { navigate: { url: p.flags.url } };
    case "search":
      return { search: { query: p.flags.query } };
    default:
      throw new Error(`unknown intent '${kind}' (want: toggle_theme|section|navigate|search)`);
  }
}

// Build the protocol `cmd` object from parsed args.
function buildCmd(name: string, p: Parsed): object {
  const window = p.flags.window ?? "main";
  switch (name) {
    case "tree":
      return { cmd: "tree", window };
    case "find":
      return { cmd: "find", window, role: p.flags.role ?? null, name: p.flags.name ?? null, text: p.flags.text ?? null };
    case "click":
      return { cmd: "click", target: target(p) };
    case "hover":
      return { cmd: "hover", target: target(p) };
    case "scroll":
      return { cmd: "scroll", target: target(p), dx: num(p.flags.dx) ?? 0, dy: num(p.flags.dy) ?? 0 };
    case "drag": {
      const refs = p.positionals.filter((t) => t.startsWith("@"));
      const from = { ref: p.flags.from ?? refs[0] ?? null, x: num(p.flags.fromx), y: num(p.flags.fromy) };
      const to = { ref: p.flags.to ?? refs[1] ?? null, x: num(p.flags.tox), y: num(p.flags.toy) };
      return { cmd: "drag", from, to };
    }
    case "type":
      return { cmd: "type", text: p.flags.text ?? p.positionals.join(" ") };
    case "press":
      return { cmd: "press", key: p.flags.key ?? p.positionals[0] ?? "", modifiers: p.mods };
    case "state":
      return { cmd: "state", path: p.flags.path ?? p.positionals[0] ?? "" };
    case "intent":
      return { cmd: "intent", intent: intent(p) };
    case "shot":
      return { cmd: "shot", window };
    case "logs":
      return { cmd: "logs", clear: p.bools.has("clear") };
    case "wait":
      return { cmd: "wait", cond: cond(p), timeout_ms: num(p.flags.timeout) ?? 5000 };
    case "expect":
      return { cmd: "expect", cond: cond(p) };
    case "windows":
      return { cmd: "windows" };
    case "a11y":
      return { cmd: "a11y", window };
    default:
      throw new Error(`unknown command '${name}' (see --help)`);
  }
}

// --- Recipe runner: pure client-side composition over the existing bridge. ---

// A recipe step is externally tagged: a single-key object like {click:{...}}.
type Step = Record<string, any>;

function stepSummary(step: Step): string {
  const kind = Object.keys(step)[0];
  const v = step[kind];
  switch (kind) {
    case "click":
      return `click ${v.role} "${v.name}"`;
    case "type":
      return `type "${v}"`;
    case "press":
      return `press ${v.key}${v.mods?.length ? ` [${v.mods.join(",")}]` : ""}`;
    case "intent":
      return `intent ${JSON.stringify(v)}`;
    case "expect":
      return `expect ${JSON.stringify(v)}`;
    case "wait":
      return `wait ${JSON.stringify(v.cond)}`;
    default:
      return kind ?? "(empty step)";
  }
}

// Run one step over the bridge; returns null on success or an error string.
async function runStep(appId: string, window: string, step: Step): Promise<string | null> {
  const kind = Object.keys(step)[0];
  const v = step[kind];
  const fail = (r: Response, what: string) => r.error ?? `${what} failed`;
  const passed = (r: Response) => (r.result as { pass?: boolean })?.pass === true;
  switch (kind) {
    case "click": {
      const found = await send(appId, { cmd: "find", window, role: v.role, name: v.name, text: null });
      if (!found.ok) return fail(found, "find");
      const matches = (found.result as { matches?: Array<{ ref: string }> })?.matches ?? [];
      if (matches.length === 0) return `no ${v.role} named "${v.name}"`;
      const clicked = await send(appId, { cmd: "click", target: { ref: matches[0].ref, x: null, y: null } });
      return clicked.ok ? null : fail(clicked, "click");
    }
    case "type": {
      const r = await send(appId, { cmd: "type", text: v });
      return r.ok ? null : fail(r, "type");
    }
    case "press": {
      const r = await send(appId, { cmd: "press", key: v.key, modifiers: v.mods ?? [] });
      return r.ok ? null : fail(r, "press");
    }
    case "intent": {
      const r = await send(appId, { cmd: "intent", intent: v });
      return r.ok ? null : fail(r, "intent");
    }
    case "expect": {
      // On a live app nothing is synchronous — intents/clicks apply on the
      // next runtime beat. A recipe `expect` is therefore "eventually, soon":
      // it rides the bridge's `wait` with a short timeout. (The in-process
      // lane keeps single-shot semantics; nothing is async there.)
      const r = await send(appId, { cmd: "wait", cond: v, timeout_ms: 3000 });
      if (!r.ok) return fail(r, "expect");
      return passed(r) ? null : "condition false (within 3s)";
    }
    case "wait": {
      const r = await send(appId, { cmd: "wait", cond: v.cond, timeout_ms: v.timeout_ms ?? 5000 });
      if (!r.ok) return fail(r, "wait");
      return passed(r) ? null : "timed out";
    }
    default:
      return `unknown step kind '${kind}'`;
  }
}

// Execute a recipe; per-step report, stop at first failure. Returns exit code.
async function runRecipe(appId: string, window: string, path: string): Promise<number> {
  const text = path === "-" ? await Bun.stdin.text() : await Bun.file(path).text();
  const recipe = JSON.parse(text) as { name?: string; steps?: Step[] };
  const steps = recipe.steps ?? [];
  console.log(`recipe ${recipe.name ?? "(unnamed)"} — ${steps.length} steps`);
  let failed = false;
  for (let i = 0; i < steps.length; i++) {
    const summary = stepSummary(steps[i]);
    if (failed) {
      console.log(`skipped ${i + 1} ${summary}`);
      continue;
    }
    const err = await runStep(appId, window, steps[i]);
    if (err === null) {
      console.log(`ok ${i + 1} ${summary}`);
    } else {
      console.log(`FAIL ${i + 1} ${summary}: ${err}`);
      failed = true;
    }
  }
  return failed ? 1 : 0;
}

async function main() {
  const argv = process.argv.slice(2);
  if (argv.length === 0 || argv[0] === "--help" || argv[0] === "-h") {
    console.log(USAGE);
    process.exit(0);
  }
  const name = argv[0];
  const p = parseArgs(argv.slice(1));
  const appId = p.flags.app ?? DEFAULT_APP_ID;

  if (name === "run") {
    const path = p.positionals[0];
    if (!path) {
      console.error("error: run needs a recipe path (or '-' for stdin)");
      process.exit(1);
    }
    process.exit(await runRecipe(appId, p.flags.window ?? "main", path));
  }

  const cmd = buildCmd(name, p);
  const resp = await send(appId, cmd);

  if (!resp.ok) {
    console.error(`error: ${resp.error ?? "unknown error"}`);
    process.exit(1);
  }

  // shot --out: decode the PNG to a file instead of dumping base64.
  if (name === "shot" && p.flags.out) {
    const b64 = (resp.result as { png_base64?: string })?.png_base64;
    if (!b64) {
      console.error("error: shot result has no png_base64");
      process.exit(1);
    }
    const bytes = Buffer.from(b64, "base64");
    await Bun.write(p.flags.out, bytes);
    console.log(JSON.stringify({ out: p.flags.out, bytes: bytes.length }));
    return;
  }

  console.log(JSON.stringify(resp.result, null, 2));
}

main().catch((e) => {
  console.error(`error: ${e?.message ?? e}`);
  process.exit(1);
});

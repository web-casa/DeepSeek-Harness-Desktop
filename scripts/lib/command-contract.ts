import { readFileSync } from "node:fs";
import { join } from "node:path";

export interface CommandContractSources {
  main: string;
  build: string;
  bootstrap: string;
  harness: string;
  frontend?: string;
}

export interface CapabilityDocument {
  identifier?: unknown;
  windows?: unknown;
  permissions?: unknown;
}

function sortedUnique(values: Iterable<string>): string[] {
  return [...new Set(values)].sort((left, right) => left.localeCompare(right, "en"));
}

function skipQuoted(source: string, index: number, quote: string): number {
  let escaped = false;
  for (let cursor = index + 1; cursor < source.length; cursor += 1) {
    const character = source[cursor];
    if (escaped) {
      escaped = false;
    } else if (character === "\\") {
      escaped = true;
    } else if (character === quote) {
      return cursor + 1;
    }
  }
  throw new Error(`unterminated ${quote} string while parsing Rust source`);
}

/** Extract one balanced Rust macro/array body while ignoring comments and literals. */
export function rustDelimitedBody(
  source: string,
  marker: string,
  open = "[",
  close = "]",
): string {
  const markerIndex = source.indexOf(marker);
  if (markerIndex < 0) throw new Error(`missing Rust marker ${JSON.stringify(marker)}`);
  const openIndex = source.indexOf(open, markerIndex + marker.length);
  if (openIndex < 0) throw new Error(`missing ${open} after ${JSON.stringify(marker)}`);

  let depth = 1;
  let cursor = openIndex + 1;
  while (cursor < source.length) {
    const character = source[cursor];
    const next = source[cursor + 1];
    if (character === '"' || character === "'") {
      cursor = skipQuoted(source, cursor, character);
      continue;
    }
    if (character === "/" && next === "/") {
      const newline = source.indexOf("\n", cursor + 2);
      cursor = newline < 0 ? source.length : newline + 1;
      continue;
    }
    if (character === "/" && next === "*") {
      const end = source.indexOf("*/", cursor + 2);
      if (end < 0) throw new Error("unterminated Rust block comment");
      cursor = end + 2;
      continue;
    }
    if (character === open) depth += 1;
    if (character === close) {
      depth -= 1;
      if (depth === 0) return source.slice(openIndex + 1, cursor);
    }
    cursor += 1;
  }
  throw new Error(`unterminated ${open}${close} body after ${JSON.stringify(marker)}`);
}

export function handlerCommands(source: string): string[] {
  const body = rustCodeOnly(rustDelimitedBody(source, "tauri::generate_handler!"));
  return sortedUnique(
    [...body.matchAll(/\b(?:[a-z][a-z0-9_]*::)+([a-z][a-z0-9_]*)\b/g)].map(
      (match) => match[1],
    ),
  );
}

export function manifestCommands(source: string): string[] {
  const body = rustDelimitedBody(source, "AppManifest::new().commands");
  return sortedUnique(rustStringLiterals(body).filter((value) => /^[a-z][a-z0-9_]*$/.test(value)));
}

export function frontendInvokeCommands(source: string): string[] {
  const commands: string[] = [];
  let cursor = 0;
  while (cursor < source.length) {
    const character = source[cursor];
    const next = source[cursor + 1];
    if (character === "/" && next === "/") {
      const newline = source.indexOf("\n", cursor + 2);
      cursor = newline < 0 ? source.length : newline + 1;
      continue;
    }
    if (character === "/" && next === "*") {
      const end = source.indexOf("*/", cursor + 2);
      if (end < 0) throw new Error("unterminated TypeScript block comment");
      cursor = end + 2;
      continue;
    }
    if (character === '"' || character === "'" || character === "`") {
      cursor = skipQuoted(source, cursor, character);
      continue;
    }
    if (
      source.startsWith("invoke", cursor) &&
      !/[A-Za-z0-9_$]/.test(source[cursor - 1] ?? "") &&
      !/[A-Za-z0-9_$]/.test(source[cursor + "invoke".length] ?? "")
    ) {
      let argument = cursor + "invoke".length;
      while (/\s/.test(source[argument] ?? "")) argument += 1;
      if (source[argument] === "(") {
        argument += 1;
        while (/\s/.test(source[argument] ?? "")) argument += 1;
        const quote = source[argument];
        if (quote === '"' || quote === "'") {
          const end = skipQuoted(source, argument, quote);
          const value = source.slice(argument + 1, end - 1);
          if (/^[a-z][a-z0-9_]*$/.test(value)) commands.push(value);
          cursor = end;
          continue;
        }
      }
    }
    cursor += 1;
  }
  return sortedUnique(commands);
}

function rustCodeOnly(source: string): string {
  let output = "";
  let cursor = 0;
  while (cursor < source.length) {
    const character = source[cursor];
    const next = source[cursor + 1];
    if (character === "/" && next === "/") {
      const newline = source.indexOf("\n", cursor + 2);
      cursor = newline < 0 ? source.length : newline;
      output += "\n";
      continue;
    }
    if (character === "/" && next === "*") {
      const end = source.indexOf("*/", cursor + 2);
      if (end < 0) throw new Error("unterminated Rust block comment");
      output += " ";
      cursor = end + 2;
      continue;
    }
    if (character === '"' || character === "'") {
      cursor = skipQuoted(source, cursor, character);
      output += " ";
      continue;
    }
    output += character;
    cursor += 1;
  }
  return output;
}

function rustStringLiterals(source: string): string[] {
  const values: string[] = [];
  let cursor = 0;
  while (cursor < source.length) {
    const character = source[cursor];
    const next = source[cursor + 1];
    if (character === "/" && next === "/") {
      const newline = source.indexOf("\n", cursor + 2);
      cursor = newline < 0 ? source.length : newline + 1;
      continue;
    }
    if (character === "/" && next === "*") {
      const end = source.indexOf("*/", cursor + 2);
      if (end < 0) throw new Error("unterminated Rust block comment");
      cursor = end + 2;
      continue;
    }
    if (character === '"') {
      const end = skipQuoted(source, cursor, character);
      values.push(source.slice(cursor + 1, end - 1));
      cursor = end;
      continue;
    }
    if (character === "'") {
      cursor = skipQuoted(source, cursor, character);
      continue;
    }
    cursor += 1;
  }
  return values;
}

function parseCapability(source: string, label: string): CapabilityDocument {
  try {
    const parsed: unknown = JSON.parse(source);
    if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
      throw new Error("root must be an object");
    }
    return parsed as CapabilityDocument;
  } catch (error) {
    throw new Error(`${label} is not valid JSON: ${(error as Error).message}`);
  }
}

export function bootstrapCommands(source: string): string[] {
  const parsed = parseCapability(source, "bootstrap capability");
  if (!Array.isArray(parsed.permissions)) {
    throw new Error("bootstrap capability permissions must be an array");
  }
  const commands: string[] = [];
  for (const permission of parsed.permissions) {
    if (typeof permission !== "string") continue;
    if (permission.startsWith("allow-") && !permission.includes(":")) {
      commands.push(permission.slice("allow-".length).replaceAll("-", "_"));
    }
  }
  return sortedUnique(commands);
}

function describeDifference(expected: string[], actual: string[], label: string): string[] {
  const expectedSet = new Set(expected);
  const actualSet = new Set(actual);
  const missing = expected.filter((command) => !actualSet.has(command));
  const extra = actual.filter((command) => !expectedSet.has(command));
  const problems: string[] = [];
  if (missing.length > 0) problems.push(`${label} missing: ${missing.join(", ")}`);
  if (extra.length > 0) problems.push(`${label} extra: ${extra.join(", ")}`);
  return problems;
}

function harnessProblems(source: string): string[] {
  const parsed = parseCapability(source, "Harness capability");
  const problems: string[] = [];
  if (parsed.identifier !== "harness") problems.push("Harness capability identifier must be harness");
  if (!Array.isArray(parsed.windows) || parsed.windows.length !== 1 || parsed.windows[0] !== "harness") {
    problems.push('Harness capability windows must equal ["harness"]');
  }
  if (!Array.isArray(parsed.permissions) || parsed.permissions.length !== 0) {
    problems.push("Harness capability permissions must remain empty");
  }
  return problems;
}

export function commandContractProblems(sources: CommandContractSources): string[] {
  let handler: string[];
  let manifest: string[];
  let bootstrap: string[];
  try {
    handler = handlerCommands(sources.main);
    manifest = manifestCommands(sources.build);
    bootstrap = bootstrapCommands(sources.bootstrap);
  } catch (error) {
    return [(error as Error).message];
  }
  const problems = [
    ...describeDifference(handler, manifest, "build.rs AppManifest"),
    ...describeDifference(handler, bootstrap, "bootstrap capability"),
  ];
  try {
    problems.push(...harnessProblems(sources.harness));
  } catch (error) {
    problems.push((error as Error).message);
  }
  if (sources.frontend !== undefined) {
    const frontend = frontendInvokeCommands(sources.frontend);
    const registered = new Set(handler);
    const unknown = frontend.filter((command) => !registered.has(command));
    if (unknown.length > 0) problems.push(`frontend invokes unregistered commands: ${unknown.join(", ")}`);
  }
  if (handler.length === 0) problems.push("invoke_handler command set must not be empty");
  return problems;
}

export function repositoryCommandContractProblems(repoRoot: string): string[] {
  const read = (relative: string) => readFileSync(join(repoRoot, relative), "utf8");
  return commandContractProblems({
    main: read("src-tauri/src/main.rs"),
    build: read("src-tauri/build.rs"),
    bootstrap: read("src-tauri/capabilities/bootstrap.json"),
    harness: read("src-tauri/capabilities/harness.json"),
    frontend: read("src/lib/api.ts"),
  });
}

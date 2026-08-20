import { fail, ok } from "./lib/common.ts";
import {
  NATIVE_RELEASE_TARGETS,
  githubNativeMatrix,
  githubMacosNotarizationMatrix,
  githubMsixMatrix,
  releasePlanProblems,
  targetById,
} from "./lib/release-artifacts.ts";

function argument(name: string): string | undefined {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : undefined;
}

const problems = releasePlanProblems();
if (problems.length > 0) {
  fail(`release plan is invalid:\n- ${problems.join("\n- ")}`);
}

const targetSelection = argument("--target") ?? "all";
if (targetSelection !== "all" && !targetById(targetSelection)) {
  fail(`unknown native release target: ${targetSelection}`);
}

if (process.argv.includes("--github-matrix")) {
  process.stdout.write(JSON.stringify(githubNativeMatrix(targetSelection)));
} else if (process.argv.includes("--github-msix-matrix")) {
  process.stdout.write(JSON.stringify(githubMsixMatrix()));
} else if (process.argv.includes("--github-macos-notarization-matrix")) {
  process.stdout.write(JSON.stringify(githubMacosNotarizationMatrix(targetSelection)));
} else {
  for (const target of NATIVE_RELEASE_TARGETS) {
    console.log(`${target.id}: ${target.bundles.join(", ")} (${target.os})`);
  }
  ok("release plan covers every requested format; Store MSIX remains separate");
}

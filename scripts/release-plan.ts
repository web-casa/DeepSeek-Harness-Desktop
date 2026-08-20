import { fail, ok } from "./lib/common.ts";
import {
  NATIVE_RELEASE_TARGETS,
  githubNativeMatrix,
  githubMacosNotarizationMatrix,
  githubMsixMatrix,
  releasePlanProblems,
} from "./lib/release-artifacts.ts";

const problems = releasePlanProblems();
if (problems.length > 0) {
  fail(`release plan is invalid:\n- ${problems.join("\n- ")}`);
}

if (process.argv.includes("--github-matrix")) {
  process.stdout.write(JSON.stringify(githubNativeMatrix()));
} else if (process.argv.includes("--github-msix-matrix")) {
  process.stdout.write(JSON.stringify(githubMsixMatrix()));
} else if (process.argv.includes("--github-macos-notarization-matrix")) {
  process.stdout.write(JSON.stringify(githubMacosNotarizationMatrix()));
} else {
  for (const target of NATIVE_RELEASE_TARGETS) {
    console.log(`${target.id}: ${target.bundles.join(", ")} (${target.os})`);
  }
  ok("release plan covers every requested format; Store MSIX remains separate");
}

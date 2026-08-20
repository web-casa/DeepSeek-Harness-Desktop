import { fail, ok, repoRoot } from "./lib/common.ts";
import { repositoryCommandContractProblems } from "./lib/command-contract.ts";

const problems = repositoryCommandContractProblems(repoRoot);
if (problems.length > 0) {
  fail(`command permission contract drift:\n- ${problems.join("\n- ")}`);
}
ok("command permission contract aligned; Harness capability remains empty");

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const buildWorkflow = readFileSync(
  new URL("../../.github/workflows/snap.yml", import.meta.url),
  "utf8",
);
const promotionWorkflow = readFileSync(
  new URL("../../.github/workflows/snap-promote.yml", import.meta.url),
  "utf8",
);
const snapDependencies = readFileSync(
  new URL("../ci/install-linux-snap-deps.sh", import.meta.url),
  "utf8",
);

test("Snap CI builds source DEBs natively and validates an enforced strict install", () => {
  assert.match(buildWorkflow, /runs-on: \$\{\{ matrix\.os \}\}/);
  assert.match(buildWorkflow, /node scripts\/snap-plan\.ts --github-matrix/);
  assert.match(buildWorkflow, /node scripts\/verify-native-host\.ts --target "\$\{\{ matrix\.nativeTarget \}\}"/);
  assert.match(buildWorkflow, /pnpm tauri build\n\s+--bundles deb/);
  assert.match(buildWorkflow, /node scripts\/build-snap\.ts --arch "\$\{\{ matrix\.arch \}\}"/);
  assert.match(buildWorkflow, /snap install --dangerous --jailmode --name="\$instance" "\$package"/);
  assert.equal((buildWorkflow.match(/instance="dsh-desktop-community"/g) ?? []).length, 2);
  assert.doesNotMatch(buildWorkflow, /instance="dsh-desktop-community_/);
  assert.doesNotMatch(buildWorkflow, /experimental\.parallel-instances/);
  assert.equal(
    (buildWorkflow.match(/\$2 == plug && \$3 == slot/g) ?? []).length,
    2,
    "connection assertions must compare the Plug and Slot columns, not the Interface column",
  );
  assert.doesNotMatch(buildWorkflow, /\$1 == plug && \$2 == slot/);
  assert.match(buildWorkflow, /sudo snap connect "\$instance:gpu-2404" mesa-2404:gpu-2404/);
  assert.match(buildWorkflow, /sudo snap connect "\$instance:gnome-46-2404" gnome-46-2404:gnome-46-2404/);
  assert.match(buildWorkflow, /assert_connection gpu-2404 mesa-2404:gpu-2404/);
  assert.match(buildWorkflow, /assert_connection gnome-46-2404 gnome-46-2404:gnome-46-2404/);
  assert.equal(
    (buildWorkflow.match(/assert_connection network-status :network-status/g) ?? []).length,
    2,
    "both local strict-install and Store-candidate checks must require the portal NetworkMonitor interface",
  );
  assert.match(snapDependencies, /amd64\) snapcraft_revision=18514/);
  assert.match(snapDependencies, /arm64\) snapcraft_revision=18519/);
  assert.match(snapDependencies, /snapcraft 9\.0\.1/);
  assert.match(snapDependencies, /for provider in mesa-2404 gtk-common-themes gnome-46-2404/);
  assert.match(snapDependencies, /sudo snap install "\$provider"/);
  assert.doesNotMatch(buildWorkflow, /snap install .*--devmode/);
});

test("Snap Store mutation is separately protected and requires the web-casa identity", () => {
  const buildJob = buildWorkflow.slice(
    buildWorkflow.indexOf("  build:"),
    buildWorkflow.indexOf("  publish-candidate:"),
  );
  assert.ok(buildJob.length > 0);
  assert.doesNotMatch(buildJob, /SNAPCRAFT_STORE_CREDENTIALS/);
  assert.match(buildWorkflow, /vars\.SNAP_CANDIDATE_PUBLISH_REQUESTED == 'true'/);
  assert.match(buildWorkflow, /environment: snap-candidate/);
  assert.match(buildWorkflow, /SNAPCRAFT_EXPECTED_EMAIL/);
  assert.match(buildWorkflow, /SNAP_CANDIDATE_PUBLISH_ENABLED/);
  assert.match(buildWorkflow, /Re-verify exactly the amd64 and arm64 package pair before upload/);
  assert.match(buildWorkflow, /node scripts\/verify-snap\.ts --arch "\$arch"/);
  assert.doesNotMatch(buildWorkflow, /merge-multiple: true/);
  assert.match(buildWorkflow, /candidate_channel="latest\/candidate\/\$SNAP_RELEASE_TAG"/);
  assert.match(buildWorkflow, /snapcraft upload --release "\$candidate_channel"/);
  assert.match(
    buildWorkflow,
    /Verify the actual Store candidate auto-connects its signed content providers/,
  );
  assert.match(
    buildWorkflow,
    /snap install --channel "\$candidate_channel" "\$instance"/,
  );
  assert.doesNotMatch(
    buildWorkflow,
    /snap install --channel "\$candidate_channel" --name=/,
  );
  assert.match(promotionWorkflow, /environment: snap-stable/);
  assert.match(promotionWorkflow, /vars\.SNAP_STABLE_PROMOTION_REQUESTED == 'true'/);
  assert.match(promotionWorkflow, /SNAP_STABLE_PROMOTION_ENABLED/);
  assert.match(promotionWorkflow, /candidate_channel="latest\/candidate\/\$SNAP_RELEASE_TAG"/);
  assert.match(
    promotionWorkflow,
    /snapcraft promote dsh-desktop-community --from-channel "\$candidate_channel" --to-channel latest\/stable --yes/,
  );
  assert.doesNotMatch(promotionWorkflow, /--from-channel candidate --to-channel stable/);
  assert.match(promotionWorkflow, /git checkout --detach "refs\/tags\/\$SNAP_RELEASE_TAG"/);
});

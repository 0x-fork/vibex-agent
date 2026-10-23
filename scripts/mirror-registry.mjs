#!/usr/bin/env node
// Mirror the published GHCR manifest lists to another registry.
//
// Every mirror target is a convenience channel for operators who already pull
// from there, so this copies the exact multi-arch indexes GHCR already
// published instead of rebuilding anything: a second build would be a second
// artifact with its own provenance. `docker buildx imagetools create` copies
// the manifest list and its blobs across registries without re-deriving them,
// so the digest a user gets from the mirror is the digest CI verified.
//
// Stable only. A mirror is where a user picks an image for a deployment they
// keep, so an `rc`/`edge` tag would be consumed as if it were supported; the
// workflows only invoke this for a `vX.Y.Z` tag, and the script refuses any tag
// with a prerelease suffix as a second guard.
//
// Docker Hub and the CNB artifact registry are both plain OCI registries, so
// the only difference between them is `--target-registry` and `--namespace`.

import { execFileSync } from "node:child_process";

const ARCHITECTURES = ["amd64", "arm64"];
const SERVICES = ["server", "relay"];

function fail(message) {
  throw new Error(message);
}

function run(command, args) {
  return execFileSync(command, args, { encoding: "utf8", stdio: "pipe" }).trim();
}

function parseArguments(argv) {
  const values = { version: "", sourceRegistry: "ghcr.io", targetRegistry: "docker.io", namespace: "vibexai", services: SERVICES, dryRun: false };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    switch (flag) {
      case "--version": values.version = argv[++index] ?? ""; break;
      case "--source-registry": values.sourceRegistry = argv[++index] ?? ""; break;
      case "--target-registry": values.targetRegistry = argv[++index] ?? ""; break;
      case "--namespace": values.namespace = argv[++index] ?? ""; break;
      case "--services": values.services = (argv[++index] ?? "").split(",").map((v) => v.trim()).filter(Boolean); break;
      case "--dry-run": values.dryRun = true; break;
      case "--help": values.help = true; break;
      default: fail(`unexpected argument ${flag}`);
    }
  }
  return values;
}

// A stable release is `MAJOR.MINOR.PATCH` with no suffix. Everything else —
// `0.1.0-rc.8`, `edge`, a commit sha — is built and published to GHCR only.
function assertStable(version) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/u.test(version)) {
    fail(`refusing to mirror a non-stable version to a public registry: ${version}`);
  }
}

// The tags a mirror serves for one stable release. `latest` moves here
// because a stable release is the one operators are told to run; GHCR keeps
// its own `latest` rule and the two are derived from the same release event.
function publishedTags(version) {
  return [version, `v${version}`, "latest"];
}

function sourceImage(registry, repository, service) {
  return `${registry}/${repository}-${service}`;
}

function architectureSet(reference) {
  const raw = run("docker", ["buildx", "imagetools", "inspect", "--raw", reference]);
  let parsed;
  try {
    parsed = JSON.parse(raw);
  } catch {
    fail(`${reference} did not answer with a manifest index`);
  }
  const manifests = parsed.manifests;
  if (!Array.isArray(manifests)) fail(`${reference} is a single manifest, not a multi-arch index`);
  return manifests
    .map((entry) => entry?.platform?.architecture)
    .filter((value) => typeof value === "string")
    .sort();
}

function main() {
  const args = parseArguments(process.argv.slice(2));
  if (args.help) {
    console.log(`Usage: node scripts/mirror-registry.mjs --version <x.y.z> --namespace <target-namespace> [options]

  --version <x.y.z>        stable release to mirror (required)
  --namespace <name>       target namespace (default: vibexai)
  --source-registry <r>    registry the release was published to (default: ghcr.io)
  --target-registry <r>    registry to mirror into (default: docker.io)
  --services <a,b>         services to mirror (default: ${SERVICES.join(",")})
  --dry-run                print the plan without writing`);
    return;
  }

  const version = args.version;
  if (!version) fail("--version is required");
  if (!args.namespace) fail("--namespace is required");
  assertStable(version);

  const repository = process.env.VIBEX_REPOSITORY || "vibex-ai/vibex";
  const tags = publishedTags(version);
  const plan = [];

  for (const service of args.services) {
    const source = sourceImage(args.sourceRegistry, repository, service);
    // The mirror repository keeps the same name the source uses, so an
    // operator can swap `ghcr.io/...` for `docker.io/<namespace>/...` without
    // learning a second naming scheme.
    const target = `${args.targetRegistry}/${args.namespace}/vibex-${service}`;
    plan.push({ service, source, target, sourceReference: `${source}:${version}` });
  }

  for (const entry of plan) {
    console.log(`${entry.sourceReference} -> ${entry.target}:{${tags.join(",")}}`);
  }
  if (args.dryRun) {
    console.log("dry run: not mirroring");
    return;
  }

  for (const entry of plan) {
    // Prove the source really is the multi-arch list this release promised
    // before writing anything to the target.
    const architectures = architectureSet(entry.sourceReference);
    if (JSON.stringify(architectures) !== JSON.stringify(ARCHITECTURES)) {
      fail(`${entry.sourceReference} carries ${architectures.join(",") || "nothing"}; expected ${ARCHITECTURES.join(",")}`);
    }

    const tagArgs = tags.map((tag) => ["-t", `${entry.target}:${tag}`]).flat();
    run("docker", ["buildx", "imagetools", "create", ...tagArgs, entry.sourceReference]);

    for (const tag of tags) {
      const mirrored = architectureSet(`${entry.target}:${tag}`);
      if (JSON.stringify(mirrored) !== JSON.stringify(ARCHITECTURES)) {
        fail(`${entry.target}:${tag} carries ${mirrored.join(",") || "nothing"} after the mirror`);
      }
    }
    console.log(`mirrored ${entry.target} at ${tags.join(", ")}`);
  }
}

// A thrown `fail` must read as one line in the CI log, not as a node stack
// trace that hides which registry or tag the mirror choked on.
try {
  main();
} catch (error) {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
}

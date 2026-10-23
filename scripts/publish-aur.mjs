#!/usr/bin/env node
// Rewrite the AUR `vibex-bin` PKGBUILD for one stable release and publish it.
//
// The package installs the reviewed Linux .deb that the tagged GitHub Release
// already publishes, so this script never builds Vibex: it pins the version,
// records the checksums of both architecture payloads, regenerates .SRCINFO,
// and pushes the result to the AUR git remote.
//
// Stable only. AUR carries exactly one version per package, so publishing a
// release candidate would downgrade every Arch user; the prerelease guard and
// the "never move backwards" comparison below both refuse that.

import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { dirname } from "node:path";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PACKAGE = "vibex-bin";
const DEFAULT_PKGBUILD = "packaging/aur/vibex-bin/PKGBUILD";
const ARCHITECTURES = ["x86_64", "aarch64"];
const AUR_RPC = "https://aur.archlinux.org/rpc/v5/info";
const REPOSITORY = "vibex-ai/vibex";

function fail(message) {
  throw new Error(message);
}

function parseArguments(argv) {
  const values = { version: "", archiveDir: "", pkgbuild: DEFAULT_PKGBUILD, dryRun: false, skipPush: false };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    switch (flag) {
      case "--version": values.version = argv[++index] ?? ""; break;
      case "--archive-dir": values.archiveDir = argv[++index] ?? ""; break;
      case "--pkgbuild": values.pkgbuild = argv[++index] ?? ""; break;
      case "--dry-run": values.dryRun = true; break;
      case "--skip-push": values.skipPush = true; break;
      case "--help": values.help = true; break;
      default: fail(`unexpected argument ${flag}`);
    }
  }
  if (!values.help && !values.version) fail("--version is required");
  return values;
}

// SemVer "MAJOR.MINOR.PATCH" with no prerelease or build suffix. AUR must only
// ever receive a stable release, so anything else is rejected outright.
function assertStable(version) {
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/u.test(version)) {
    fail(`refusing to publish a non-stable version to the AUR: ${version}`);
  }
}

function compareVersions(left, right) {
  const parse = (value) => value.split(".").map((part) => Number.parseInt(part, 10));
  const a = parse(left);
  const b = parse(right);
  for (let index = 0; index < Math.max(a.length, b.length); index += 1) {
    const delta = (a[index] ?? 0) - (b[index] ?? 0);
    if (delta !== 0) return Math.sign(delta);
  }
  return 0;
}

function sha256(file) {
  return createHash("sha256").update(readFileSync(file)).digest("hex");
}

// `name=(\n  'value'\n  'value'\n)` — the PKGBUILD keeps one checksum array per
// architecture so an aarch64 install verifies the same payload an x86_64 one
// does instead of inheriting `SKIP`.
function replaceArray(source, name, values) {
  const opener = `${name}=(`;
  const start = source.indexOf(opener);
  if (start < 0) fail(`the PKGBUILD has no ${name} array`);
  const end = source.indexOf("\n)", start);
  if (end < 0) fail(`the ${name} array is unterminated`);
  const body = values.map((value) => `  '${value}'`).join("\n");
  return `${source.slice(0, start)}${opener}\n${body}${source.slice(end)}`;
}

function releaseAsset(version, arch) {
  return `vibex-${version}-linux-${arch}-deb.deb`;
}

async function download(url, destination) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) fail(`cannot download ${url}: ${response.status} ${response.statusText}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length === 0) fail(`${url} answered with an empty body`);
  writeFileSync(destination, bytes);
  return bytes.length;
}

async function currentAurVersion() {
  const response = await fetch(`${AUR_RPC}?arg[]=${PACKAGE}`);
  if (!response.ok) fail(`AUR RPC answered ${response.status}`);
  const payload = await response.json();
  const result = payload?.results?.[0];
  return result?.Version ?? null;
}

function run(command, args, options = {}) {
  return execFileSync(command, args, { encoding: "utf8", stdio: "pipe", ...options }).trim();
}

function packageVersionFromSrcinfo(srcinfo) {
  const match = /^\tpkgver = (.+)$/mu.exec(srcinfo);
  return match ? match[1].trim() : null;
}

// `makepkg` refuses to run as root before it even looks at `--printsrcinfo`
// (its EUID guard precedes the mode dispatch), and a container job starts as
// root. Re-entering as an unprivileged user keeps the invocation identical on a
// developer machine, where the guard never fires.
function runMakepkg(args, cwd) {
  if (typeof process.getuid === "function" && process.getuid() === 0) {
    const user = process.env.PKG_BUILDER_USER || "builder";
    return run("runuser", ["-u", user, "--", "makepkg", ...args], { cwd });
  }
  return run("makepkg", args, { cwd });
}

async function main() {
  const args = parseArguments(process.argv.slice(2));
  if (args.help) {
    console.log(`Usage: node scripts/publish-aur.mjs --version <x.y.z> [options]

  --version <x.y.z>     stable release to package (required)
  --archive-dir <dir>   use local .deb files instead of downloading them
  --pkgbuild <path>     PKGBUILD to rewrite (default: ${DEFAULT_PKGBUILD})
  --dry-run             rewrite and verify, print the result, publish nothing
  --skip-push           rewrite and verify without touching the AUR remote`);
    return;
  }

  const version = args.version;
  assertStable(version);

  const published = await currentAurVersion();
  if (published && compareVersions(version, published) <= 0) {
    console.log(`AUR already carries ${PACKAGE} ${published}; nothing newer to publish for ${version}.`);
    return;
  }
  console.log(published ? `AUR ${PACKAGE} ${published} -> ${version}` : `AUR ${PACKAGE} is new at ${version}`);

  const staging = mkdtempSync(join(tmpdir(), "vibex-aur-"));
  const archives = {};
  try {
    for (const arch of ARCHITECTURES) {
      const name = releaseAsset(version, arch);
      const local = args.archiveDir ? join(resolve(args.archiveDir), name) : join(staging, name);
      if (args.archiveDir) {
        if (!existsSync(local)) fail(`missing local archive ${local}`);
      } else {
        const url = `https://github.com/${REPOSITORY}/releases/download/v${version}/${name}`;
        const bytes = await download(url, local);
        console.log(`downloaded ${name} (${bytes} bytes)`);
      }
      archives[arch] = { path: local, sha256: sha256(local) };
    }

    const license = join(staging, `vibex-${version}-LICENSE`);
    if (args.archiveDir) {
      const provided = join(resolve(args.archiveDir), `vibex-${version}-LICENSE`);
      if (!existsSync(provided)) fail(`missing local license copy ${provided}`);
      writeFileSync(license, readFileSync(provided));
    } else {
      await download(`https://raw.githubusercontent.com/${REPOSITORY}/v${version}/LICENSE`, license);
    }
    const licenseSha = sha256(license);

    const pkgbuildPath = resolve(ROOT, args.pkgbuild);
    let source = readFileSync(pkgbuildPath, "utf8");
    source = source.replace(/^pkgver=.*$/mu, `pkgver=${version}`);
    source = source.replace(/^pkgrel=.*$/mu, "pkgrel=1");
    for (const arch of ARCHITECTURES) {
      source = replaceArray(source, `sha256sums_${arch}`, [archives[arch].sha256, licenseSha]);
    }
    writeFileSync(pkgbuildPath, source);
    console.log(`rewrote ${args.pkgbuild} for ${version}`);

    // .SRCINFO is what the AUR actually parses; a stale one hides the version
    // bump from every AUR helper.
    const srcinfo = runMakepkg(["--printsrcinfo"], dirname(pkgbuildPath));
    // The checked-in template carries `SKIP` so a maintainer can dry-run it
    // before the assets exist. That placeholder must never reach the AUR: a
    // published `SKIP` would make every user's `makepkg` accept whatever bytes
    // the release URL happens to serve.
    if (/^\s*sha256sums_\w+ = SKIP$/mu.test(srcinfo)) {
      fail("refusing to write a .SRCINFO that still carries a SKIP checksum");
    }
    for (const arch of ARCHITECTURES) {
      if (!srcinfo.includes(archives[arch].sha256)) {
        fail(`.SRCINFO has no ${arch} payload checksum`);
      }
    }
    writeFileSync(join(dirname(pkgbuildPath), ".SRCINFO"), `${srcinfo}\n`);
    if (packageVersionFromSrcinfo(srcinfo) !== version) {
      fail(`generated .SRCINFO reports ${packageVersionFromSrcinfo(srcinfo)} instead of ${version}`);
    }
    console.log(`regenerated .SRCINFO for ${version}`);

    if (args.dryRun || args.skipPush) {
      console.log(args.dryRun ? "dry run: not publishing" : "skip-push: not publishing");
      return;
    }

    // Publishing is a clone-copy-commit-push of exactly the two files the AUR
    // tracks. Anything else in the staging tree would be committed by accident.
    const checkout = join(staging, PACKAGE);
    run("git", ["clone", `ssh://aur@aur.archlinux.org/${PACKAGE}.git`, checkout]);
    for (const file of ["PKGBUILD", ".SRCINFO"]) {
      writeFileSync(join(checkout, file), readFileSync(join(dirname(pkgbuildPath), file)));
    }
    run("git", ["add", "PKGBUILD", ".SRCINFO"], { cwd: checkout });
    run("git", ["commit", "-m", `upgpkg: ${PACKAGE} ${version}-1`, "-m", `Release ${version}`], {
      cwd: checkout,
      env: {
        ...process.env,
        GIT_AUTHOR_NAME: process.env.AUR_GIT_NAME || "PeatBoy",
        GIT_AUTHOR_EMAIL: process.env.AUR_GIT_EMAIL || "peatboy@outlook.com",
        GIT_COMMITTER_NAME: process.env.AUR_GIT_NAME || "PeatBoy",
        GIT_COMMITTER_EMAIL: process.env.AUR_GIT_EMAIL || "peatboy@outlook.com"
      }
    });
    run("git", ["push", "origin", "HEAD:master"], { cwd: checkout });
    console.log(`published ${PACKAGE} ${version}-1 to the AUR`);
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});

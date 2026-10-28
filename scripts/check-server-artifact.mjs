#!/usr/bin/env node
// Check a headless server binary before it is packaged for a release.
//
// The release contract is:
//   * the ELF machine matches the requested architecture;
//   * the binary does not need libdbus or libsystemd (the OS keychain is
//     opt-in and the server leaves it out);
//   * every referenced GLIBC symbol is at most the supported floor, which is
//     2.35 (Ubuntu 22.04). Building on Debian 12 is not enough on its own:
//     its glibc is 2.36, so a dependency could silently raise the floor.
//
// Usage:
//   node scripts/check-server-artifact.mjs --binary <path> --arch <x86_64|aarch64> [--max-glibc 2.35]

import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";

const MACHINES = {
  x86_64: /X86-64/i,
  aarch64: /AArch64/i
};

function fail(message) {
  console.error(`check-server-artifact: ${message}`);
  process.exit(1);
}

function parseArguments(argv) {
  const values = { maxGlibc: "2.35" };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag.startsWith("--") || value === undefined) {
      fail(`invalid argument ${flag ?? ""}`.trim());
    }
    index += 1;
    switch (flag) {
      case "--binary":
        values.binary = value;
        break;
      case "--arch":
        values.arch = value;
        break;
      case "--max-glibc":
        values.maxGlibc = value;
        break;
      default:
        fail(`unknown argument ${flag}`);
    }
  }
  if (!values.binary || !values.arch) {
    fail("--binary and --arch are required");
  }
  if (!MACHINES[values.arch]) {
    fail(`--arch must be one of ${Object.keys(MACHINES).join(", ")}`);
  }
  if (!/^\d+\.\d+$/.test(values.maxGlibc)) {
    fail("--max-glibc must look like 2.35");
  }
  return values;
}

function readelf(args) {
  try {
    return execFileSync("readelf", args, {
      encoding: "utf8",
      env: { ...process.env, LC_ALL: "C" },
      maxBuffer: 64 * 1024 * 1024
    });
  } catch (error) {
    fail(`readelf ${args.join(" ")} failed: ${error.message}`);
  }
}

function maxGlibcVersion(text) {
  const versions = new Map();
  for (const match of text.matchAll(/GLIBC_(\d+)\.(\d+)/g)) {
    const major = Number(match[1]);
    const minor = Number(match[2]);
    versions.set(`${major}.${minor}`, { major, minor });
  }
  let highest = null;
  for (const version of versions.values()) {
    if (
      highest === null ||
      version.major > highest.major ||
      (version.major === highest.major && version.minor > highest.minor)
    ) {
      highest = version;
    }
  }
  return highest;
}

function compareVersion(left, right) {
  if (left.major !== right.major) return left.major - right.major;
  return left.minor - right.minor;
}

const { binary, arch, maxGlibc } = parseArguments(process.argv.slice(2));
if (!existsSync(binary)) {
  fail(`binary does not exist: ${binary}`);
}

const header = readelf(["-h", binary]);
if (!MACHINES[arch].test(header)) {
  fail(`binary is not ${arch}: ${header.match(/Machine:\s+(.+)/)?.[1]?.trim() ?? "unknown machine"}`);
}

const dynamic = readelf(["-d", binary]);
for (const forbidden of ["libdbus", "libsystemd"]) {
  if (new RegExp(`NEEDED[^\\n]*${forbidden}`, "i").test(dynamic)) {
    fail(`binary links ${forbidden}; the headless keychain split regressed`);
  }
}

const versions = readelf(["--version-info", binary]);
const highest = maxGlibcVersion(versions);
if (highest === null) {
  fail("binary has no GLIBC symbol references");
}
const [limitMajor, limitMinor] = maxGlibc.split(".").map(Number);
const limit = { major: limitMajor, minor: limitMinor };
if (compareVersion(highest, limit) > 0) {
  fail(
    `binary requires GLIBC_${highest.major}.${highest.minor}, above the ${maxGlibc} floor`
  );
}

const needed = [...dynamic.matchAll(/NEEDED[^\n]*\[([^\]]+)\]/g)].map((match) => match[1]);
console.log(
  JSON.stringify(
    {
      binary,
      arch,
      maxGlibc: `${highest.major}.${highest.minor}`,
      needed
    },
    null,
    2
  )
);

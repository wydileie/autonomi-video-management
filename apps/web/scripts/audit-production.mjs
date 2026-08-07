import { spawnSync } from "node:child_process";
import { pathToFileURL } from "node:url";

export const allowedAdvisories = new Map([
  [
    "react-router",
    new Map([
      [
        "GHSA-qwww-vcr4-c8h2",
        {
          expiresOn: "2026-12-31",
          reason:
            "Only affects unstable React Router RSC APIs, which this SPA does not use; remove when 8.3.0 is published on npm.",
        },
      ],
    ]),
  ],
]);

function advisoryId(via) {
  return via.url?.match(/GHSA-[a-z0-9-]+/i)?.[0];
}

export function findUnapprovedPackages(
  vulnerabilities,
  allowlist = allowedAdvisories,
  asOf = new Date().toISOString().slice(0, 10),
) {
  const memo = new Map();

  function hasUnapprovedAdvisory(packageName, visiting = new Set()) {
    if (memo.has(packageName)) return memo.get(packageName);
    // npm currently emits an acyclic advisory graph. Treat malformed cycles as
    // unapproved so an unexpected audit shape cannot weaken the security gate.
    if (visiting.has(packageName)) return true;

    const vulnerability = vulnerabilities[packageName];
    if (!vulnerability || !Array.isArray(vulnerability.via)) return true;

    const nextVisiting = new Set(visiting).add(packageName);
    const unapproved = vulnerability.via.some((via) => {
      if (typeof via === "string") {
        return hasUnapprovedAdvisory(via, nextVisiting);
      }

      const id = advisoryId(via);
      const approval = id && allowlist.get(packageName)?.get(id);
      return (
        !approval || !/^\d{4}-\d{2}-\d{2}$/.test(approval.expiresOn) || approval.expiresOn < asOf
      );
    });

    memo.set(packageName, unapproved);
    return unapproved;
  }

  return Object.keys(vulnerabilities).filter((packageName) => hasUnapprovedAdvisory(packageName));
}

function run() {
  const audit = spawnSync("npm", ["audit", "--omit=dev", "--json"], {
    encoding: "utf8",
    shell: false,
  });

  if (audit.error) {
    console.error(`Unable to run npm audit: ${audit.error.message}`);
    process.exit(1);
  }

  let report;
  try {
    report = JSON.parse(audit.stdout);
  } catch (error) {
    console.error("npm audit did not return valid JSON.");
    if (audit.stderr) console.error(audit.stderr.trim());
    console.error(error);
    process.exit(1);
  }

  const vulnerabilities = report.vulnerabilities ?? {};
  if (report.error || (audit.status !== 0 && Object.keys(vulnerabilities).length === 0)) {
    console.error("npm audit failed before producing a vulnerability report.");
    if (report.error) console.error(JSON.stringify(report.error));
    if (audit.stderr) console.error(audit.stderr.trim());
    process.exit(1);
  }

  const failingPackages = findUnapprovedPackages(vulnerabilities);
  if (failingPackages.length > 0) {
    console.error(
      `npm audit found unapproved production vulnerabilities in: ${failingPackages.join(", ")}`,
    );
    for (const packageName of failingPackages) {
      for (const via of vulnerabilities[packageName].via ?? []) {
        if (typeof via !== "string") {
          console.error(`- ${via.severity}: ${via.title} (${via.url})`);
        }
      }
    }
    process.exit(1);
  }

  if (Object.keys(vulnerabilities).length === 0) {
    console.log("npm audit found no production vulnerabilities.");
    return;
  }

  console.warn("npm audit reported only explicitly reviewed production advisories:");
  for (const [packageName, advisories] of allowedAdvisories) {
    for (const [id, approval] of advisories) {
      console.warn(`- ${packageName} ${id} (expires ${approval.expiresOn}): ${approval.reason}`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  run();
}

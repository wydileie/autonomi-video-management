import assert from "node:assert/strict";
import test from "node:test";

import { findUnapprovedPackages } from "./audit-production.mjs";

const approvedAdvisory = {
  severity: "high",
  title: "RSC Mode CSRF Bypass",
  url: "https://github.com/advisories/GHSA-qwww-vcr4-c8h2",
};

test("allows the reviewed advisory and its transitive dependent", () => {
  const vulnerabilities = {
    "react-router": { via: [approvedAdvisory] },
    "react-router-dom": { via: ["react-router"] },
  };

  assert.deepEqual(findUnapprovedPackages(vulnerabilities), []);
});

test("rejects a new direct advisory and its transitive dependent", () => {
  const vulnerabilities = {
    dependency: {
      via: [
        {
          severity: "high",
          title: "Unexpected vulnerability",
          url: "https://github.com/advisories/GHSA-aaaa-bbbb-cccc",
        },
      ],
    },
    parent: { via: ["dependency"] },
  };

  assert.deepEqual(findUnapprovedPackages(vulnerabilities), ["dependency", "parent"]);
});

test("rejects an additional advisory on an otherwise approved package", () => {
  const vulnerabilities = {
    "react-router": {
      via: [
        approvedAdvisory,
        {
          severity: "moderate",
          title: "New advisory",
          url: "https://github.com/advisories/GHSA-dddd-eeee-ffff",
        },
      ],
    },
  };

  assert.deepEqual(findUnapprovedPackages(vulnerabilities), ["react-router"]);
});

test("accepts an empty vulnerability report", () => {
  assert.deepEqual(findUnapprovedPackages({}), []);
});

test("rejects malformed advisory data", () => {
  assert.deepEqual(findUnapprovedPackages({ dependency: { via: [{}] } }), ["dependency"]);
  assert.deepEqual(findUnapprovedPackages({ dependency: {} }), ["dependency"]);
});

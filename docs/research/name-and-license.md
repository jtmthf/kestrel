# Name collisions and the license landscape

Resolves [#6](https://github.com/jtmthf/kestrel/issues/6). Findings only — the decisions belong to
[#9](https://github.com/jtmthf/kestrel/issues/9). Queried 2026-08-26.

## Part 1 — the name

### Registry and namespace availability: everything is taken

All checked by live query, not recall.

| Namespace | Status | Detail |
|---|---|---|
| **npm `kestrel`** | **TAKEN** | v0.0.1, "Node.js client for Kestrel", last published **2011-06-28**. Abandoned, but it blocks the name. |
| **PyPI `kestrel`** | **TAKEN** | v0.6.1 — *"A fast, efficient inference engine for multimodal models"*. **An active AI project.** |
| **crates.io `kestrel`** | **TAKEN** | v0.0.1, "Actuarial Modeling in Rust", 1,673 downloads. Effectively dead. |
| **GitHub user/org `kestrel`** | **TAKEN** | Existing user account, 0 public repos. Org name unavailable. |

### Domains: all registered

Every plausible candidate resolves to live nameservers:

`kestrel.dev`, `kestrel.sh`, `kestrelhq.com`, `getkestrel.com`, `kestrel.build`, `usekestrel.com`
— **all registered.** Several sit on Cloudflare or Vercel nameservers, suggesting active use or
deliberate holding rather than lapsed registrations.

### The name is more crowded than "the ASP.NET thing"

Charting assumed one collision. GitHub's own search, by stars, shows at least four established meanings:

| Stars | Repo | What it is |
|---|---|---|
| 2,753 | `twitter-archive/kestrel` | Twitter's distributed message queue (inactive) |
| 2,630 | `aspnet/KestrelHttpServer` | ASP.NET Core's web server (archived repo; the server itself lives on inside ASP.NET Core and is enormously deployed) |
| 323 | `opencybersecurityalliance/kestrel-lang` | **Kestrel threat hunting language — active, and in security** |
| 189 | `KestrelComputer/kestrel` | A homebrew computer family |

So "kestrel" in a developer context already means: a message queue, a web server, a threat-hunting DSL,
a hobby computer — **and, on PyPI, a multimodal inference engine.** That last one is the most
uncomfortable, because it is in AI and therefore in kestrel's own search neighbourhood.

**Assessment for [#9](https://github.com/jtmthf/kestrel/issues/9):** the cost is not one famous collision,
it is that every distribution channel a new infrastructure project would want is already occupied, the
GitHub org is gone, and no obvious domain is free. A rename now costs nothing; the same rename after the
project has users costs documentation, package names, and search equity.

## Part 2 — the license

### What comparable projects actually ship (GitHub API, 2026-08-26)

| Project | SPDX per GitHub | Stars |
|---|---|---|
| `hashicorp/terraform` | **NOASSERTION** | 49,543 |
| `opentofu/opentofu` | MPL-2.0 | 29,938 |
| `redis/redis` | **NOASSERTION** | 76,114 |
| `valkey-io/valkey` | BSD-3-Clause | 26,991 |
| `elastic/elasticsearch` | **NOASSERTION** | 77,863 |
| `grafana/grafana` | AGPL-3.0 | 76,439 |
| `gitpod-io/gitpod` | AGPL-3.0 | 13,754 |
| `temporalio/temporal` | MIT | 22,549 |

**A concrete, under-appreciated finding: every source-available project returns `NOASSERTION`.**
GitHub's license detector does not recognise BSL, SSPL, or RSAL as a license. That is not cosmetic —
it means automated tooling, dependency scanners, and license-policy gates see "unknown license," which
is precisely the worst answer to give an enterprise legal review. Since kestrel's north-star audience is
platform engineering teams at mid-size orgs, this argues directly against the source-available hedge.

### The relicensing case studies

The three canonical events, and what followed:

- **HashiCorp Terraform** — MPL-2.0 → **BSL 1.1**, August 2023. Forked as **OpenTofu** under the Linux
  Foundation, now MPL-2.0 with ~30k stars.
- **Redis** — BSD-3 → **RSALv2 / SSPLv1**, March 2024. Forked as **Valkey**, backed by AWS, Google Cloud,
  Oracle, Ericsson and the Linux Foundation; BSD-3-Clause, ~27k stars. Redis **added AGPLv3 in May 2025**,
  a partial reversal.
- **Elasticsearch** — Apache-2.0 → **SSPL / Elastic License**, January 2021, driven by AWS competition.
  Forked as **OpenSearch**. Elastic **added AGPL in 2024**, also a partial reversal.

Reported consequences, from secondary sources — [CHAOSS](https://chaoss.community/what-happens-to-relicensed-open-source-projects-and-their-forks/),
[an arXiv study on relicensing and forks](https://arxiv.org/pdf/2411.04739),
[InfoWorld](https://www.infoworld.com/article/3975620/redis-bets-big-on-an-open-source-return/) —
**treat the specific figures with caution; they are not primary and I did not verify them independently:**

- Redis is reported to have lost most external contributors: before the fork, 12 non-employees made 54%
  of commits; afterwards, reportedly zero non-employees exceeded 5 commits.
- Valkey is reported at high enterprise adoption with roughly double Redis's PR rate.
- HashiCorp's change is described as the most damaging to community trust, because IaC tooling was felt
  to be part of open-source identity.

**The pattern that matters is structural, not statistical, and it is consistent across all three:**
every successful fork had cloud-provider backing plus foundation governance; every partial reversal came
*after* the fork had already taken the contributors; **no project that relicensed got its community back.**

### Reading each option against kestrel

| | Cloud vendor may host it | Enterprise legal review | Outside contribution | Notes |
|---|---|---|---|---|
| **MIT / Apache-2.0** | Yes | Easiest; Apache-2.0 adds an explicit patent grant | Strongest | Apache-2.0 is the usual choice for infra wanting corporate contributors |
| **AGPL-3.0** | Yes, but network use triggers copyleft | Many enterprises have blanket AGPL bans — a real adoption tax on a self-hosted product | Good, though CLA-dependent | Grafana and Gitpod both live here |
| **BSL / SSPL / RSAL** | No (that's the point) | **Worst** — plus `NOASSERTION` in tooling | Chilling; invites a fork | Every case study above ended in a fork |

**On the charted reasoning that ruled out BSL:** kestrel declared "not a hosted SaaS" a non-goal, which
removes the usual motive for a source-available license — you only need to stop cloud vendors reselling
you if you were planning to sell it yourself. That reasoning **holds**, with one honest caveat: it is a
bet that the non-goal survives. Every project above adopted BSL *after* commercial pressure appeared,
not at founding. The relevant lesson is that relicensing later is the expensive, trust-destroying move —
so the license choice should be one you would still accept if commercial pressure arrives.

**CLA vs DCO:** a CLA (copyright assignment or broad grant) is what makes future relicensing *possible* —
it is the mechanism, and contributors increasingly read it as a signal of intent. A DCO is a lightweight
attestation with no relicensing power. Choosing Apache-2.0 + DCO is the strongest "we will not pull the
rug" signal; Apache-2.0 + CLA deliberately keeps the option open, at the cost of some trust.

*Not legal advice — these are tradeoffs, not verdicts.*

## Open questions for [#9](https://github.com/jtmthf/kestrel/issues/9)

- If the name changes, the replacement needs the same availability sweep run before it is committed to.
- Open-Inspect, kestrel's closest analogue ([#5](https://github.com/jtmthf/kestrel/issues/5)), ships
  **MIT**. Matching or exceeding its permissiveness may be table stakes for contributors choosing between
  them.
- Does kestrel want a foundation path eventually? That choice constrains the license now.

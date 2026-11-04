# Models for the harness

Which models to put in each of the harness's tiers (frontier, balanced, fast)
for each backend, and which tier each kind of task should get. This is the
research behind TermOS's defaults ([harness.md](harness.md)), as of
4 November 2026: prices, context windows and benchmark evidence for each
provider, then the recommendations. Models change every few months; check a
provider's own list (`termos agent models PROVIDER`) before you rely on a
name here. TermOS itself names no model: you fill the tiers.

Every number has a source number in `[n]`; the sources are at the end. Prices
are US dollars per million tokens (MTok), seen on 2026-11-04 unless the entry
says otherwise.

**How to read the evidence.**

- **F**: the page was fetched and read directly.
- **R**: read from a source repository at a pinned commit.
- **S**: seen only through web-search result summaries; the research could not fetch
  `developers.openai.com`, `platform.openai.com` or `ai.google.dev` directly. Check S
  facts against the page before relying on them. The source list marks each source.
- **Vendor**: a vendor-reported benchmark. These use the vendor's own harness and effort
  setting, so they don't compare well across vendors.
- **Vals TB4**: Vals AI's Terminal-Bench 4.0 run, the one cross-vendor table in this guide
  where every model ran in the same harness (mini-swe-agent, 3 runs per model, mean score,
  board updated 2026-10-29) [56]. It also reports cost per task, and its scores sit below most
  vendor numbers.
- **⚠**: the sources disagree or the claim is uncertain.

---

## 1. Benchmark landscape in late 2026 (read this first)

- **SWE-bench Verified is no longer a frontier signal.** In February 2026 OpenAI stopped
  reporting it because of contamination and test-design problems, and it recommends SWE-bench
  Pro instead [27]. Anthropic's Opus 5.5 table doesn't include it [3]. Numbers still published
  for it are mostly open-weight launch claims, such as Mistral Medium 3.5 at 77.6% [47] and
  DeepSeek V4-Pro at 80.6% [40].
- **SWE-bench Pro (Scale) is close to saturated on its public set.** The V2 public board has
  Claude Opus 5 at 99.4%. Scale's private split is lower: 222 of 272 tasks solved, against
  638 of 642 on the public set [57].
- **Aider polyglot is stale.** The official board was last updated in November 2025, with
  gpt-5 (high) on top at 88.0%. It has no 2026 models [58].
- **The current cross-vendor numbers** are Terminal-Bench 4.0 (TB4, 66 hard terminal
  workflows), DeepSWE v1.1 (Datacurve, long-horizon tasks [63]), FrontierCode 1.1 and
  CursorBench 4.0. Terminal-Bench 2.x is near its ceiling (many models score 85–90%), so it
  hardly separates models any more.
- **Vendor and independent TB4 scores differ.**

  | Model | Vendor TB4 | Vals TB4 |
  |---|---|---|
  | Claude Sonnet 5.5 | 70.6% [4] | 64.14% [56] |
  | Grok 4.7 | 46.0% [51] | 28.79% [56] |

  Use the Vals TB4 numbers for cross-provider comparisons and vendor numbers only within one
  vendor's lineup.

**Vals TB4 snapshot.** These are the models this guide covers [56]. Rank is out of 59
entries; the source gave ranks only for some models. "$/task" is Vals' cost per test.

| Model | Score | $/task | Rank |
|---|---|---|---|
| Claude Opus 5.5 | 65.15% | $13.20 | 1 |
| Claude Sonnet 5.5 | 64.14% | $16.51 | |
| Kimi K3.1 Pro | 63.64% | $4.57 | 3 |
| Gemini 4 Carbon (limited access) | 63.13% | $7.45 | |
| GPT-6 Astra | 59.60% | $9.58 | |
| Gemini 4 Argon | 57.58% | $17.64 | |
| GPT-6.1 Sol | 55.05% | $1.72 | 9 |
| GLM 5.3 | 38.89% | $9.37 | |
| Qwen 3.8 Max 0902 | 38.89% | $12.15 | |
| GLM 5.3 Flash | 37.37% | $1.22 | |
| Claude Haiku 5.5 | 36.36% | $3.12 | 19 |
| Qwen 3.8 Max | 34.34% | $10.63 | 20 |
| DeepSeek V4.1 Flash 1017 | 31.82% | $0.54 | |
| Grok 4.7 | 28.79% | $18.01 | 24 |
| Mistral Large 4 | 26.26% | $2.73 | 26 |
| Gemini 3.8 Flash | 19.70% | $8.65 | 34 |
| GPT-6 Luna | 13.64% | $0.35 | 39 |

Cost per task does not track price per token: on this board Opus 5.5 ($4/$20 per MTok) cost
less per task than Sonnet 5.5 ($2/$10) [56][2]. Judge a tier filling by cost per completed
task, not by list price.

---

## 2. Providers

### 2.1 Anthropic (API, and Claude Code under a subscription)

| Model id | Tier | Context / max out | Price in / out | Cache read | Status |
|---|---|---|---|---|---|
| `claude-fable-5-1` | frontier+ | 1M / 128K | $10 / $50 | $0.25 | current [1][2] |
| `claude-opus-5-5` | frontier | 1M / 128K | $4 / $20 | $0.20 | current, released 2026-09-22 [1][3] |
| `claude-sonnet-5-5` | balanced | 1M / 128K | $2 / $10 | $0.10 | current, released 2026-09-28 [1][4] |
| `claude-haiku-5-5` | fast | 1M / 128K | $0.10 / $0.50 (prompt ≤100K); $0.50 / $2.50 (>100K) | $0.01 / $0.05 | current, released 2026-10-07 [6] |

**Pricing details** [2]. Cache writes cost 1.25× base input for the 5-minute TTL and 2× for
the 1-hour TTL. Cache reads cost 0.05× on Opus 5.5 and Sonnet 5.5, 0.025× on Fable 5.1 and
0.1× elsewhere. Batch is 50% off. 1M context costs the standard rate on all except Haiku 5.5.
Fast mode on Opus 5.5 costs $8 / $40. `inference_geo: "us"` multiplies prices by 1.1.

**Thinking and effort.** Thinking is always on for Fable 5.1 and Opus 5.5, so effort is the
only control. Default effort on the API is `medium` for Opus 5.5 and Haiku 5.5, and `high` for
Fable 5.1 and Sonnet 5.5 [1].

**Benchmarks** (all Vendor).

Opus 5.5 at max effort, unless noted [3]:

| Benchmark | Opus 5.5 | Fable 5.1 | GPT-6 Astra |
|---|---|---|---|
| TB4 (Opus 5.5 at xhigh) | 66.4% | 55.8% | 57.9% |
| FrontierCode 1.1 Main | 54.4% | 50.3% | 53.3% |
| CursorBench 4.0 | 57.8% | 51.8% | n/a |

- Opus 5.5 at its default `medium` effort scores 54.6% on FrontierCode and 52.5% on
  CursorBench [3].
- DeepSWE v1.1: Opus 5.5 scores 74.2%. This figure is from Google's table [31].

- Sonnet 5.5 [4]: TB4 70.6%; FrontierCode 1.1 Main 52.1% at xhigh, 46.2% at max;
  CursorBench 4.0 55.5%.
- Haiku 5.5 [5]: TB4 39.2%; FrontierCode 1.1 Main 46.4%.
- Vals TB4 [56]: Opus 5.5 65.15%, Sonnet 5.5 64.14%, Haiku 5.5 36.36%.

**Strengths and weaknesses by task.**

- **Planning and review:** Opus 5.5 is the strongest evidenced choice. Anthropic positions it
  for long-running agentic coding and singles out "codebase-wide migrations and audits" [3].
  In a Deloitte quote on Anthropic's page, Opus 5.5 at its *lowest* effort "caught 72% of known
  bugs in our code reviews to Opus 5's 56% at high effort" [3]. That is customer-reported,
  not a benchmark.
- **Fable 5.1:** Anthropic recommends it only "when your evals on Claude Opus 5.5 at higher
  effort still fall short" [1]. It scores below Opus 5.5 on every coding row above, at 2.5×
  the price, so it is not a default frontier filling.
- **Coding and tests:** Sonnet 5.5 is within about 1–2 points of Opus 5.5 on CursorBench and
  Vals TB4 [4][56]. It is "30%+ faster" than Sonnet 5 [4]. One caveat: at max effort it ran
  Claude Code's code-review skill, which "splits the review across many subagents", causing
  timeouts. Anthropic attributes its lower max-effort FrontierCode score to this, so cap
  Sonnet at xhigh for review and tests [4].
- **Search, small-edits and docs:** Haiku 5.5 "pairs well with Opus 5.5 and Sonnet 5.5 as a
  subagent on coding work". It is best for narrowly scoped work (compaction, summarization,
  subagents). Anthropic says Sonnet and Opus "remain better choices for complex agentic
  coding" [5]. Its price rises 5× once a prompt exceeds 100K tokens [6], so keep its contexts
  small.

**No gaps:** Anthropic has a good fit for every tier.

### 2.2 OpenAI (API, and Codex CLI under a ChatGPT plan)

| Model id | Tier | API context / max out | Price in / cached / out | Notes |
|---|---|---|---|---|
| `gpt-6-astra` | frontier | 1.05M / 128K [18] | $10 / $1 / $50 [17] | Above 272K input: 2× input and cache, 1.5× output [18][19] |
| `gpt-6.1-sol` | balanced | 1.05M / 128K [19] | $2 / $0.10 / $10 [19][22] | OpenAI: "Near-Astra performance for complex work at a lower cost" [16] |
| `gpt-6.1-terra` | balanced-cheap | 1.05M / 128K [20] ⚠ | $1 / $0.05 / $5 [17][20] | No dedicated model page found ⚠ |
| `gpt-6-luna` | fast | 1.05M / 128K [20] ⚠ | $0.10 / $0.01 / $0.50 [17] | "Strong performance for focused, high-volume tasks" [16] |
| `gpt-6-sol` | previous balanced | n/a | n/a | Superseded by `gpt-6.1-sol` [16] |

**Pricing and API details.**

- Batch and Flex cost half the standard rate [17]. Fast mode costs 2× [17][19].
- Prompt caching on GPT-5.6 and later [26]:
  - cache writes cost 1.25× input for the default 30-minute TTL;
  - a 12-hour TTL costs 2.5× (Responses API only);
  - reads cost 0.1×, or 0.05× on GPT-6.1 Sol and Terra.
- Astra and Sol 6.1 accept reasoning effort `low` through `max`; `none` returns HTTP 400.
  Tool calling needs the Responses API [16][18].

⚠ **Availability conflict:** OpenAI's Enterprise rate card says GPT-6.1 Sol, GPT-6.1 Terra and
GPT-6 Luna "remain available only in ChatGPT Work and Codex". The API pricing and model pages
list them as API models [17][19][20]. Test API access before relying on them.

**Benchmarks** (Vendor, from OpenAI's Astra page [21]):

| Benchmark | GPT-6 Astra | GPT-5.6 Sol |
|---|---|---|
| TB4 | 57.9% | 37.3% |
| DeepSWE v1.1 | 74.1% | 72.7% |
| FrontierCode 1.1 Main | 53.3% | n/a |

- GPT-6.1 Sol's launch post reports no SWE-bench Pro or TB4 number found by this search. It
  says Sol "matches GPT-6 Astra" on DeepSWE v1.1 at about one fifth of the cost [22] ⚠.
- Anthropic's Haiku page lists GPT-6 Luna at 16.4% on TB4 and 42.4% on FrontierCode Main [5].
- Vals TB4 [56]: Astra 59.60% ($9.58), Sol 6.1 55.05% ($1.72), Luna 13.64% ($0.35).

**Strengths and weaknesses by task.**

- **Planning and review:** Astra; the OpenAI docs describe it as "For the most demanding
  reasoning, coding, and professional work" [16]. OpenAI says Astra at Low effort "can
  outperform Sol at High effort", and recommends starting Astra at Low or Medium [24].
- **Coding and tests:** GPT-6.1 Sol has the best price/performance on Vals TB4 among models
  above 50%: 55% at $1.72 per task [56].
- **Search, small-edits and docs:** Luna's agentic score is weak (13.64% on Vals TB4 [56]), so
  use it for read-only search, summaries and docstrings, not multi-step edits. Codex itself
  uses `gpt-6-luna` for background memory extraction and `gpt-5.6-luna` for thread titles
  [15], which matches that role.

### 2.3 Google (Gemini API)

| Model id | Tier | Context / max out | Price in / cached / out | Notes |
|---|---|---|---|---|
| `gemini-4-argon` | frontier | 1,048,576 shared input+thinking+output; default max output 65,536 [30] | $2 / $0.10 / $10 until 2026-11-30, then $4 / $0.20 / $20 from 2026-12-01 [29] | Thinking low/medium/high, default medium, can't be disabled; implicit caching only [30] |
| `gemini-3.8-flash` | fast | 1,048,576 / 65,536 [32] | $0.75 / $0.075 / $3.75 until 2026-12-31, then $1.50 / $0.15 / $7.50 [29][32] | Batch is half price [29] |
| Gemini 4 Carbon | none yet | n/a | announced at the Argon introductory price [33] | Access limited to cyber defenders and partners; no public API id [33] |
| `gemini-3.1-pro-preview` | legacy | 1M [29] | $2 / $12 (≤200K) [29] | Still listed with no shutdown date [34] |

**Benchmarks.**

- Gemini 4 Argon (Vendor [31]):
  - DeepSWE v1.1: 77.9%, against Opus 5.5 at 74.2% and Astra at 74.1%.
  - TB4: 57.4%, self-computed; Opus 5.5 66.4%, Astra 58.2%.
  - FrontierSWE v2: 55.0%, against Astra at 65.5%.
- Gemini 3.8 Flash (Vendor [32]): DeepSWE v1.1 73.7%; TB4 19.1%; Terminal-Bench 2.1 89.4% on
  the model card, but ⚠ another Google page reports 90.8%.
- Vals TB4 [56]: Argon 57.58% at $17.64 per task (the most expensive per task of the top
  group); 3.8 Flash 19.70% at $8.65.

**Strengths and weaknesses by task.**

- **Argon:** strong on long-horizon software engineering (DeepSWE) but behind on terminal
  agency (TB4). Reasonable as a *cross-model reviewer* or planner; poor value as an
  autonomous coding loop on current prices.
- **3.8 Flash:** has a 1M context but scores low on TB4. Use it for search and summaries only.

**Gaps:** Google has no distinct balanced-tier model. Use Argon at `low` thinking, or skip
Google for the balanced tier. ⚠ A Flash-Lite id appears only in an API listing this research
couldn't verify, so it is omitted.

### 2.4 Moonshot (Kimi)

| Model id | Tier | Context | Price in / cache hit / out | Notes |
|---|---|---|---|---|
| `kimi-k3.1-pro` | frontier-value / balanced | 1M [35] | $2 / $0.20 / $10 [35] | `kimi-k3.1-pro-highspeed` costs $4 / $0.20 / $20 [35] |
| `kimi-k3` | previous flagship | 1M [35] | $3 / $0.30 / $15; cache writes $3 (5m) / $6 (1h) [35] | |
| `kimi-k2.7-code` | fast (coding) | 256K [35] | $0.95 / $0.19 / $4.00 ⚠ [37] | Price from a Kimi resources page, not the platform pricing page ⚠ |

**Pricing details.** Caching is automatic, with a default 5-minute TTL [35]. The K3 series
gets 60% of list price on batch [35]. ⚠ The K2 series retirement date conflicts across
Moonshot pages: one gives 2026-05-25, another the end of December 2026 [35].

**Benchmarks.**

- Vendor: K3.1 Pro scores 65.7% on TB4 with the Claude Code harness [36].
- Vals TB4: 63.64% at $4.57 per task, rank 3 of 59. That is about a third of Opus 5.5's cost
  per task for 1.5 points less [56].
- Moonshot said K3.1 Pro's open weights would ship by 2026-11-04. ⚠ This research could not
  confirm the release [36].

**Strengths.** It is the best value among frontier-level agentic coders on Vals TB4 [56], so it
is a strong API filling for coding or as a second-opinion reviewer. Little evidence was found
on its planning or review quality specifically.

### 2.5 DeepSeek

| Model id | Tier | Context / max out | Price in (miss) / hit / out, off-peak → peak | Notes |
|---|---|---|---|---|
| `deepseek-flash` | fast | 1M / 384K [38] | $0.15 → $0.30 / $0.003 → $0.006 / $0.60 → $1.20 [38] ⚠ | DeepSeek-V4.1-Flash-1017 since 2026-10-17 [39] |
| `deepseek-v4-pro` | balanced | 1M / 384K [38] | $0.66 → $1.32 / $0.022 → $0.044 / $1.98 → $3.96 [38] ⚠ | |

**Pricing details.**

- Off-peak prices are half of peak; the current prices took effect on 2026-08-16 [38].
- ⚠ DeepSeek's platform page lists different numbers [41]: Flash $0.14 miss / $0.0028 hit /
  $0.28 output; V4-Pro $0.435 / $0.003625 / $0.87.
- Caching is automatic [41].
- `deepseek-v4-flash` is still accepted and is served by V4.1-Flash-1017 [39].
- `deepseek-chat` and `deepseek-reasoner` were scheduled for retirement after 2026-07-24 [39].

**Benchmarks.**

- Vendor, at V4-Pro's April 2026 launch, Max mode [40]:
  - SWE-bench Verified: 80.6.
  - Terminal-Bench 2.0: 67.9.
  - LiveCodeBench: 93.5.
  - SWE-bench Pro: 55.4.
- Vals TB4 [56]: V4.1 Flash 1017 scores 31.82% at $0.54 per task. No V4-Pro TB4 figure was
  found.

**Strengths.** Very cheap tokens with deep cache discounts (a cache hit costs about 1/50 of a
miss on Flash [38]). That makes Flash a good fast tier for search, summaries and small
edits. **Gaps:** no frontier fit; agentic scores are well below the top group [56].

### 2.6 Alibaba Qwen (API and local)

| Model id | Tier | Context / max out | Price in / out | Notes |
|---|---|---|---|---|
| `qwen3.8-max` (snapshot `qwen3.8-max-0902`) | balanced | 1,000,000 (max input 991,808) / 131,072 [43] ⚠ | $2 / $6 (Singapore, US-only); $1.65 / $4.951 (global, Beijing) [43] | The console says 128K max output ⚠; batch 50%, not combinable with cache discount [43] |
| `qwen3-coder-plus` | balanced (older) | up to 1M, tiered [43] | $1 / $5 (≤32K) rising to $6 / $60 (256K–1M) [43] | ⚠ A second "Global" table on the same page shows lower rates |

**Benchmarks.**

- Qwen3.8-Max, Vendor [42]:
  - Terminal-Bench 2.1: 86.6.
  - SWE-bench Pro: 67.7.
  - GPT-5.6 Sol (max) scored 88.8 on TB2.1 in the same table.
- Vals TB4 [56]: Qwen 3.8 Max 34.34% ($10.63); 0902 snapshot 38.89% ($12.15).

**Local open-weight Qwen models.**

- **`Qwen/Qwen3.8-27B`** (dense):
  - 262,144 native context, extensible to 1M with YaRN; the card warns that static YaRN can
    hurt short-text performance [44].
  - Vendor scores: TB2.1 73.0, SWE-bench Pro 61.7, DeepSWE 1.1 42.2 [44].
  - ⚠ The license (Apache 2.0) is stated only on derivative cards.
- **`Qwen/Qwen3.8-Flash-Next`** (MoE): about 125B total and 6B active per token, counting a
  51B n-gram embedding table; 262,144 native context, extensible to 1M; Vendor SWE-bench Pro
  62.5 [45]. ⚠ Community reports say an 85 GB quant runs on a 64 GB Mac, and one user
  reports quality drops after about 150K tokens [45].
- **`Qwen/Qwen3.8-2.4T-A95B`:** open weights of the Max model exist on Hugging Face [42],
  but they are server-class, not local.

**Strengths and weaknesses.**

- The API model scores well on vendor SWE-bench Pro but lower on Vals TB4, and is expensive
  per task [56].
- The Qwen 27B and Flash-Next models are the most plausible *local* balanced or fast fillings
  in late 2026. Their scores are Vendor only; no independent harness numbers were found.

**Gaps:** no frontier fit.

### 2.7 Mistral (Large 4, Devstral, Codestral)

| Model id | Tier | Context | Price in / cached / out | Status |
|---|---|---|---|---|
| `mistral-large-4` (`mistral-large-4-0`) | balanced | 1M [46] ⚠ (OpenRouter listed 512K) | $0.70 / $0.07 / $2.10 [46] ⚠ (the launch page listed $1.36 / $4.18; a promotional rate may apply) | Public preview from 2026-10-06; open weights `mistralai/Mistral-Large-4-1T-A52B` under a modified MIT license [46] |
| `mistral-medium-3-5` | balanced (expiring) | 256K [47] | $1.5 / $7.5 [47] | Deprecation date 2026-10-31, replaced by Large 4 [47] ⚠ status on 2026-11-04 unclear |
| `codestral-2508` | FIM / autocomplete only | n/a | $0.3 / $0.03 / $0.9 [49] | Active, no retirement date [49] |
| `devstral-2512` (Devstral 2), `labs-devstral-small-2512` | deprecated | 256K | $0.40 / $2.00; Small $0.10 / $0.30 [48] | Deprecated on the API (5/22/2026 and 2/27/2026). Devstral Small 2 weights (24B, Apache 2.0) remain usable locally [48] |

**Benchmarks.**

- Large 4, Vendor [46]:
  - DeepSWE v1.1: 61.7%.
  - TB4: 28.3% in the preview; Mistral reports about 41% for the improved open-weight
    version.
- Large 4, Vals TB4 [56]: 26.26% at $2.73.
- Medium 3.5, Vendor: SWE-bench Verified 77.6% [47].

**Strengths.**

- Cheap and open-weight; EU vendor.
- Codestral is purpose-built for fill-in-the-middle completion [49], which the harness tiers
  don't use.

**Gaps:** no frontier fit. Mistral is weak for agentic coding loops on current evidence [56],
so use it only if you need open weights or an EU vendor.

### 2.8 xAI (Grok)

| Model id | Tier | Context | Price in / cached / out | Notes |
|---|---|---|---|---|
| `grok-4.7` | balanced | 500,000 [50] | $2 / $0.50 / $6 (<200K prompt); $4 / $1 / $12 above [50] | Reasoning low/medium/high (default)/xhigh. A US endpoint costs +10%. "Grok 4.7 Fast" is not on the public API [50] |
| `grok-4.3` | fast-ish | 1,000,000 [50] | $1.25 / $0.20 / $2.50 [50] | Cheapest public text model; long-context tier costs more [50] |
| `grok-build-0.1` | coding agent model | 256,000 [50] | $1.00 / $0.20 / $2.00 [50] | No benchmark numbers found |

**Benchmarks.** xAI's model card gives Grok 4.7 46.0% on TB4 at high effort [51]. Vals TB4
gives 28.79% at $18.01 per task, the highest cost per task in the snapshot table [56] ⚠
(large harness gap). **Gaps:** no frontier fit, and poor measured value for coding loops.

### 2.9 Z.ai (GLM)

| Model id | Tier | Context / max out | Price in / cached / out | Notes |
|---|---|---|---|---|
| `glm-5.3` | balanced | 1M / 128K (default `max_tokens` 65,536) [52] | $1.40 / $0.26 / $4.40 [52] | Text-only; OpenAI-compatible base URL `https://api.z.ai/api/paas/v4/` [52] |
| GLM-5.3-Flash (exact API id not confirmed) | fast | 1M / 128K [52] | $0.15 / n/a / $0.50 [52] | Open weights: `zai-org/GLM-5.3` is 744B-A40B, `zai-org/GLM-5.3-Flash` is 320B-A18B [53] |
| GLM-5.3-FlashX (id not confirmed) | fast | n/a | $0.37 / n/a / $1.25 [52] | |

**Benchmarks.**

- Vendor [53]:
  - GLM-5.3: Terminal-Bench 2.1 88.2, Terminal-Bench 3.0 28.3.
  - GLM-5.3-Flash: Terminal-Bench 2.1 84.3.
- Vals TB4 [56]:
  - GLM 5.3: 38.89% at $9.37.
  - **GLM 5.3 Flash: 37.37% at $1.22**, nearly the same score at about one eighth the cost.

**Subscription.** The GLM Coding Plan starts at $18/month and is usable from Claude Code,
Cline and OpenCode [54].

**Strengths.** GLM-5.3-Flash is one of the best-value fast-tier fillings measured [56].
**Gaps:** no frontier fit.

### 2.10 Open-weight models to run locally (llama.cpp)

| Model | Size | Context | Evidence | Fit |
|---|---|---|---|---|
| `openai/gpt-oss-20b` | 20.9B total / 3.6B active, runs in 16 GB [55] | 128k [55] | Aug 2025; "similar to o4-mini" at launch [55] | fast tier on a laptop. Dated. Codex's `--oss` default is `gpt-oss:20b` [15] |
| `openai/gpt-oss-120b` | 116.8B / 5.1B active, fits 80 GB [55] | 128k [55] | same | fast to balanced on a workstation. Dated. No newer gpt-oss release found |
| `Qwen/Qwen3.8-27B` | 27B dense | 262K native [44] | Vendor SWE-bench Pro 61.7, TB2.1 73.0 [44] | local balanced candidate (single large GPU or Mac; quant size not verified) |
| `Qwen/Qwen3.8-Flash-Next` | about 125B total / 6B active [45] | 262K native [45] | Vendor SWE-bench Pro 62.5 [45] | local fast or balanced on 64 GB+ unified memory ⚠ (community claim) |
| Devstral Small 2 | 24B, Apache 2.0 [48] | 256K [48] | superseded on Mistral's API [48] | fallback on a 24 GB GPU |
| `mistralai/Mistral-Medium-3.5-128B` | 128B dense, "as few as four GPUs" [47] | 256K | Vendor SWE-bench Verified 77.6% [47] | server-class |
| GLM-5.3-Flash, DeepSeek V4.1-Flash, Kimi K3 | 320B+ total | | see above | not local for most users |

**What the evidence says about local models.** No independent agentic benchmark (such as Vals
TB4) covers the small local models above. The open-weight models Vals did measure, all
server-class, sit at 31–39% on TB4, against about 64–65% for the top hosted models [56].
Treat local models as good enough for search, summaries and docs, small test-guarded edits,
and privacy-required work. Don't use them for planning or review.

---

## 3. Claude Code and Codex CLI: models, selection and plan limits

### 3.1 Claude Code (`claude -p`)

**Model aliases** [7]:

| Alias | Anthropic API resolves to |
|---|---|
| `opus` | Opus 5.5 |
| `sonnet` | Sonnet 5.5 |
| `haiku` | Haiku 5.5 |
| `fable` | Fable 5.1 |

- `best` uses Fable where available, otherwise Opus.
- `opusplan` uses Opus in plan mode, then Sonnet for execution.
- `default` resolves to **Opus 5.5** on Pro, Max, Ultra, Team, Enterprise and the API.
- Full ids (`claude-opus-5-5`) also work.
- Aliases resolve to different models on Bedrock, Vertex and Foundry.

**Selecting a model** (priority order) [7]:

1. `/model`
2. `--model <alias|id>`
3. `ANTHROPIC_MODEL`
4. the `model` setting

You can remap aliases with `ANTHROPIC_DEFAULT_{OPUS,SONNET,HAIKU,FABLE}_MODEL`, and set the
subagent default with `CLAUDE_CODE_SUBAGENT_MODEL` [7].

**Effort.** `--effort low|medium|high|xhigh|max|ultracode` [8]. In Claude Code the default is
`medium` on Opus 5.5, Sonnet 5.5 and Haiku 5.5 [7].

**Headless flags useful to the harness** [8]: `-p`, `--output-format text|json|stream-json`,
`--max-turns`, `--max-budget-usd` (estimated API spend, print mode), `--json-schema`, `--bare`
(skips CLAUDE.md, plugins and MCP for faster scripted starts) and `--fallback-model a,b`.
`--fallback-model` covers an overloaded or unavailable model, **not** plan limits.

**Subagents.** The built-in Explore subagent runs on the main model, not Haiku, unless you
override it. To put it on Haiku, define your own `Explore` with `model: haiku` [11].

**Plan limits** (publicly documented) [12][13]:

| Plan | Price | 5-hour window | Weekly window | Fable |
|---|---|---|---|---|
| Pro | $20/mo ($17/mo billed annually) | yes | yes | only through usage credits |
| Max | $100/mo | 5× Pro | 5× Pro | "up to half your weekly limit" (separate Fable weekly limit) |
| Ultra | $200/mo | none for verified accounts | 10× Pro | 50% of weekly limits |
| Team | Standard seat $20–25; Premium $100–125 | rolling 5-hour and weekly per-seat allowance [9] | | Premium only |

- Claude and Claude Code share the same limits [12].
- No message or hour counts are published [12].
- A session or weekly limit is shared across all models, so switching with `/model` doesn't
  restore access. A model-specific "Opus limit" message can be worked around by switching
  family [9].
- Usage credits let you continue past a limit, billed at API rates [12].

### 3.2 Codex CLI (`codex exec`)

**Models.** The bundled catalog in the Codex source lists these as visible picker models
(commit `93b4e5b`, 2026-11-04) [14]:

| Model id | Codex context window | Default effort |
|---|---|---|
| `gpt-6.1-sol` | 272,000 | low |
| `gpt-6-astra` | 272,000 | low |
| `gpt-6-sol` | 272,000 | medium |
| `gpt-6-luna` | 272,000 | medium |
| `gpt-5.6-sol` | 272,000 | low |
| `gpt-6.1-terra` | 500,000 | low |
| `gpt-5.6-terra` | 272,000 | medium |
| `gpt-5.6-luna` | 272,000 | medium |
| `gpt-5.5` | 272,000 | medium |

- Hidden entries: `codex-auto-review` (approval review) and the `gpt-daybreak-*` cyber
  models [14].
- Effort levels are `low`, `medium`, `high`, `xhigh` and `max`, plus `ultra` ("Maximum
  reasoning with automatic task delegation") on Astra, Sol and Terra [14].
- The default is the first visible model by priority, **`gpt-6.1-sol`** in this snapshot.
  Codex can refresh the catalog from the server, so the live default may differ [14][15].
- ⚠ Codex's context windows are smaller than the API's 1.05M [14][18]. OpenAI Help says
  GPT-6.1 Sol defaults to 500,000 in Work and Codex [25]; the bundled catalog says 272,000.
- Astra in the Codex CLI needs version 0.153.0 or later [24].

**Selecting a model** [15][28]. Use `codex exec -m <id>` (or `--model <id>`); `-c key=value`
overrides config, e.g. `-c model_reasoning_effort=high`. The `config.toml` keys are `model`,
`model_reasoning_effort` and `review_model` (the model `/review` uses). `codex exec review`
runs a review non-interactively. `--oss --local-provider ollama|lmstudio` runs a local model,
default `gpt-oss:20b`. `--json` emits JSONL events; `--output-schema` validates the final
message.

**ChatGPT plan limits** [23][24]:

- Plus and Business Standard have a 5-hour window plus a weekly limit, and you need allowance
  left in both to keep going.
- Pro ($100 / $200 / $500) "currently ha[s] no five-hour usage limit in Work and Codex" but
  keeps a weekly allowance.
- Switching models does not restore a shared allowance.
- Fast mode uses more allowance; Ultrafast is only on Pro $500 and eligible Enterprise/Edu.

Estimated local messages per 5 hours on Plus or Business Standard [23]:

| Model | Messages per 5 h |
|---|---|
| GPT-6 Astra | 5–45 |
| GPT-6.1 Sol | 15–160 |
| GPT-6 Sol | 15–150 |
| GPT-6.1 Terra | 35–285 |
| GPT-6 Luna | 350–3,000 |

⚠ The Codex pricing page's date is unclear, and the Pro wording differs between the help
article and the pricing page [23][24].

---

## 4. Recommended task-type → tier defaults

These are TermOS's defaults (`[harness.routes]` changes them), and reviews go
first to another backend than the one that wrote the code (`[harness]
cross-review`, on by default), for the reasons in the review row.

| Task | Default tier | Evidence and reasoning |
|---|---|---|
| planning | **frontier** | Both vendors reserve their top model for this. Claude Code's `opusplan` switches to Opus only for plan mode [7]; Anthropic's cost guide says to "reserve Opus for complex architectural decisions or multi-step reasoning" [9]; OpenAI positions Astra for "the most demanding reasoning" [16]. Planning is a few calls, so the frontier premium is small. |
| coding | **balanced**; frontier for multi-file or long-horizon work | Sonnet 5.5 is within about 1–2 points of Opus 5.5 on Vals TB4 and CursorBench [56][4]. GPT-6.1 Sol reaches 55% on Vals TB4 at about 1/5 of Astra's cost per task [56]. Raise to frontier for migrations and audits, where Anthropic says Opus 5.5 is "particularly good" [3]. |
| review | **frontier, cross-provider** | Reviewer quality matters more than effort: Opus 5.5 at lowest effort caught 72% of known bugs against 56% for the older Opus at high effort (customer quote) [3]. LLM judges favour their own generations [61], partly for good reasons [62]. That supports reviewing with a different model family than the author. ⚠ No code-review-specific study was found, so this is an inference. Codex's dedicated `review_model` and `codex exec review` make it the natural reviewer for Claude-written code, and the reverse also works [15]. CodeRabbit moved "simple and moderate reviews" to Sonnet 5.5 [4], so balanced is acceptable for small diffs. |
| tests | **balanced** | Writing tests is coding. Running tests and triaging their output is verbose, and Anthropic recommends delegating it to subagents so the output stays out of the main context [9]. The harness can send the "run and summarize failures" step to fast. Avoid max effort on Sonnet 5.5 here (subagent fan-out timeouts) [4]. |
| search | **fast** (balanced for "explain the architecture" questions) | Haiku 5.5 is positioned for subagent and summarization work [5]; Codex uses Luna-class models for background extraction [15]. ⚠ Claude Code keeps its own Explore subagent on the main model by default [11], a sign that Anthropic does not treat search as safely fast-only. Keep fast-tier search contexts under 100K tokens on Haiku 5.5, where its price rises 5× [6]. |
| small-edits | **fast** | Haiku 5.5 scores 39.2% (vendor) / 36.4% (Vals) on TB4 [5][56], enough for scoped edits guarded by tests. GPT-6 Luna (13.6% on Vals TB4) is better kept to single-file edits [56]. |
| docs | **fast** (balanced for user-facing guides) | This is summarization-shaped work that Haiku-class models are positioned for [5]. Use balanced where prose quality matters. |

**Suggested tier fillings per backend.**

| Backend | frontier | balanced | fast |
|---|---|---|---|
| Claude Code (subscription) | `opus` at `high`/`xhigh` effort; `fable` only on Max or Ultra when Opus falls short [1][13] | `sonnet` (or `opus` at `medium`: Opus 5.5 was cheaper per task than Sonnet 5.5 on Vals TB4 [56]) | `haiku` |
| Codex CLI (ChatGPT plan) | `gpt-6-astra` at `low`/`medium` [24]. On Plus, budget is 5–45 messages per 5 h [23] | `gpt-6.1-sol` | `gpt-6-luna`, or `gpt-6.1-terra` when you need agentic edits [23][56] |
| Anthropic API | `claude-opus-5-5` | `claude-sonnet-5-5` | `claude-haiku-5-5` |
| OpenAI API | `gpt-6-astra` | `gpt-6.1-sol` | `gpt-6-luna` |
| Google | `gemini-4-argon` (best as reviewer or planner) | `gemini-4-argon` at low thinking ⚠ (no distinct model) | `gemini-3.8-flash` (search and docs only) |
| Moonshot | `kimi-k3.1-pro` | `kimi-k3.1-pro` | `kimi-k2.7-code` ⚠ |
| DeepSeek | none | `deepseek-v4-pro` | `deepseek-flash` |
| Qwen | none | `qwen3.8-max` | local `Qwen3.8-27B` / `Qwen3.8-Flash-Next` |
| Mistral | none | `mistral-large-4` (weak agentic evidence) | none for agentic work; `codestral-2508` is FIM only |
| xAI | none | `grok-4.7` (poor measured value) | `grok-4.3` ⚠ (no coding benchmark found) |
| Z.ai | none | `glm-5.3` | GLM-5.3-Flash |
| Local (llama.cpp) | none | `Qwen3.8-27B` ⚠ | `gpt-oss-20b`, `Qwen3.8-Flash-Next` |

**Cross-model pairing suggestions** (an inference from [61][62], not measured):

- Code with Claude Code (`sonnet`) and review with Codex (`gpt-6-astra` as `review_model`).
- Or code with Codex (`gpt-6.1-sol`) and review with Claude Code (`opus`).
- Kimi K3.1 Pro and Gemini 4 Argon are API reviewers from a third family.

**Fallback.** On a plan limit, fall back to the *API* of a comparable tier, not another model
on the same plan. Both vendors share one allowance across models [9][24], and
`--fallback-model` does not cover plan limits [8].

---

## 5. Cost and efficiency tips

1. **Keep one model and effort per session, and route at session boundaries.** On Anthropic,
   switching models, turning on fast mode or changing MCP tools invalidates the prompt cache.
   Changing effort keeps it only on Opus 5.5, Sonnet 5.5, Haiku 5.5 and Fable 5.1 (API key or
   subscription) [10]. The same prefix rule applies to OpenAI, where you change effort
   mid-conversation with a `configuration_update` item so the cache survives [16].
2. **Count on cache reads.**

   | Provider / model | Cache read price |
   |---|---|
   | Anthropic Opus 5.5, Sonnet 5.5 | 0.05× input [2] |
   | OpenAI GPT-6.1 Sol and Terra | 0.05× input [26] |
   | Gemini 4 Argon | 95% off (implicit caching only) [29][30] |
   | DeepSeek Flash | $0.003 against $0.15 for a miss [38] |
   | Kimi K3 | $0.30 against $3.00 [35] |

   Anthropic and OpenAI charge 1.25× for cache *writes* [2][26], so don't let volatile text
   (timestamps, per-request ids) into the prefix.
3. **Pick the TTL deliberately.**
   - Claude Code uses a 1-hour TTL for the main conversation on a subscription, and 5 minutes
     for subagents and for API-key or usage-credit billing. You can set either with
     `promptCacheTtl` / `CLAUDE_CODE_PROMPT_CACHE_TTL` [10].
   - OpenAI defaults to 30 minutes, with 12 hours available at a 2.5× write cost [26].
   - Long idle gaps make the first turn back an uncached full re-read [10].
4. **Use cheap tiers for exploration, but watch price thresholds.** Haiku 5.5 costs 5× more
   once a prompt passes 100K tokens [6]; GPT-6 Astra and Sol cost 2× input and 1.5× output
   once input passes 272K [18][19]; grok-4.7 doubles above 200K [50]. Exploring a large repo
   inside one huge context can cost more than a few small subagent calls, each of which
   returns only a summary [9].
5. **Judge by cost per task, not price per token.**
   - On Vals TB4, GPT-6.1 Sol costs $1.72 per task against $9.58 for Astra, Kimi K3.1 Pro
     $4.57 against $13.20 for Opus 5.5, and GLM-5.3-Flash $1.22 against $9.37 for GLM-5.3 at
     similar scores [56].
   - Opus 5.5 at default effort beat Opus 5 at max "at about a fifth of the cost" [3], and
     Astra at Low can beat Sol at High [24]: try lower effort first, raise it where evals
     show headroom.
6. **Use batch or off-peak for non-interactive jobs** (doc generation, bulk reviews):
   Anthropic batch is 50% off [2]; OpenAI Batch and Flex [17] and Gemini batch [29] are half
   price; the Kimi K3 series costs 60% of list on batch [35]; Qwen batch is 50% [43];
   DeepSeek off-peak is half of peak [38].
7. **Avoid fast modes on background tiers.** Opus 5.5 fast mode costs 2× ($8 / $40) [2] and
   OpenAI fast mode costs 2× [17]. Fast and standard speed also keep separate caches [10].
8. **Watch introductory prices that expire.** Gemini 4 Argon doubles on 2026-12-01 [29];
   Gemini 3.8 Flash doubles on 2027-01-01 [29][32]; Mistral Large 4's current rate may be
   promotional [46].
9. **OpenRouter.** It bills inference at provider list price plus a fee on credit purchases
   (5.5% Standard, 8% Business) [59]. Most providers cache automatically, but Anthropic and
   Qwen models need explicit `cache_control`; sticky routing keeps cache hits on the same
   endpoint [59].
10. **Local (llama.cpp).** `llama-server` reuses a slot's KV cache for a shared prefix
    (`cache_prompt`, on by default), but agent switches that change the system prompt force a
    full re-prefill, so keep one stable system prompt per local model. `--cache-reuse` is not
    a general prefix cache [60]. Local models are good enough for search, docs and
    test-guarded small edits; hosted frontier models still lead agentic benchmarks by about
    25 points over the best measured open-weight models [56].

---

## 6. What is uncertain

- ⚠ Pages for OpenAI and Google (S sources) were seen only through search summaries:
  [17]–[26] and [29]–[34] are worth reading directly.
- ⚠ DeepSeek's prices differ between its docs pricing page [38] and its platform page [41].
- ⚠ OpenAI's API availability of Sol, Terra and Luna conflicts with the Enterprise rate card;
  the context window of GPT-6.1 Sol in Codex conflicts (272K in the source against 500K in
  the help center) [14][25].
- ⚠ Kimi K2 retirement date, Kimi K2.7 Code pricing, and whether K3.1 Pro weights shipped
  [35][36][37].
- ⚠ Mistral Large 4 context (1M against 512K) and price (promotional?), and whether Mistral
  Medium 3.5 is still served after 2026-10-31 [46][47].
- ⚠ Exact API ids for GLM-5.3-Flash and FlashX, and for any Gemini Flash-Lite.
- ⚠ Gemini 3.8 Flash Terminal-Bench 2.1: 89.4% on the model card against 90.8% elsewhere [32].
- ⚠ Grok 4.7 TB4: 46.0% (xAI) against 28.79% (Vals) [51][56]; Sonnet 5.5: 70.6% against
  64.14% [4][56].
- No independent benchmark covers code review quality or cross-model review. The review
  recommendation rests on customer quotes [3][4] and general judge-bias research [61][62].

---

## 7. Sources (all accessed 2026-11-04)

Access method: **F** fetched and read; **R** read from a repository at a pinned commit;
**S** seen only via web-search result summaries (direct fetch blocked or not attempted).

1. Anthropic, Models overview. https://platform.claude.com/docs/en/about-claude/models/overview (F)
2. Anthropic, Pricing. https://platform.claude.com/docs/en/about-claude/pricing (F)
3. Anthropic, Introducing Claude Opus 5.5. https://www.anthropic.com/news/claude-opus-5-5 (F)
4. Anthropic, Introducing Claude Sonnet 5.5. https://www.anthropic.com/claude-sonnet-5-5 (F)
5. Anthropic, Introducing Claude Haiku 5.5. https://www.anthropic.com/claude-haiku-5-5 (F)
6. Anthropic, Claude Haiku 5.5 overview. https://platform.claude.com/docs/en/models/haiku-5-5/overview (F)
7. Claude Code docs, Model configuration. https://code.claude.com/docs/en/model-config (F)
8. Claude Code docs, CLI reference. https://code.claude.com/docs/en/cli-reference (F)
9. Claude Code docs, Manage costs effectively. https://code.claude.com/docs/en/costs (F)
10. Claude Code docs, How Claude Code uses prompt caching. https://code.claude.com/docs/en/prompt-caching (F)
11. Claude Code docs, Subagents. https://code.claude.com/docs/en/sub-agents (F)
12. Claude Help Center, Use Claude Code with your Pro, Max, or Ultra plan. https://support.claude.com/en/articles/11145838-use-claude-code-with-your-pro-or-max-plan (F; page says "Updated this week")
13. Claude, Plans & pricing. https://claude.com/pricing (F)
14. openai/codex, `codex-rs/models-manager/models.json` at commit 93b4e5b4 (2026-11-04). https://github.com/openai/codex/blob/93b4e5b4c6474cdbf12614c4ac3acbd006fbee89/codex-rs/models-manager/models.json (R)
15. openai/codex at 93b4e5b4 (R): `codex-rs/utils/cli/src/shared_options.rs` (`-m`, `--oss`, `--local-provider`), `codex-rs/utils/cli/src/config_override.rs` (`-c`), `codex-rs/exec/src/cli.rs` (`--json`, `--output-schema`, `review`), `codex-rs/config/src/config_toml.rs` (`model`, `review_model`, `model_reasoning_effort`), `codex-rs/protocol/src/openai_models.rs` (default selection), `codex-rs/ollama/src/lib.rs` (`gpt-oss:20b`), `codex-rs/model-provider/src/provider.rs` and `codex-rs/tui/src/app/thread_title.rs` (auxiliary models)
16. openai/codex at 93b4e5b4, `codex-rs/skills/src/assets/samples/openai-docs/references/upgrading-to-gpt-6-astra.md` and `latest-model.md`: OpenAI's bundled copy of its GPT-6 guide, which it labels a non-authoritative fallback (R)
17. OpenAI, API Pricing. https://developers.openai.com/api/docs/pricing (S)
18. OpenAI, GPT-6 Astra model page. https://developers.openai.com/api/docs/models/gpt-6-astra (S)
19. OpenAI, GPT-6.1 Sol model page. https://developers.openai.com/api/docs/models/gpt-6.1-sol (S)
20. OpenAI, API platform page. https://openai.com/api/ (S; search metadata showed an old crawl ⚠)
21. OpenAI, GPT-6 Astra: A new generation of intelligence. https://openai.com/index/gpt-6-astra/ (S)
22. OpenAI, Introducing GPT-6.1 Sol. https://openai.com/index/introducing-gpt-6-1-sol/ (S)
23. OpenAI, Codex pricing. https://developers.openai.com/codex/pricing (S; page date unclear ⚠)
24. OpenAI Help, Managing usage with GPT-6 Astra in Work and Codex. https://help.openai.com/en/articles/20001516-managing-usage-with-gpt-6-astra-in-work-and-codex (S)
25. OpenAI Help, ChatGPT Work and Codex. https://help.openai.com/en/articles/20001275-chatgpt-work-and-codex (S)
26. OpenAI, Prompt caching guide. https://developers.openai.com/api/docs/guides/prompt-caching (S)
27. OpenAI, Why we no longer evaluate SWE-bench Verified. https://openai.com/index/why-we-no-longer-evaluate-swe-bench-verified/ (S)
28. OpenAI, Codex config basics. https://developers.openai.com/codex/config-basic (S)
29. Google, Gemini Developer API pricing. https://ai.google.dev/gemini-api/docs/pricing (S); Agent Platform pricing. https://cloud.google.com/gemini-enterprise-agent-platform/generative-ai/pricing (S)
30. Google, Gemini 4 Argon model page and "What's new in Gemini 4 Argon". https://ai.google.dev/gemini-api/docs/models/gemini-4-argon, https://ai.google.dev/gemini-api/docs/latest-model (S)
31. Google, Introducing Gemini 4 Argon. https://blog.google/innovation-and-ai/models-and-research/gemini-models/introducing-gemini-4-argon/, plus model card https://deepmind.google/models/model-cards/gemini-4-argon/ (S)
32. Google DeepMind, Gemini 3.8 Flash model card. https://deepmind.google/models/model-cards/gemini-3-8-flash/; Cloud docs https://docs.cloud.google.com/gemini-enterprise-agent-platform/models/gemini/3-8-flash; blog https://blog.google/innovation-and-ai/models-and-research/gemini-models/3-8-flash-and-3-8-flash-cyber/ (S)
33. Google, Gemini 4 Carbon. https://blog.google/innovation-and-ai/models-and-research/gemini-models/introducing-gemini-4-carbon/ (S)
34. Google, Gemini deprecations. https://ai.google.dev/gemini-api/docs/deprecations (S)
35. Moonshot, Kimi API model list, pricing and context-caching guide. https://platform.kimi.ai/docs/models, https://platform.kimi.ai/docs/pricing/chat, https://platform.kimi.ai/docs/guide/context-caching (S)
36. Moonshot, Kimi K3.1 Pro tech blog. https://www.kimi.com/en/blog/kimi-k3-1-pro (S)
37. Moonshot, Kimi K2.7 Code pricing. https://www.kimi.com/resources/kimi-2-7-pricing (S; not the platform pricing page ⚠)
38. DeepSeek, Models & Pricing and change log. https://api-docs.deepseek.com/quick_start/pricing/, https://api-docs.deepseek.com/updates/ (S)
39. DeepSeek, V4 preview, V4.1-Flash and V4.1-Flash-1017 release notes. https://api-docs.deepseek.com/news/news260424/, https://api-docs.deepseek.com/news/news260910/, https://api-docs.deepseek.com/news/news261017/ (S)
40. DeepSeek, DeepSeek-V4-Pro model card. https://huggingface.co/deepseek-ai/DeepSeek-V4-Pro (S)
41. DeepSeek, Platform page. https://www.deepseek.com/en/platform/ (S; conflicts with [38] ⚠)
42. Qwen, Qwen3.8-Max blog. https://qwen.ai/blog?id=qwen3.8; open weights https://huggingface.co/Qwen/Qwen3.8-2.4T-A95B (S)
43. Alibaba Cloud Model Studio, qwen3.8-max model info and model pricing. https://www.alibabacloud.com/help/en/model-studio/qwen3-8-max, https://www.alibabacloud.com/help/en/model-studio/model-pricing (S)
44. Qwen, Qwen3.8-27B model card. https://huggingface.co/Qwen/Qwen3.8-27B (S)
45. Qwen, Qwen3.8-Flash-Next model card and blog. https://huggingface.co/Qwen/Qwen3.8-Flash-Next, https://qwen.ai/blog?id=qwen3.8-flash-next; community quants https://huggingface.co/unsloth/Qwen3.8-Flash-Next-GGUF (S)
46. Mistral, Mistral Large 4 docs, announcement and open-weight post. https://docs.mistral.ai/models/mistral-large-4-0.md, https://mistral.ai/news/mistral-large-4/, https://mistral.ai/news/mistral-large-4-oss/ (S)
47. Mistral, Mistral Medium 3.5 model card. https://docs.mistral.ai/models/model-cards/mistral-medium-3-5-26-04, plus https://huggingface.co/mistralai/Mistral-Medium-3.5-128B (S)
48. Mistral, Devstral 2 and Devstral Small 2 docs. https://docs.mistral.ai/models/devstral-2-25-12, https://docs.mistral.ai/models/devstral-small-2-25-12 (S)
49. Mistral, Codestral 25.08 docs, API pricing and lifecycle. https://docs.mistral.ai/models/codestral-25-08, https://docs.mistral.ai/inference/pricing, https://legal.mistral.ai/ai-governance/models/codestral/ (S)
50. xAI, Models & Pricing, Grok 4.7 developer guide and release notes. https://docs.x.ai/developers/models, https://docs.x.ai/developers/grok-4-7, https://docs.x.ai/developers/release-notes (S)
51. xAI, Grok 4.7 model card (2026-09-21). https://media.x.ai/v1/website/card4p7-3a96f40b.pdf (S)
52. Z.ai, Pricing, GLM-5.3 guide and parameter reference. https://docs.z.ai/guides/overview/pricing, https://docs.z.ai/guides/llm/glm-5.3, https://docs.z.ai/guides/overview/concept-param (S)
53. Z.ai, GLM-5.3 and GLM-5.3-Flash model cards. https://huggingface.co/zai-org/GLM-5.3, https://huggingface.co/zai-org/GLM-5.3-Flash (S)
54. Z.ai, GLM Coding Plan. https://z.ai/subscribe (S)
55. OpenAI, Introducing gpt-oss, plus model card. https://openai.com/index/introducing-gpt-oss/, https://cdn.openai.com/pdf/419b6906-9da6-406c-a19d-1bb078ac7637/oai_gpt-oss_model_card.pdf (S)
56. Vals AI, Terminal-Bench 4.0 leaderboard (updated 2026-10-29). https://www.vals.ai/benchmarks/terminal-bench-4 (S)
57. Scale AI, SWE-Bench Pro V2 leaderboard and post. https://labs.scale.com/leaderboard/swe_bench_pro_public_v2, https://labs.scale.com/blog/swe-bench-pro-v2 (S)
58. Aider, LLM leaderboards. https://aider.chat/docs/leaderboards/ (S)
59. OpenRouter, Pricing and Prompt caching guide. https://openrouter.ai/pricing, https://openrouter.ai/docs/guides/best-practices/prompt-caching (S)
60. llama.cpp, server README and discussion #22354. https://github.com/ggml-org/llama.cpp/blob/master/tools/server/README.md, https://github.com/ggml-org/llama.cpp/discussions/22354 (S)
61. Panickssery et al., "LLM Evaluators Recognize and Favor Their Own Generations" (2024). https://arxiv.org/abs/2404.13076 (S)
62. Chen et al., "Do LLM Evaluators Prefer Themselves for a Reason?" (2025). https://arxiv.org/abs/2504.03846 (S)
63. Datacurve, "DeepSWE: Measuring Frontier Coding Agents on Original, Long-Horizon Engineering Tasks". https://arxiv.org/pdf/2607.07946 (S)

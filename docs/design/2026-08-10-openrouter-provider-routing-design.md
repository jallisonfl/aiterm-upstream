# OpenRouter provider routing: policy, pins, and what actually ran

Status: designed 2026-08-10, not yet implemented.

## The problem

A model id on OpenRouter is not a price, and it is not a company either.
`z-ai/glm-5.2` is served by 32 hosts. On 2026-08-10 their output prices ran from
$1.58/M (Novita) to $3.15/M (Sail Research) for the same model, and the hosts
sit in at least four countries. OpenRouter picks one, the pick can change
between one session and the next, and aiterm neither constrains it nor reports
it.

Three separate costs come out of that:

- **Price.** Model access shows one "Output $1.58/M" line, taken from the model
  record. That is the cheapest endpoint, not the one that will serve you. aiterm
  states a price it does not control.
- **Jurisdiction.** Some hosts are Chinese companies, some are US companies with
  Chinese datacenters. Nothing in aiterm can say "not those", and the decision
  is being made silently on every request.
- **Visibility.** After a session there is no way to answer "who ran that". Not
  in aiterm, not in the transcript.

A fourth, smaller gap: the model browser filters by All / Free / Paid and by
context, but not by what is already on the startup list. With 400 models in the
catalog, finding the handful you starred means remembering their names.

## Scope

- A **routing policy** for the OpenRouter provider: excluded countries,
  excluded providers, and a default price ceiling.
- A **per-model route**: an optional pin, and an optional price ceiling that
  overrides the default.
- A **provider list** in the model card that shows every endpoint — price,
  country, uptime — and sets the pin.
- **Attribution**: which host served a reply, live in aiterm chat, and which
  hosts served the last 30 days, from OpenRouter's activity record.
- A **Starred** filter in the model browser.

Both routing consumers are in scope: aiterm's own chat console and an OpenCode
launch. A policy that only one of them honours would be worse than none, because
an OpenCode session is what an API pick starts whenever OpenCode is installed.

Out of scope: `sort`, `quantizations`, `data_collection`, `zdr` and the rest of
OpenRouter's provider object; ordered chains of more than one preferred provider
(the stored shape allows them, the UI writes one); routing for any provider that
is not OpenRouter; changing OpenRouter's own account-wide ignore list, which is
set on their site and merges server-side where aiterm cannot see it.

## What was verified first

Six facts, established by experiment and by reading OpenRouter's OpenAPI
document on 2026-08-10, before the design was fixed. No API key was spent.

**A pin is a provider slug, and the slug is derivable.** The endpoints reply
tags each host `novita/fp8`, `wafer/fast`. The part before the slash matches the
`slug` field in `/api/v1/providers`. That is what `order` and `ignore` take.

**OpenCode forwards a model's `options` block onto the request body.** A stub
HTTP server standing in for OpenRouter captured what OpenCode sent. With
`provider.openrouter.models.<id>.options.provider` set in config, the captured
body carried `"provider": {"order": ["novita"], "allow_fallbacks": false}` as a
top-level sibling of `model` and `messages` — the shape OpenRouter documents.

The same experiment ruled out the obvious alternative: under
`options.extraBody`, the body carried a literal top-level `"extraBody"` key one
level too deep, which OpenRouter ignores in silence. `extraBody` is a convention
of a different SDK layer and must not be built here.

**`OPENCODE_CONFIG_CONTENT` merges, it does not replace.** Running
`opencode models` with an inline config naming only the openrouter provider
still listed `local/local` from `~/.config/opencode/opencode.json`. The routing
block can be handed to OpenCode per launch, in the environment, without aiterm
writing a file it does not own.

**Every streaming chunk names the host that served it.** Two live streams
against a `:free` model, one with `stream_options` and one without, carried a
top-level `"provider": "Darkbloom"` on all seven chunks of each. Attribution in
aiterm chat is therefore a field read on the first chunk — no second request,
no `openrouter_metadata` parsing, no waiting for the reply to finish.

Two details the implementation has to respect. OpenRouter's own OpenAPI document
does **not** list `provider` on `ChatStreamChunk`; the field is real but
undeclared, so the parser reads it where present and shows nothing where absent
rather than treating its absence as an error. And the non-streaming path is
different: `ChatResult` carries the same fact under
`openrouter_metadata.endpoints.available[].selected`. aiterm chat streams, so
the chunk field is the one that matters.

**A reply's cost is available for the asking.** With
`stream_options: {"include_usage": true}`, a final chunk arrives carrying
`usage` with `prompt_tokens`, `completion_tokens` and `cost` in dollars for that
exchange. It costs one flag and nothing else.

**The account's history is queryable.** `GET /api/v1/activity` returns up to 30
days, one row per day and model and provider, carrying `provider_name`,
`requests`, `prompt_tokens`, `completion_tokens` and `usage` in dollars. This is
what makes OpenCode sessions visible: aiterm intercepts nothing and still knows
what ran. `GET /api/v1/generation?id=` gives the same attribution for a single
request, with `total_cost` and latency.

**Country data exists, and is imperfect.** `/api/v1/providers` carries
`headquarters` and `datacenters` per provider. Of 101 providers: 53 US, 6 CN,
6 SG, and **29 report no country at all**. The two fields disagree in ways that
matter — `alibaba` is headquartered SG with datacenters in **SG and CN**, while
`novita`, the host this work started from, reports **US**. A country rule has to
say which field it reads, and what it does about the 29 unknowns. This design
reads both and treats unknown as a user decision.

## Data model

`Provider` gains two fields. A `providers.json` from 0.10.40 has neither, and
`#[serde(default)]` reads it unchanged; the next save writes them. There is no
migration step.

```json
{
  "id": "openrouter",
  "startup_models": ["z-ai/glm-5.2"],
  "policy": {
    "blocked_countries": ["CN"],
    "block_unknown_country": true,
    "blocked_providers": [],
    "max_price": { "completion": 2.5 },
    "resolved_ignore": ["baidu", "streamlake", "alibaba", "…"],
    "resolved_at": 1786000000
  },
  "routes": {
    "z-ai/glm-5.2": {
      "order": ["novita"],
      "allow_fallbacks": false,
      "max_price": { "completion": 1.8 }
    }
  }
}
```

```rust
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct MaxPrice {
    /// USD per *million* prompt tokens — OpenRouter's unit for this field,
    /// which is not the per-token unit `/models` quotes prices in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Policy {
    /// ISO country codes. A provider is blocked when its headquarters or any
    /// of its datacenters is listed.
    pub blocked_countries: Vec<String>,
    /// What to do with the 29 providers that report no country.
    pub block_unknown_country: bool,
    /// Slugs blocked by hand, whatever their country says.
    pub blocked_providers: Vec<String>,
    /// Default ceiling, overridable per model.
    pub max_price: MaxPrice,
    /// The policy compiled against the provider directory: the slugs actually
    /// sent as `ignore`. Written by the panel, read by both launchers.
    pub resolved_ignore: Vec<String>,
    /// When that resolution was computed, epoch seconds.
    pub resolved_at: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Route {
    /// Provider slugs, most preferred first. One entry is a pin.
    pub order: Vec<String>,
    /// False means "fail rather than route elsewhere".
    pub allow_fallbacks: bool,
    /// Overrides `Policy::max_price` for this model when set.
    #[serde(default)]
    pub max_price: MaxPrice,
}
```

Four consequences, each deliberate:

- **`startup_models` keeps its type.** `StartControls`, `opencode_models` and
  `agents.rs` are untouched by this spec. The startup list stays membership and
  ordering; routing is a separate concern keyed by the same ids.
- **A route outlives its star.** Unstarring leaves the route, so re-starring
  restores the pin. A route for an unstarred model is never read.
- **`order` is a list from the start.** An ordered chain later is a UI change,
  not a second format change.
- **Policy is intent; `resolved_ignore` is the compiled artifact.** See below.

### Why the ignore list is stored, not computed

A country rule has to become a list of slugs before it can be sent. Computing it
needs `/api/v1/providers`, a network call. Both places that build a request are
the wrong place for one: `chat::run` is a CLI process starting in a pty, and a
launch is a keystroke away from a terminal that should already be running. A
provider directory fetch there means a launch that hangs when the network is
slow and a policy that silently lapses when it is down.

So the panel compiles the policy — on save, and on an explicit **Refresh
directory** — and stores the result. Both launchers read a list of strings and
make no network call. The panel shows what was compiled ("excluding 12
providers") and how old it is, and says so when the resolution is more than 30
days stale. New hosts appear on OpenRouter regularly; a stored list does not
learn about them, and pretending otherwise is the failure mode worth naming in
the UI rather than in a comment.

`ProviderView` carries `policy` and `routes` across IPC unchanged. Neither holds
a secret.

## What goes on the wire

One function builds the block, and both consumers call it:

```rust
pub fn routing_block(p: &Provider, model: &str) -> Option<serde_json::Value>
```

- `ignore`: `policy.resolved_ignore`, when non-empty.
- `order`: `route.order`, when the model is pinned.
- `allow_fallbacks`: `false` when pinned. Omitted otherwise.
- `max_price`: the route's, else the policy's, when either sets a field.

Returns `None` when all four are empty, so an unrouted model sends exactly what
it sends today.

A pin therefore means **only that host**, per the decision taken 2026-08-10.
That has a consequence worth stating plainly, because it will look like a bug
one day: a pinned host whose price rises above the cap, or which goes down, does
not fall back. The request fails. That is the requested behaviour — knowing who
ran your tokens beats finishing the request — but the error text has to say
which constraint refused, or the session looks broken for no reason.

`ignore` and `max_price` are still sent alongside a pin, where they are usually
redundant. They cost nothing, they keep one shape for the block, and they stay
correct if the pin is removed later.

### aiterm chat

`chat_body` takes the block and merges it in:

```rust
pub fn chat_body(model: &str, messages: &[Msg], routing: Option<&Value>) -> String
```

`chat::run` resolves the provider into `p` before the loop, so this is
`routing_block(p, &model)` — looked up **per send, not once**, because `/model`
swaps the model mid-chat and the route has to swap with it.

### OpenCode

The launch environment gains one variable when the model has anything to send:

```
OPENCODE_CONFIG_CONTENT={"provider":{"openrouter":{"models":
  {"z-ai/glm-5.2":{"options":{"provider":{ …the same block… }}}}}}}
```

Built with `serde_json`, never by formatting strings: a model id is user data
that lands inside a JSON key. It goes where the OpenRouter key already goes —
the env-injection path in `pty.rs` that gives an OpenCode tab its
`OPENROUTER_API_KEY`. Nothing is written to `~/.config/opencode/opencode.json`.

## Reading OpenRouter

Three new commands, all OpenRouter-only, all reusing one curl helper. The curl
invocation currently inside `fetch_models_response` moves out to
`fetch(provider, url) -> Result<String>`, and every caller uses it. The reason is
the comment already in that function: the key goes in on stdin because
`/proc/<pid>/cmdline` is world-readable. A second implementation of that rule is
a second chance to get it wrong.

```rust
provider_model_endpoints(id, model) -> Vec<EndpointCard>  // {base}/models/{model}/endpoints
provider_directory(id)              -> Vec<DirectoryEntry> // {base}/providers
provider_activity(id)               -> Vec<ActivityRow>    // {base}/activity
```

```rust
pub struct EndpointCard {
    pub provider_name: String,        // "Novita"
    pub slug: String,                 // "novita" — what a pin or ignore writes
    pub tag: String,                  // "novita/fp8" — shown, never sent
    pub quantization: Option<String>,
    pub context_length: Option<u64>,
    pub prompt_price: Option<f64>,    // USD per token, as quoted
    pub completion_price: Option<f64>,
    pub max_completion_tokens: Option<u64>,
    pub uptime_30m: Option<f64>,
}

pub struct DirectoryEntry {
    pub slug: String,
    pub name: String,
    pub headquarters: Option<String>,
    pub datacenters: Vec<String>,
}

pub struct ActivityRow {
    pub date: String,
    pub model: String,
    pub provider_name: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub usage: f64,                   // USD
}
```

Endpoints keep OpenRouter's own order; it ranks them, and re-sorting would
disagree with the site the user just came from. A non-OpenRouter provider is
refused with a plain sentence — the paths are OpenRouter's, and a `/endpoints`
call to a bare llama.cpp is a 404 nobody can act on.

## The panel

### Routing policy

A section on the OpenRouter provider card, above the model browser:

- Country blocks as toggles, seeded from the countries actually present in the
  directory, with **CN** the one this was built for. The rule reads headquarters
  **and** datacenters, so `alibaba` (HQ SG, datacenters SG + CN) is caught.
- **Also block providers that report no country** — one checkbox, defaulting to
  off, with the count beside it, because 29 of 101 is too large a fraction to
  decide silently in either direction.
- Blocked-by-hand slugs, added from the endpoint rows.
- Default price ceiling, in $/M, prompt and completion, either or both.
- The compiled result and its age: *"Excluding 12 providers · directory read
  today"*, with **Refresh directory**.

### Starred

A fourth segment beside All / Free / Paid, showing models whose id is on the
startup list for the provider being browsed. The group is no longer only about
price, so `priceFilter` renames to `filter` rather than keeping a name that
lies.

### Providers, in the model card

A disclosure below the price meta, labelled `Providers (32)` once the count is
known, fetched when opened and cached per model id for the session.

That is not a style choice. The card follows the selection, the selection
follows the arrow keys through a 400-row list, and fetching on selection would
fire one request per keystroke.

Each row: provider name, country (HQ, and datacenters when they differ),
quantization, input and output price, context, 30-minute uptime. Rows the policy
excludes are dimmed and labelled with the reason — *blocked: CN*, *over cap*,
*blocked by hand* — because seeing which hosts a rule removes is most of the
value of having the rule. Clicking an allowed row pins it; clicking the pinned
row unpins. A per-model price ceiling sits under the list, showing the default
it inherits.

The pin shows without opening the section, one line under the startup-list
button: *"Pinned to Novita — no fallback"*.

One caveat is stated in the section rather than left to be discovered: a pin
names a provider, not a quantization. `wafer` and `wafer/fast` are two rows and
one slug, and pinning either can land on the other.

### What actually ran

Two places, because the two session types leave different traces.

- **In aiterm chat**, each reply gets a dim suffix line naming its host and what
  it cost — `· via Novita · $0.0021` — the name read from the `provider` field
  on the first chunk, the cost from the `usage` chunk that
  `stream_options: {"include_usage": true}` adds. Both are optional in the
  parser: an absent field means an absent half of the line, never an error and
  never a guess. `sse_delta` returns text today, so it grows a shape that can
  carry these two facts alongside the text instead of a second scan of the
  stream.
- **In Model access**, a *Routing activity* view over `/api/v1/activity`:
  the last 30 days grouped by provider, with requests, tokens and dollars, and
  a second grouping by model. Rows for providers the current policy would now
  block are flagged — *"Baidu · 12 requests · $0.31 · now blocked"* — which is
  the report that answers "have I been using anyone I did not want to".

Activity is per account, not per app, so it includes traffic from outside
aiterm. The view says so rather than implying aiterm caused all of it.

## Failure and testing

A failed endpoints, directory or activity fetch shows its sentence in the same
inline notice the model browser already uses. None of them disable anything: a
stored policy keeps being sent when the directory is unreachable, and a model
with no reachable endpoint list is still startable, just unpinnable.

Unit tests, beside the code they cover:

- a `providers.json` with neither `policy` nor `routes` loads, and a saved one
  round-trips;
- an endpoints reply parses, including the `tag` → `slug` split and string
  prices;
- policy resolution: HQ match, datacenter match, unknown-country toggle in both
  positions, hand-blocked slugs, and a directory entry for a provider that
  serves nothing;
- `routing_block` in each combination — nothing set returns `None`, ignore
  only, pin adds `allow_fallbacks: false`, route ceiling beats policy ceiling;
- `chat_body` with and without a block, and that no `provider` key appears
  without one;
- chunk parsing: a chunk carrying `provider` yields the host, a chunk without it
  yields nothing rather than an error, and the final `usage` chunk yields the
  cost — the same table `sse_delta` is already tested with;
- the OpenCode env value is valid JSON with the model id as a key, including an
  id containing characters that would break a formatted string;
- activity rows group by provider and sum dollars.

One manual check closes it, because every test above asserts what aiterm sends
rather than what OpenRouter does: start GLM 5.2 pinned to Novita in both
consumers, confirm the chat line says Novita, and confirm the next day's
activity row agrees.

An OpenCode session gets no such line — OpenCode renders its own screen and
aiterm does not read its stream. The activity view is the whole answer there,
and the panel says so rather than leaving the asymmetry to be discovered.

## What this does not fix

- The model card still shows the catalog price, which is the cheapest endpoint,
  not the routed one. Once a pin exists, the honest number is the pinned
  endpoint's price. Follow-up, not this spec.
- OpenRouter's own account-wide ignore list merges server-side and is invisible
  to the API. If a provider is blocked there, aiterm cannot show it, and the
  activity view is the only place its absence would be noticed.
- A stored `resolved_ignore` does not know about providers added since it was
  compiled. The panel shows its age; nothing refreshes it automatically.

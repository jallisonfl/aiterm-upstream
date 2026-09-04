# OpenRouter Provider Routing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user constrain which OpenRouter hosts serve a model — by country, by price, or by pinning one — and show which host actually served each reply.

**Architecture:** A stored policy on the OpenRouter provider compiles to an explicit `ignore` list at save time; a per-model route adds a pin and a price ceiling. One Rust function, `routing_block`, turns provider + model into the JSON OpenRouter expects, and both consumers call it: aiterm chat merges it into the request body, and an OpenCode launch hands it over in `OPENCODE_CONFIG_CONTENT`, which OpenCode merges onto the wire without aiterm writing the user's config file.

**Tech Stack:** Rust (Tauri 2 commands, `serde_json`, `curl` subprocess — the project pulls in no TLS stack), React 19 + TypeScript, `cargo test` and `node --test`.

**Spec:** `docs/design/2026-08-10-openrouter-provider-routing-design.md`. Read it before Task 1; it records six facts that were verified by experiment and that this plan depends on.

## Global Constraints

- **No new dependencies.** HTTP is `curl` as a subprocess, exactly as `providers.rs` and `usage.rs` already do it.
- **The API key never crosses IPC.** No command returns it. It reaches curl on stdin via `curl_auth_config`, never on argv — `/proc/<pid>/cmdline` is world-readable.
- **Never write `~/.config/opencode/opencode.json`.** Routing reaches OpenCode through the `OPENCODE_CONFIG_CONTENT` environment variable only. That file is the user's.
- **Never use `extraBody`.** Verified 2026-08-10: OpenCode passes it through literally, one level too deep, and OpenRouter ignores it in silence. The routing object goes directly under a model's `options`.
- **`max_price` is dollars per *million* tokens.** Everything else in this codebase quotes prices per token. Convert at the boundary, never in the middle.
- **Back-compat:** a `providers.json` written by 0.10.40 has no `policy` and no `routes` and must load unchanged. Every new field is `#[serde(default)]`.
- **Commit style:** sentence-case, descriptive, no `feat:`/`fix:` prefixes. Match `git log` — e.g. "Refuse to add or remove a hook when the hooks block is not an object".
- **Rust tests** live inline in the module they cover, in a `#[cfg(test)] mod tests`, with sentence-shaped names. Run with `cd src-tauri && cargo test`.
- **Frontend tests** are `node --test` over pure modules (`npm run test:ui`). There is no React test harness — logic that needs a test belongs in Rust or in a pure `.ts` module, not in a component.
- **Dogfooding hazard:** the user runs Claude inside aiterm. A `cargo build`/`npm run tauri dev` cycle does not disturb the installed release build, but do not restart his running instance to test. Verification uses a dev build.

---

### Task 1: The stored shape

**Files:**
- Modify: `src-tauri/src/providers.rs:41-55` (`Provider`), `:74-85` (`ProviderView`), `:110-119` (`view`)
- Test: same file, `mod tests`

**Interfaces:**
- Produces: `MaxPrice { prompt: Option<f64>, completion: Option<f64> }`, `Policy { blocked_countries: Vec<String>, block_unknown_country: bool, blocked_providers: Vec<String>, max_price: MaxPrice, resolved_ignore: BTreeMap<String,String>, resolved_at: u64 }`, `Route { order: Vec<String>, allow_fallbacks: bool, max_price: MaxPrice }`, and the fields `Provider::policy`, `Provider::routes: BTreeMap<String, Route>`, both mirrored on `ProviderView`.
- `resolved_ignore` maps a provider slug to the reason it is excluded ("CN", "no country", "blocked by hand"). The keys are what goes on the wire; the values are what the UI shows.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn a_providers_file_without_policy_or_routes_still_loads() {
    let old = r#"[{"id":"openrouter","name":"OpenRouter",
        "base_url":"https://openrouter.ai/api/v1","api_key":"k",
        "startup_models":["z-ai/glm-5.2"]}]"#;
    let list: Vec<Provider> = serde_json::from_str(old).expect("0.10.40 file must load");
    assert_eq!(list[0].startup_models, vec!["z-ai/glm-5.2"]);
    assert!(list[0].routes.is_empty());
    assert!(list[0].policy.blocked_countries.is_empty());
    assert!(!list[0].policy.block_unknown_country);
}

#[test]
fn a_policy_and_a_route_round_trip_through_json() {
    let mut p = provider("openrouter");
    p.policy.blocked_countries = vec!["CN".into()];
    p.policy.block_unknown_country = true;
    p.policy.max_price.completion = Some(2.5);
    p.policy.resolved_ignore.insert("baidu".into(), "CN".into());
    p.policy.resolved_at = 1786000000;
    p.routes.insert(
        "z-ai/glm-5.2".into(),
        Route { order: vec!["novita".into()], allow_fallbacks: false,
                max_price: MaxPrice { prompt: None, completion: Some(1.8) } },
    );
    let text = serde_json::to_string(&[p.clone()]).unwrap();
    let back: Vec<Provider> = serde_json::from_str(&text).unwrap();
    assert_eq!(back[0], p);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test a_providers_file_without_policy`
Expected: compile error — no field `routes` on `Provider`.

- [ ] **Step 3: Add the types**

```rust
/// A ceiling in USD per *million* tokens — OpenRouter's unit for `max_price`,
/// which is not the per-token unit `/models` quotes. Convert at the boundary.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct MaxPrice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<f64>,
}

impl MaxPrice {
    pub fn is_empty(&self) -> bool {
        self.prompt.is_none() && self.completion.is_none()
    }
}

/// Which hosts this account will not use, and the most it will pay.
///
/// `resolved_ignore` is the policy *compiled* against the provider directory:
/// slug → the reason it is out. It is stored rather than computed per request
/// because both places that build a request — a CLI process in a pty, and a
/// launch one keystroke from a running terminal — are the wrong places for a
/// network call. See the spec section "Why the ignore list is stored".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Policy {
    #[serde(default)]
    pub blocked_countries: Vec<String>,
    #[serde(default)]
    pub block_unknown_country: bool,
    #[serde(default)]
    pub blocked_providers: Vec<String>,
    #[serde(default)]
    pub max_price: MaxPrice,
    #[serde(default)]
    pub resolved_ignore: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub resolved_at: u64,
}

/// What one model prefers. An empty `order` is "no pin".
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Route {
    pub order: Vec<String>,
    pub allow_fallbacks: bool,
    #[serde(default)]
    pub max_price: MaxPrice,
}
```

Add to `Provider`, after `startup_models`:

```rust
    /// Routing policy for this provider. `default` so files written before
    /// this field existed load.
    #[serde(default)]
    pub policy: Policy,
    /// Per-model routing, keyed by model id. A route outlives its star, so
    /// re-adding a model to the startup list restores its pin.
    #[serde(default)]
    pub routes: std::collections::BTreeMap<String, Route>,
```

Add the same two fields to `ProviderView` and to `Provider::view()`:

```rust
            policy: self.policy.clone(),
            routes: self.routes.clone(),
```

Every existing construction of `Provider` and `ProviderView` in tests needs
`..Default::default()` or the two new fields; `Provider` does not derive
`Default` (it has no sensible default id), so add the fields explicitly where
the compiler points.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test providers::`
Expected: PASS, including the pre-existing provider tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/providers.rs
git commit -m "Store a routing policy and per-model routes beside the startup list"
```

---

### Task 2: One function decides what goes on the wire

**Files:**
- Modify: `src-tauri/src/providers.rs`
- Test: same file

**Interfaces:**
- Consumes: `Provider`, `Policy`, `Route`, `MaxPrice` from Task 1.
- Produces: `pub fn routing_block(p: &Provider, model: &str) -> Option<serde_json::Value>` — the value of the `provider` key in an OpenRouter request, or `None` when there is nothing to say. Tasks 6 and 7 both call this and nothing else.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn nothing_configured_sends_no_routing_block() {
    let p = provider("openrouter");
    assert_eq!(routing_block(&p, "z-ai/glm-5.2"), None);
}

#[test]
fn a_policy_alone_sends_only_the_ignore_list() {
    let mut p = provider("openrouter");
    p.policy.resolved_ignore.insert("baidu".into(), "CN".into());
    p.policy.resolved_ignore.insert("streamlake".into(), "CN".into());
    assert_eq!(
        routing_block(&p, "z-ai/glm-5.2").unwrap(),
        serde_json::json!({"ignore": ["baidu", "streamlake"]}),
    );
}

#[test]
fn a_pin_means_only_that_host() {
    let mut p = provider("openrouter");
    p.routes.insert("z-ai/glm-5.2".into(), Route {
        order: vec!["novita".into()], allow_fallbacks: false, ..Default::default()
    });
    let b = routing_block(&p, "z-ai/glm-5.2").unwrap();
    assert_eq!(b["order"], serde_json::json!(["novita"]));
    assert_eq!(b["allow_fallbacks"], serde_json::json!(false));
}

#[test]
fn an_unpinned_model_omits_allow_fallbacks_rather_than_sending_true() {
    let mut p = provider("openrouter");
    p.policy.max_price.completion = Some(2.5);
    let b = routing_block(&p, "z-ai/glm-5.2").unwrap();
    assert!(b.get("allow_fallbacks").is_none());
    assert!(b.get("order").is_none());
}

#[test]
fn a_models_ceiling_replaces_the_policy_ceiling_rather_than_merging() {
    let mut p = provider("openrouter");
    p.policy.max_price = MaxPrice { prompt: Some(1.0), completion: Some(2.5) };
    p.routes.insert("z-ai/glm-5.2".into(), Route {
        max_price: MaxPrice { prompt: None, completion: Some(1.8) },
        ..Default::default()
    });
    let b = routing_block(&p, "z-ai/glm-5.2").unwrap();
    assert_eq!(b["max_price"], serde_json::json!({"completion": 1.8}));
    // The other model still gets the account default.
    let b2 = routing_block(&p, "z-ai/glm-5.1").unwrap();
    assert_eq!(b2["max_price"], serde_json::json!({"prompt": 1.0, "completion": 2.5}));
}

#[test]
fn a_route_for_another_model_does_not_leak_into_this_one() {
    let mut p = provider("openrouter");
    p.routes.insert("z-ai/glm-5.1".into(), Route {
        order: vec!["novita".into()], allow_fallbacks: false, ..Default::default()
    });
    assert_eq!(routing_block(&p, "z-ai/glm-5.2"), None);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test routing_block`
Expected: FAIL — `routing_block` not found.

- [ ] **Step 3: Write it**

```rust
/// The `provider` object for one request: the account policy, plus whatever
/// this model asks for on top.
///
/// `None` when there is nothing to send, so an unrouted model's request is
/// byte-for-byte what it was before this feature existed.
///
/// A pin sets `allow_fallbacks: false` — "only that host", per the decision
/// recorded in the spec. A pinned host that is down, or priced above the
/// ceiling, fails the request rather than routing elsewhere. That is the
/// point; the caller is responsible for saying so when it reports the error.
pub fn routing_block(p: &Provider, model: &str) -> Option<serde_json::Value> {
    let route = p.routes.get(model);
    let cap = match route {
        Some(r) if !r.max_price.is_empty() => &r.max_price,
        _ => &p.policy.max_price,
    };
    let pinned = route.map(|r| !r.order.is_empty()).unwrap_or(false);
    if p.policy.resolved_ignore.is_empty() && !pinned && cap.is_empty() {
        return None;
    }
    let mut b = serde_json::Map::new();
    if !p.policy.resolved_ignore.is_empty() {
        let slugs: Vec<&String> = p.policy.resolved_ignore.keys().collect();
        b.insert("ignore".into(), serde_json::json!(slugs));
    }
    if let Some(r) = route.filter(|_| pinned) {
        b.insert("order".into(), serde_json::json!(r.order));
        b.insert("allow_fallbacks".into(), serde_json::json!(false));
    }
    if !cap.is_empty() {
        b.insert("max_price".into(), serde_json::to_value(cap).ok()?);
    }
    Some(serde_json::Value::Object(b))
}
```

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test routing_block`
Expected: PASS, six tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/providers.rs
git commit -m "One function builds the routing block both launchers send"
```

---

### Task 3: Read a model's endpoints

**Files:**
- Modify: `src-tauri/src/providers.rs:346-398` (`fetch_models_response` splits), `src-tauri/src/lib.rs:75` (command registration)
- Test: `src-tauri/src/providers.rs`

**Interfaces:**
- Consumes: `Policy`/`Route` from Task 1.
- Produces: `pub fn fetch(p: &Provider, url: &str) -> Result<String, String>` (body, newline, HTTP status — the existing contract); `EndpointCard { provider_name, slug, tag, quantization, context_length, prompt_price, completion_price, max_completion_tokens, uptime_30m, excluded: Option<String> }`; command `provider_model_endpoints(id: String, model: String) -> Result<Vec<EndpointCard>, String>`; `pub fn parse_endpoints(response: &str, p: &Provider, model: &str) -> Result<Vec<EndpointCard>, String>`.
- `excluded` is the reason this row is out under the stored policy, or `None`. Computing it here rather than in TypeScript keeps one implementation of the rule.

- [ ] **Step 1: Write the failing test**

```rust
/// Trimmed from a real `/models/z-ai/glm-5.2/endpoints` reply, 2026-08-10.
const ENDPOINTS: &str = r#"{"data":{"id":"z-ai/glm-5.2","endpoints":[
  {"name":"Sail Research | z-ai/glm-5.2","provider_name":"Sail Research",
   "tag":"sail-research/fp8","quantization":"fp8","context_length":1048576,
   "max_completion_tokens":131072,"uptime_last_30m":99.34,
   "pricing":{"prompt":"0.0000005","completion":"0.00000315"}},
  {"name":"Novita | z-ai/glm-5.2","provider_name":"Novita",
   "tag":"novita/fp8","quantization":"fp8","context_length":1048576,
   "max_completion_tokens":131072,"uptime_last_30m":98.71,
   "pricing":{"prompt":"0.0000005026","completion":"0.00000158"}},
  {"name":"Baidu | z-ai/glm-5.2","provider_name":"Baidu",
   "tag":"baidu/fp8","quantization":"fp8","context_length":1048576,
   "max_completion_tokens":131072,"uptime_last_30m":97.0,
   "pricing":{"prompt":"0.000000504","completion":"0.000001584"}}]}}
200"#;

#[test]
fn an_endpoint_reply_becomes_rows_with_routing_slugs() {
    let p = provider("openrouter");
    let rows = parse_endpoints(ENDPOINTS, &p, "z-ai/glm-5.2").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].provider_name, "Novita");
    // The tag is shown; the slug before the slash is what a pin sends.
    assert_eq!(rows[1].tag, "novita/fp8");
    assert_eq!(rows[1].slug, "novita");
    assert_eq!(rows[1].completion_price, Some(0.00000158));
    assert_eq!(rows[1].max_completion_tokens, Some(131072));
    assert!(rows.iter().all(|r| r.excluded.is_none()));
}

#[test]
fn rows_carry_the_reason_the_policy_excludes_them() {
    let mut p = provider("openrouter");
    p.policy.resolved_ignore.insert("baidu".into(), "CN".into());
    // $2/M completion: Sail Research at $3.15/M is over, Novita at $1.58/M is not.
    p.policy.max_price.completion = Some(2.0);
    let rows = parse_endpoints(ENDPOINTS, &p, "z-ai/glm-5.2").unwrap();
    assert_eq!(rows[0].excluded.as_deref(), Some("over cap"));
    assert_eq!(rows[1].excluded, None);
    assert_eq!(rows[2].excluded.as_deref(), Some("CN"));
}

#[test]
fn a_models_own_ceiling_decides_which_rows_are_over_cap() {
    let mut p = provider("openrouter");
    p.policy.max_price.completion = Some(5.0);
    p.routes.insert("z-ai/glm-5.2".into(), Route {
        max_price: MaxPrice { prompt: None, completion: Some(1.6) },
        ..Default::default()
    });
    let rows = parse_endpoints(ENDPOINTS, &p, "z-ai/glm-5.2").unwrap();
    assert_eq!(rows[0].excluded.as_deref(), Some("over cap"));  // $3.15/M
    assert_eq!(rows[1].excluded, None);                         // $1.58/M
    assert_eq!(rows[2].excluded, None);                         // $1.584/M... under 1.6
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test endpoint`
Expected: FAIL — `parse_endpoints` not found.

- [ ] **Step 3: Split the fetch, then parse**

First pull the curl call out of `fetch_models_response` without changing its
behaviour. The new `fetch` takes the provider and the full URL; the old
function becomes two lines. The comment block about stdin and
`/proc/<pid>/cmdline` moves with the code — it explains the mechanism, not the
caller.

```rust
/// One authenticated GET as curl sees it: body, newline, HTTP status.
pub fn fetch(p: &Provider, url: &str) -> Result<String, String> {
    // …the existing body of fetch_models_response from the Command::new("curl")
    // line down, with `&url` replaced by `url`…
}

fn fetch_models_response(id: &str) -> Result<String, String> {
    let list = load();
    let p = list.iter().find(|p| p.id == id).ok_or("No such provider.")?;
    if p.api_key.is_empty() {
        return Err("No API key saved for this provider.".into());
    }
    fetch(p, &format!("{}/models", p.base_url))
}
```

Then the endpoint reader:

```rust
/// One host's offer of one model.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct EndpointCard {
    pub provider_name: String,
    /// The routing slug — `novita`. What `order` and `ignore` take.
    pub slug: String,
    /// The full tag — `novita/fp8`. Shown, never sent: OpenRouter filters
    /// quantization through a separate field, so a pin cannot name one.
    pub tag: String,
    pub quantization: Option<String>,
    pub context_length: Option<u64>,
    /// USD per token, as quoted — the UI scales to $/M.
    pub prompt_price: Option<f64>,
    pub completion_price: Option<f64>,
    pub max_completion_tokens: Option<u64>,
    pub uptime_30m: Option<f64>,
    /// Why the stored policy rules this row out, or `None`.
    pub excluded: Option<String>,
}

/// The `/endpoints` reply as rows, annotated against the stored policy.
///
/// The annotation happens here, in the one place that already knows the
/// policy, rather than in the panel. Two implementations of "is this row
/// allowed" would be two answers the day one of them is edited.
pub fn parse_endpoints(
    response: &str,
    p: &Provider,
    model: &str,
) -> Result<Vec<EndpointCard>, String> {
    let body = checked_body(response)?;
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| "The provider did not return JSON.".to_string())?;
    let items = v
        .pointer("/data/endpoints")
        .and_then(|e| e.as_array())
        .ok_or_else(|| "No endpoint list in the reply.".to_string())?;

    let route = p.routes.get(model);
    let cap = match route {
        Some(r) if !r.max_price.is_empty() => &r.max_price,
        _ => &p.policy.max_price,
    };
    let price = |m: &serde_json::Value, key: &str| -> Option<f64> {
        let x = m.pointer(&format!("/pricing/{key}"))?;
        x.as_f64().or_else(|| x.as_str()?.trim().parse().ok())
    };

    Ok(items
        .iter()
        .filter_map(|e| {
            let tag = e.get("tag").and_then(|t| t.as_str()).unwrap_or("").to_string();
            let slug = tag.split('/').next().unwrap_or("").to_string();
            let prompt_price = price(e, "prompt");
            let completion_price = price(e, "completion");
            // Per token here, per million in the ceiling.
            let over = |p_tok: Option<f64>, cap_m: Option<f64>| {
                matches!((p_tok, cap_m), (Some(t), Some(c)) if t * 1e6 > c)
            };
            let excluded = p
                .policy
                .resolved_ignore
                .get(&slug)
                .cloned()
                .or_else(|| {
                    (over(prompt_price, cap.prompt) || over(completion_price, cap.completion))
                        .then(|| "over cap".to_string())
                });
            Some(EndpointCard {
                provider_name: e.get("provider_name").and_then(|n| n.as_str())?.to_string(),
                slug,
                tag,
                quantization: e.get("quantization").and_then(|q| q.as_str()).map(String::from),
                context_length: e.get("context_length").and_then(|c| c.as_u64()),
                prompt_price,
                completion_price,
                max_completion_tokens: e.get("max_completion_tokens").and_then(|c| c.as_u64()),
                uptime_30m: e.get("uptime_last_30m").and_then(|u| u.as_f64()),
                excluded,
            })
        })
        .collect())
}

/// The endpoint list for one model. OpenRouter only — the path is theirs, and
/// a `/endpoints` call to a bare llama.cpp is a 404 nobody can act on.
#[tauri::command(async)]
pub fn provider_model_endpoints(id: String, model: String) -> Result<Vec<EndpointCard>, String> {
    let list = load();
    let p = list.iter().find(|p| p.id == id).ok_or("No such provider.")?;
    if !p.is_openrouter() {
        return Err("Provider routing is an OpenRouter feature.".into());
    }
    if p.api_key.is_empty() {
        return Err("No API key saved for this provider.".into());
    }
    let url = format!("{}/models/{model}/endpoints", p.base_url);
    parse_endpoints(&fetch(p, &url)?, p, &model)
}
```

Register it in `src-tauri/src/lib.rs` beside `providers::provider_model_cards`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test providers::`
Expected: PASS. The pre-existing `/models` tests must still pass — the fetch
split is a refactor and changes no behaviour.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/providers.rs src-tauri/src/lib.rs
git commit -m "Read a model's endpoints, annotated with why the policy excludes each host"
```

---

### Task 4: Compile the policy

**Files:**
- Modify: `src-tauri/src/providers.rs`, `src-tauri/src/lib.rs`
- Test: `src-tauri/src/providers.rs`

**Interfaces:**
- Consumes: `fetch` (Task 3), `Policy` (Task 1).
- Produces: `DirectoryEntry { slug, name, headquarters: Option<String>, datacenters: Vec<String> }`; `pub fn parse_directory(response: &str) -> Result<Vec<DirectoryEntry>, String>`; `pub fn resolve_ignore(policy: &Policy, dir: &[DirectoryEntry]) -> BTreeMap<String,String>`; commands `provider_directory(id) -> Vec<DirectoryEntry>`, `provider_policy_set(id, policy) -> Vec<ProviderView>` (compiles before saving), `provider_route_set(id, model, route) -> Vec<ProviderView>`.

- [ ] **Step 1: Write the failing test**

```rust
fn directory() -> Vec<DirectoryEntry> {
    vec![
        DirectoryEntry { slug: "novita".into(), name: "Novita".into(),
            headquarters: Some("US".into()), datacenters: vec![] },
        DirectoryEntry { slug: "baidu".into(), name: "Baidu".into(),
            headquarters: Some("CN".into()), datacenters: vec![] },
        // Headquartered Singapore, serving from China — the case that makes
        // reading only `headquarters` wrong.
        DirectoryEntry { slug: "alibaba".into(), name: "Alibaba".into(),
            headquarters: Some("SG".into()), datacenters: vec!["SG".into(), "CN".into()] },
        DirectoryEntry { slug: "mystery".into(), name: "Mystery".into(),
            headquarters: None, datacenters: vec![] },
    ]
}

#[test]
fn a_country_block_catches_headquarters_and_datacenters() {
    let policy = Policy { blocked_countries: vec!["CN".into()], ..Default::default() };
    let out = resolve_ignore(&policy, &directory());
    assert_eq!(out.get("baidu").map(String::as_str), Some("CN"));
    assert_eq!(out.get("alibaba").map(String::as_str), Some("CN"));
    assert!(!out.contains_key("novita"));
    assert!(!out.contains_key("mystery"), "unknown is not blocked unless asked");
}

#[test]
fn blocking_unknown_countries_is_a_separate_decision() {
    let policy = Policy {
        blocked_countries: vec!["CN".into()], block_unknown_country: true,
        ..Default::default()
    };
    let out = resolve_ignore(&policy, &directory());
    assert_eq!(out.get("mystery").map(String::as_str), Some("no country"));
    assert!(!out.contains_key("novita"));
}

#[test]
fn a_hand_block_needs_no_country_data_and_wins_the_reason() {
    let policy = Policy {
        blocked_providers: vec!["novita".into(), "baidu".into()],
        blocked_countries: vec!["CN".into()],
        ..Default::default()
    };
    let out = resolve_ignore(&policy, &directory());
    assert_eq!(out.get("novita").map(String::as_str), Some("blocked by hand"));
    assert_eq!(out.get("baidu").map(String::as_str), Some("blocked by hand"));
}

#[test]
fn a_directory_reply_parses_both_country_fields() {
    let body = r#"{"data":[
      {"slug":"novita","name":"Novita","headquarters":"US","datacenters":null},
      {"slug":"alibaba","name":"Alibaba","headquarters":"SG","datacenters":["SG","CN"]}]}
200"#;
    let dir = parse_directory(body).unwrap();
    assert_eq!(dir[0].datacenters, Vec::<String>::new());
    assert_eq!(dir[1].datacenters, vec!["SG", "CN"]);
    assert_eq!(dir[1].headquarters.as_deref(), Some("SG"));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test resolve_ignore`
Expected: FAIL — `resolve_ignore` not found.

- [ ] **Step 3: Write it**

```rust
/// One provider in OpenRouter's directory. Country is optional and often
/// missing — 29 of 101 providers reported none on 2026-08-10 — which is why
/// `block_unknown_country` exists as its own decision.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct DirectoryEntry {
    pub slug: String,
    pub name: String,
    pub headquarters: Option<String>,
    #[serde(default)]
    pub datacenters: Vec<String>,
}

pub fn parse_directory(response: &str) -> Result<Vec<DirectoryEntry>, String> {
    let body = checked_body(response)?;
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| "The provider did not return JSON.".to_string())?;
    let items = v.get("data").and_then(|d| d.as_array())
        .ok_or_else(|| "No provider list in the reply.".to_string())?;
    Ok(items
        .iter()
        .filter_map(|e| {
            Some(DirectoryEntry {
                slug: e.get("slug")?.as_str()?.to_string(),
                name: e.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                headquarters: e.get("headquarters").and_then(|h| h.as_str()).map(String::from),
                datacenters: e
                    .get("datacenters")
                    .and_then(|d| d.as_array())
                    .map(|a| a.iter().filter_map(|c| c.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// The policy compiled to slugs: what actually goes in `ignore`, and why.
///
/// A hand block wins the reason line, because it is the one the user typed and
/// the one they will look for when they wonder where a host went.
pub fn resolve_ignore(
    policy: &Policy,
    dir: &[DirectoryEntry],
) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for e in dir {
        let countries: Vec<&str> = e
            .headquarters
            .iter()
            .map(String::as_str)
            .chain(e.datacenters.iter().map(String::as_str))
            .collect();
        if countries.is_empty() {
            if policy.block_unknown_country {
                out.insert(e.slug.clone(), "no country".to_string());
            }
        } else if let Some(c) = countries
            .iter()
            .find(|c| policy.blocked_countries.iter().any(|b| b.eq_ignore_ascii_case(c)))
        {
            out.insert(e.slug.clone(), c.to_string());
        }
    }
    // Hand blocks last: they overwrite a country reason, and they apply to
    // slugs the directory has never heard of.
    for slug in &policy.blocked_providers {
        out.insert(slug.clone(), "blocked by hand".to_string());
    }
    out
}
```

The three commands. `provider_policy_set` compiles before it saves — that is
the whole reason the panel has a Save button rather than writing on each
keystroke:

```rust
#[tauri::command(async)]
pub fn provider_directory(id: String) -> Result<Vec<DirectoryEntry>, String> {
    let list = load();
    let p = list.iter().find(|p| p.id == id).ok_or("No such provider.")?;
    if !p.is_openrouter() {
        return Err("Provider routing is an OpenRouter feature.".into());
    }
    parse_directory(&fetch(p, &format!("{}/providers", p.base_url))?)
}

/// Save a policy, compiling it against the live directory first.
///
/// A directory that will not load is a hard error rather than a silent save:
/// storing a policy whose `resolved_ignore` is empty would read in the panel
/// as "nothing is blocked", which is the opposite of what was asked for.
#[tauri::command(async)]
pub fn provider_policy_set(id: String, policy: Policy) -> Result<Vec<ProviderView>, String> {
    let dir = provider_directory(id.clone())?;
    crate::run_blocking(move || {
        let mut list = load();
        let p = list.iter_mut().find(|p| p.id == id).ok_or("No such provider.")?;
        let mut policy = policy;
        policy.resolved_ignore = resolve_ignore(&policy, &dir);
        policy.resolved_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        p.policy = policy;
        save(&list)?;
        Ok(list.iter().map(Provider::view).collect())
    })
    .await
}

/// Set or clear one model's route. An empty `order` with no ceiling removes
/// the entry rather than storing a route that says nothing.
#[tauri::command(async)]
pub fn provider_route_set(
    id: String,
    model: String,
    route: Route,
) -> Result<Vec<ProviderView>, String> {
    crate::run_blocking(move || {
        let mut list = load();
        let p = list.iter_mut().find(|p| p.id == id).ok_or("No such provider.")?;
        if route.order.is_empty() && route.max_price.is_empty() {
            p.routes.remove(&model);
        } else {
            p.routes.insert(model, route);
        }
        save(&list)?;
        Ok(list.iter().map(Provider::view).collect())
    })
    .await
}
```

Match the surrounding async style: `provider_startup_set` is
`#[tauri::command] pub async fn` wrapping `crate::run_blocking`. Follow that
shape rather than the one sketched above if the compiler disagrees — the
existing file is the authority.

Register all three in `src-tauri/src/lib.rs`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test providers::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/providers.rs src-tauri/src/lib.rs
git commit -m "Compile a country policy into the ignore list the launchers send"
```

---

### Task 5: The activity record

**Files:**
- Modify: `src-tauri/src/providers.rs`, `src-tauri/src/lib.rs`
- Test: `src-tauri/src/providers.rs`

**Interfaces:**
- Produces: `ActivityRow { date, model, provider_name, requests, prompt_tokens, completion_tokens, usage }`; `pub fn parse_activity(response: &str) -> Result<Vec<ActivityRow>, String>`; command `provider_activity(id) -> Vec<ActivityRow>`.
- Grouping is the panel's job (Task 12). This returns rows as OpenRouter gives them.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn an_activity_reply_becomes_rows_with_dollars_and_hosts() {
    let body = r#"{"data":[
      {"date":"2026-08-09","model":"z-ai/glm-5.2","provider_name":"Novita",
       "requests":5,"prompt_tokens":50,"completion_tokens":125,"usage":0.015},
      {"date":"2026-08-09","model":"z-ai/glm-5.2","provider_name":"Baidu",
       "requests":2,"prompt_tokens":20,"completion_tokens":40,"usage":0.004}]}
200"#;
    let rows = parse_activity(body).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].provider_name, "Baidu");
    assert_eq!(rows[0].usage, 0.015);
    assert_eq!(rows[0].requests, 5);
}

#[test]
fn an_activity_row_missing_optional_counts_still_parses() {
    let body = r#"{"data":[{"date":"2026-08-09","model":"m","provider_name":"X"}]}
200"#;
    let rows = parse_activity(body).unwrap();
    assert_eq!(rows[0].requests, 0);
    assert_eq!(rows[0].usage, 0.0);
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test activity`
Expected: FAIL — `parse_activity` not found.

- [ ] **Step 3: Write it**

```rust
/// One day of one model on one host, from OpenRouter's activity record.
///
/// Account-wide, not app-wide: this includes traffic aiterm never launched.
/// The panel says so — see Task 12.
#[derive(serde::Serialize, Clone, Debug, PartialEq)]
pub struct ActivityRow {
    pub date: String,
    pub model: String,
    pub provider_name: String,
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// USD.
    pub usage: f64,
}

pub fn parse_activity(response: &str) -> Result<Vec<ActivityRow>, String> {
    let body = checked_body(response)?;
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|_| "The provider did not return JSON.".to_string())?;
    let items = v.get("data").and_then(|d| d.as_array())
        .ok_or_else(|| "No activity in the reply.".to_string())?;
    Ok(items
        .iter()
        .filter_map(|r| {
            Some(ActivityRow {
                date: r.get("date")?.as_str()?.to_string(),
                model: r.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string(),
                provider_name: r
                    .get("provider_name").and_then(|p| p.as_str()).unwrap_or("").to_string(),
                requests: r.get("requests").and_then(|x| x.as_u64()).unwrap_or(0),
                prompt_tokens: r.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                completion_tokens: r
                    .get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                usage: r.get("usage").and_then(|x| x.as_f64()).unwrap_or(0.0),
            })
        })
        .collect())
}

#[tauri::command(async)]
pub fn provider_activity(id: String) -> Result<Vec<ActivityRow>, String> {
    let list = load();
    let p = list.iter().find(|p| p.id == id).ok_or("No such provider.")?;
    if !p.is_openrouter() {
        return Err("Activity is an OpenRouter feature.".into());
    }
    if p.api_key.is_empty() {
        return Err("No API key saved for this provider.".into());
    }
    parse_activity(&fetch(p, &format!("{}/activity", p.base_url))?)
}
```

Register in `src-tauri/src/lib.rs`.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test activity`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/providers.rs src-tauri/src/lib.rs
git commit -m "Read the account's 30-day activity, which is how OpenCode sessions become visible"
```

---

### Task 6: aiterm chat sends the block and names the host

**Files:**
- Modify: `src-tauri/src/chat.rs:179-186` (`chat_body`), `:195-232` (`sse_delta`), `:441-514` (`stream_reply`), and the send loop in `run`
- Test: `src-tauri/src/chat.rs`

**Interfaces:**
- Consumes: `providers::routing_block` (Task 2).
- Produces: `chat_body(model, messages, routing: Option<&serde_json::Value>) -> String`; `Frame { text: Option<String>, provider: Option<String>, cost: Option<f64> }` returned by the renamed chunk parser.
- `sse_delta` keeps its name and its `Result<Option<String>, String>` error contract for text; the provider and cost ride a second function over the same line so the existing tests keep their shape. Choose one: if a `Frame` return reads better, update the existing `sse_delta` tests in the same commit rather than leaving two parsers.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn the_body_carries_the_routing_block_when_there_is_one() {
    let routing = serde_json::json!({"ignore": ["baidu"], "order": ["novita"],
                                     "allow_fallbacks": false});
    let body = chat_body("z-ai/glm-5.2", &[], Some(&routing));
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["provider"]["order"], serde_json::json!(["novita"]));
    assert_eq!(v["stream"], serde_json::json!(true));
    // Cost per reply, which is why include_usage goes on unconditionally.
    assert_eq!(v["stream_options"]["include_usage"], serde_json::json!(true));
}

#[test]
fn an_unrouted_model_sends_no_provider_key_at_all() {
    let body = chat_body("z-ai/glm-5.2", &[], None);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(v.get("provider").is_none());
}

#[test]
fn a_chunk_names_the_host_that_served_it() {
    // Verified live 2026-08-10: `provider` rides every chunk, and is absent
    // from OpenRouter's own OpenAPI schema. Read it where present, never
    // require it.
    let line = r#"data: {"id":"x","provider":"Novita","choices":[{"delta":{"content":"hi"}}]}"#;
    let f = frame(line).unwrap();
    assert_eq!(f.text.as_deref(), Some("hi"));
    assert_eq!(f.provider.as_deref(), Some("Novita"));
}

#[test]
fn a_chunk_without_a_provider_is_not_an_error() {
    let line = r#"data: {"id":"x","choices":[{"delta":{"content":"hi"}}]}"#;
    let f = frame(line).unwrap();
    assert_eq!(f.text.as_deref(), Some("hi"));
    assert_eq!(f.provider, None);
}

#[test]
fn the_final_usage_chunk_carries_the_cost() {
    let line = r#"data: {"id":"x","provider":"Novita","choices":[],
                  "usage":{"prompt_tokens":14,"completion_tokens":5,"cost":0.0021}}"#;
    let f = frame(line).unwrap();
    assert_eq!(f.text, None);
    assert_eq!(f.cost, Some(0.0021));
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test chat::`
Expected: FAIL — `chat_body` takes two arguments, `frame` not found.

- [ ] **Step 3: Implement**

```rust
/// The request body for one exchange: full history, streaming on, routing when
/// the model has any, and usage accounting so a reply can report its cost.
pub fn chat_body(
    model: &str,
    messages: &[Msg],
    routing: Option<&serde_json::Value>,
) -> String {
    let mut v = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    if let (Some(r), Some(map)) = (routing, v.as_object_mut()) {
        map.insert("provider".into(), r.clone());
    }
    v.to_string()
}

/// What one `data:` line carries. Any field may be absent; absent is absent.
#[derive(Debug, Default, PartialEq)]
pub struct Frame {
    pub text: Option<String>,
    /// The host that served this reply — a top-level `provider` on every
    /// chunk. Undeclared in OpenRouter's OpenAPI document, present on the
    /// wire, so it is read where offered and never required.
    pub provider: Option<String>,
    /// USD for the exchange, on the final chunk when `include_usage` is set.
    pub cost: Option<f64>,
}

pub fn frame(line: &str) -> Result<Frame, String> {
    // …same `data:`/[DONE]/keep-alive/error handling `sse_delta` has today,
    // then, from the parsed value:
    //   text     = v.pointer("/choices/0/delta/content").and_then(as_str)
    //   provider = v.get("provider").and_then(as_str)
    //   cost     = v.pointer("/usage/cost").and_then(as_f64)
}
```

In `stream_reply`, accumulate the first non-empty `provider` and the last
`cost`, and return them alongside the text. In `run`, the send site becomes:

```rust
    let routing = crate::providers::routing_block(p, &model);
    let reply = stream_reply(&url, &p.api_key, &model, &messages, routing.as_ref());
```

`routing` is rebuilt **per send**, inside the loop, because `/model` swaps the
model mid-chat and the route has to swap with it.

After a successful reply, print the attribution line — dim, one line, and only
the parts that arrived:

```rust
    // "· via Novita · $0.0021" — either half may be missing.
    let mut bits = Vec::new();
    if let Some(h) = host { bits.push(format!("via {h}")); }
    if let Some(c) = cost { bits.push(format!("${c:.4}")); }
    if !bits.is_empty() {
        println!("\x1b[2m· {}\x1b[0m", bits.join(" · "));
    }
```

When a request fails and the model is pinned, say which constraint refused, so
"only that host" does not read as a broken session:

```rust
    eprintln!(
        "\x1b[2m  {model} is pinned to {} with no fallback — \
         OpenRouter had nothing to route to.\x1b[0m",
        route_order.join(", ")
    );
```

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test chat::`
Expected: PASS, including the pre-existing transcript and SSE tests.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/chat.rs
git commit -m "Chat sends the routing block and reports which host answered"
```

---

### Task 7: OpenCode gets the same block

**Files:**
- Modify: `src-tauri/src/launch.rs:47-57` (`LaunchPlan`) and its resolver, `src-tauri/src/pty.rs:115-158`, `src/ipc.ts:285-287`, `src/App.tsx:1511-1518`, `src/components/TerminalView.tsx:53,224`
- Test: `src-tauri/src/pty.rs` (or `providers.rs` if the builder lives there)

**Interfaces:**
- Consumes: `providers::routing_block` (Task 2).
- Produces: `pub fn opencode_config_content(p: &Provider, model: &str) -> Option<String>`; `LaunchPlan::env_model: Option<String>`; `pty_spawn(..., env_provider: Option<String>, env_model: Option<String>)`.
- The frontend passes the model id only. The JSON is built in Rust from stored state, so no routing decision crosses IPC.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn opencode_gets_the_routing_block_under_the_models_options() {
    let mut p = provider("openrouter");
    p.policy.resolved_ignore.insert("baidu".into(), "CN".into());
    p.routes.insert("z-ai/glm-5.2".into(), Route {
        order: vec!["novita".into()], allow_fallbacks: false, ..Default::default()
    });
    let text = opencode_config_content(&p, "z-ai/glm-5.2").unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let opts = &v["provider"]["openrouter"]["models"]["z-ai/glm-5.2"]["options"];
    assert_eq!(opts["provider"]["order"], serde_json::json!(["novita"]));
    assert_eq!(opts["provider"]["allow_fallbacks"], serde_json::json!(false));
    // Verified 2026-08-10: OpenCode passes `options` straight onto the body,
    // and does NOT unwrap `extraBody`. An extraBody key here would be silently
    // ignored by OpenRouter.
    assert!(opts.get("extraBody").is_none());
}

#[test]
fn an_unrouted_model_sets_no_environment_variable() {
    let p = provider("openrouter");
    assert_eq!(opencode_config_content(&p, "z-ai/glm-5.2"), None);
}

#[test]
fn a_model_id_with_awkward_characters_stays_valid_json() {
    let mut p = provider("openrouter");
    p.routes.insert("weird/\"quote\"".into(), Route {
        order: vec!["novita".into()], allow_fallbacks: false, ..Default::default()
    });
    let text = opencode_config_content(&p, "weird/\"quote\"").unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).expect("must be valid JSON");
    assert!(v["provider"]["openrouter"]["models"]["weird/\"quote\""].is_object());
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cd src-tauri && cargo test opencode_config_content`
Expected: FAIL — not found.

- [ ] **Step 3: Implement**

```rust
/// The inline OpenCode config that carries this model's routing.
///
/// `OPENCODE_CONFIG_CONTENT` *merges* over the user's own config — verified
/// 2026-08-10 by running `opencode models` with this set and finding their
/// `local/local` provider still listed. aiterm therefore never writes
/// `~/.config/opencode/opencode.json`.
///
/// Built with `serde_json`, never by formatting: the model id is user data
/// that lands inside a JSON key.
pub fn opencode_config_content(p: &Provider, model: &str) -> Option<String> {
    let block = routing_block(p, model)?;
    Some(
        serde_json::json!({
            "provider": {"openrouter": {"models": {model: {"options": {"provider": block}}}}}
        })
        .to_string(),
    )
}
```

In `pty.rs`, beside the existing key injection — same block, same reasoning
about `environ` versus `argv`:

```rust
    if let Some(pid) = env_provider {
        if let Some(p) = crate::providers::load_providers().iter().find(|p| p.id == pid) {
            if p.is_openrouter() && !p.api_key.is_empty() {
                cmd.env("OPENROUTER_API_KEY", &p.api_key);
            }
            if let Some(model) = env_model.as_deref() {
                if let Some(cfg) = crate::providers::opencode_config_content(p, model) {
                    cmd.env("OPENCODE_CONFIG_CONTENT", cfg);
                }
            }
        }
    }
```

Thread `env_model` through: `LaunchPlan` gains the field (set to the model id
for an `apiModel` request, `None` otherwise), `ipc.ts` passes `envModel`,
`App.tsx` puts it on the tab beside `envProvider`, and `TerminalView` hands it
to `ptySpawn`. Follow `envProvider` exactly — it is the same path.

- [ ] **Step 4: Run the tests**

Run: `cd src-tauri && cargo test && npm run build`
Expected: PASS, and TypeScript compiles.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src src/ipc.ts src/App.tsx src/components/TerminalView.tsx
git commit -m "Hand OpenCode the routing block in its environment, never in its config file"
```

---

### Task 8: The IPC surface

**Files:**
- Modify: `src/ipc.ts:344-384`

**Interfaces:**
- Produces the TypeScript mirrors of Tasks 1–5. Snake_case, because these come straight off Rust structs.

- [ ] **Step 1: Write the types and calls**

```ts
export interface MaxPrice { prompt?: number | null; completion?: number | null }

/** Which hosts this account will not use. `resolved_ignore` maps a provider
 *  slug to why it is out — compiled when the policy is saved, sent verbatim as
 *  OpenRouter's `ignore`. */
export interface Policy {
  blocked_countries: string[];
  block_unknown_country: boolean;
  blocked_providers: string[];
  max_price: MaxPrice;
  resolved_ignore: Record<string, string>;
  /** Epoch seconds. 0 means never compiled. */
  resolved_at: number;
}

export interface Route {
  order: string[];
  allow_fallbacks: boolean;
  max_price: MaxPrice;
}

/** One host's offer of one model. `excluded` is why the policy rules it out. */
export interface EndpointCard {
  provider_name: string;
  slug: string;
  tag: string;
  quantization: string | null;
  context_length: number | null;
  prompt_price: number | null;
  completion_price: number | null;
  max_completion_tokens: number | null;
  uptime_30m: number | null;
  excluded: string | null;
}

export interface DirectoryEntry {
  slug: string; name: string;
  headquarters: string | null; datacenters: string[];
}

export interface ActivityRow {
  date: string; model: string; provider_name: string;
  requests: number; prompt_tokens: number; completion_tokens: number;
  usage: number;
}

export const providerModelEndpoints = (id: string, model: string) =>
  invoke<EndpointCard[]>("provider_model_endpoints", { id, model });
export const providerDirectory = (id: string) =>
  invoke<DirectoryEntry[]>("provider_directory", { id });
/** Compiles the policy against the live directory before saving — a directory
 *  that will not load fails the save rather than storing an empty rule. */
export const providerPolicySet = (id: string, policy: Policy) =>
  invoke<ProviderView[]>("provider_policy_set", { id, policy });
export const providerRouteSet = (id: string, model: string, route: Route) =>
  invoke<ProviderView[]>("provider_route_set", { id, model, route });
export const providerActivity = (id: string) =>
  invoke<ActivityRow[]>("provider_activity", { id });
```

Add `policy: Policy` and `routes: Record<string, Route>` to `ProviderView`.

- [ ] **Step 2: Compile**

Run: `npm run build`
Expected: type errors only where `ProviderView` is constructed in tests or
mocks; fix those, then a clean build.

- [ ] **Step 3: Commit**

```bash
git add src/ipc.ts
git commit -m "Mirror the routing commands on the TypeScript side"
```

---

### Task 9: The Starred filter

**Files:**
- Modify: `src/components/ModelAccess.tsx:60` (state), `:106-111` (`shown`), `:235-243` (the segment group)

- [ ] **Step 1: Rename the state and widen the type**

`priceFilter` becomes `filter`, because the group is no longer only about
price and a name that lies is worse than a rename:

```tsx
  const [filter, setFilter] = useState<"all" | "free" | "paid" | "starred">("all");
```

- [ ] **Step 2: Filter on it**

```tsx
  const starred = browsingProv?.startup_models ?? [];
  const shown = (all ?? []).filter(
    (m) =>
      (!q || m.id.toLowerCase().includes(q) || (m.name ?? "").toLowerCase().includes(q)) &&
      (filter === "all" ||
        (filter === "starred" ? starred.includes(m.id) : (filter === "free") === isFree(m))) &&
      (minCtx === 0 || (m.context_length ?? 0) >= minCtx),
  );
```

- [ ] **Step 3: Add the segment**

```tsx
              {(["all", "free", "paid", "starred"] as const).map((f) => (
                <button
                  key={f}
                  className={"seg-btn" + (filter === f ? " on" : "")}
                  onClick={() => setFilter(f)}
                >{f === "all" ? "All" : f === "free" ? "Free"
                  : f === "paid" ? "Paid" : "Starred"}</button>
              ))}
```

- [ ] **Step 4: See it work**

Run: `npm run tauri dev`, open Settings → Model access → OpenRouter → Models,
press **Starred**. Expected: only `z-ai/glm-5.2` and
`~deepseek/deepseek-v4-flash-latest`, and the count line reads "2 of 400
models".

- [ ] **Step 5: Commit**

```bash
git add src/components/ModelAccess.tsx
git commit -m "Filter the model browser down to what is already starred"
```

---

### Task 10: The provider list in the model card

**Files:**
- Modify: `src/components/ModelAccess.tsx` (card body, around `:282-318`), `src/styles.css` (or wherever `.mb-card` lives)

**Interfaces:**
- Consumes: `providerModelEndpoints`, `providerRouteSet`, `EndpointCard`, `Route` (Task 8).

- [ ] **Step 1: State and loader**

Fetch on open, cache per model id. The card follows arrow-key selection through
400 rows; fetching on selection would be one request per keystroke.

```tsx
  const [endpoints, setEndpoints] = useState<Record<string, EndpointCard[]>>({});
  const [epOpen, setEpOpen] = useState<string | null>(null);
  const [epErr, setEpErr] = useState<string | null>(null);

  const openEndpoints = async (modelId: string) => {
    if (epOpen === modelId) { setEpOpen(null); return; }
    setEpOpen(modelId); setEpErr(null);
    if (!endpoints[modelId] && browsingProv) {
      try {
        const rows = await providerModelEndpoints(browsingProv.id, modelId);
        setEndpoints((e) => ({ ...e, [modelId]: rows }));
      } catch (e) { setEpErr(String(e)); }
    }
  };
```

- [ ] **Step 2: The pin action**

```tsx
  const pin = async (modelId: string, slug: string | null) => {
    if (!browsingProv) return;
    const cur = browsingProv.routes[modelId];
    const next: Route = {
      order: slug ? [slug] : [],
      allow_fallbacks: false,      // a pin means only that host
      max_price: cur?.max_price ?? {},
    };
    setProviders(await providerRouteSet(browsingProv.id, modelId, next));
    // The rows carry `excluded`, which the new ceiling may change.
    const rows = await providerModelEndpoints(browsingProv.id, modelId);
    setEndpoints((e) => ({ ...e, [modelId]: rows }));
  };
```

- [ ] **Step 3: Render**

Under the startup-list button, the current pin, so it is visible without
opening the section:

```tsx
  {browsingProv?.routes[sel.id]?.order[0] && (
    <div className="mb-pin">
      Pinned to {browsingProv.routes[sel.id].order[0]} — no fallback
      <button className="act-btn" onClick={() => pin(sel.id, null)}>Unpin</button>
    </div>
  )}
```

Then the disclosure and the rows. An excluded row is dimmed, labelled, and not
clickable — seeing which hosts a rule removes is most of the value of the rule:

```tsx
  <button className="act-btn" onClick={() => openEndpoints(sel.id)}>
    {epOpen === sel.id ? "Hide providers"
      : `Providers${endpoints[sel.id] ? ` (${endpoints[sel.id].length})` : ""}`}
  </button>
  {epOpen === sel.id && (epErr ? <div className="set-notice">{epErr}</div> : (
    <div className="ep-list">
      {(endpoints[sel.id] ?? []).map((e) => (
        <button
          key={e.tag}
          className={"ep-row"
            + (e.excluded ? " off" : "")
            + (browsingProv?.routes[sel.id]?.order[0] === e.slug ? " on" : "")}
          disabled={!!e.excluded}
          title={e.excluded ? `Excluded: ${e.excluded}` : `Pin ${e.provider_name}`}
          onClick={() => pin(sel.id, e.slug)}
        >
          <span className="ep-name">{e.provider_name}</span>
          <span className="ep-tag">{e.quantization ?? ""}</span>
          <span className="ep-price">
            {fmtPrice(e.prompt_price)} / {fmtPrice(e.completion_price)}
          </span>
          <span className="ep-ctx">{fmtCtx(e.context_length)}</span>
          <span className="ep-up">
            {e.uptime_30m == null ? "—" : `${e.uptime_30m.toFixed(1)}%`}
          </span>
          {e.excluded && <span className="ep-off">{e.excluded}</span>}
        </button>
      ))}
      <div className="set-hint">
        A pin names a provider, not a quantization — <code>wafer</code> and
        <code>wafer/fast</code> are two rows and one slug.
      </div>
    </div>
  ))}
```

- [ ] **Step 4: The per-model ceiling**

Two number inputs under the list, in $/M, showing what they inherit:

```tsx
  <label className="ep-cap">
    Max $/M out
    <input
      className="set-input" type="number" step="0.01"
      placeholder={String(browsingProv?.policy.max_price.completion ?? "no cap")}
      value={browsingProv?.routes[sel.id]?.max_price.completion ?? ""}
      onChange={…}   // blur or Enter commits, via providerRouteSet
    />
  </label>
```

- [ ] **Step 5: See it work**

Run: `npm run tauri dev`. Open GLM 5.2, press **Providers**, confirm 32 rows
with Novita at $1.58/M out. Pin Novita. Confirm `~/.config/aiterm/providers.json`
now holds `"routes":{"z-ai/glm-5.2":{"order":["novita"],"allow_fallbacks":false…`.

- [ ] **Step 6: Commit**

```bash
git add src/components/ModelAccess.tsx src/styles.css
git commit -m "Show every host that serves a model, and let one be pinned"
```

---

### Task 11: The policy section

**Files:**
- Modify: `src/components/ModelAccess.tsx` (provider card), `src/styles.css`

**Interfaces:**
- Consumes: `providerDirectory`, `providerPolicySet`, `Policy` (Task 8).

- [ ] **Step 1: Draft state from the stored policy**

The section edits a draft and saves on a button, because saving compiles
against the network and must not fire per keystroke.

- [ ] **Step 2: Render the controls**

- Country toggles, built from the distinct countries in the directory, **CN**
  first because it is the one this was built for.
- One checkbox: *"Also block providers that report no country"*, with the live
  count beside it — `(29 providers)`. It defaults off. 29 of 101 is too large a
  fraction to decide silently in either direction.
- A default ceiling, prompt and completion, in $/M.
- The compiled result and its age:

```tsx
  <div className="set-hint">
    {Object.keys(prov.policy.resolved_ignore).length === 0
      ? "Nothing excluded."
      : `Excluding ${Object.keys(prov.policy.resolved_ignore).length} providers`}
    {prov.policy.resolved_at > 0 && ` · directory read ${ago(prov.policy.resolved_at)}`}
    {stale && " · out of date — new hosts are not covered"}
  </div>
```

`stale` is `resolved_at` older than 30 days. Say it plainly: a stored list does
not learn about providers added since it was compiled, and hiding that would
make the rule look stronger than it is.

- [ ] **Step 3: Save**

```tsx
  const savePolicy = async () => {
    try { setProviders(await providerPolicySet(p.id, draft)); }
    catch (e) { setError(String(e)); }   // a directory that will not load fails the save
  };
```

- [ ] **Step 4: See it work**

Run: `npm run tauri dev`. Block **CN**, save. Expected: the hint reads
"Excluding 6 providers" (or 35 with unknowns blocked), and
`providers.json` gains `resolved_ignore` with `baidu`, `streamlake` and
`alibaba` among the keys — `alibaba` proving the datacenter rule fired, since
its headquarters is SG.

- [ ] **Step 5: Commit**

```bash
git add src/components/ModelAccess.tsx src/styles.css
git commit -m "Set a country and price policy once, compiled into an ignore list"
```

---

### Task 12: What actually ran

**Files:**
- Create: `src/components/RoutingActivity.tsx`
- Modify: `src/components/ModelAccess.tsx` (mount it under the OpenRouter card)

**Interfaces:**
- Consumes: `providerActivity`, `ActivityRow`, `ProviderView.policy` (Task 8).

- [ ] **Step 1: The component**

Group the rows by `provider_name`, summing requests, tokens and dollars; a
second grouping by model underneath. Flag any host the current policy would now
block — that is the report that answers "have I been using anyone I did not
want to":

```tsx
  const blocked = prov.policy.resolved_ignore;   // slug → reason
  // Activity gives display names ("Baidu"); the policy holds slugs ("baidu").
  // Match on a normalised name rather than assuming they are the same string.
  const slugOf = (name: string) => name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
```

Render `Baidu · 12 requests · $0.31 · now blocked` for a flagged row.

- [ ] **Step 2: Say whose numbers these are**

One line under the table: *"OpenRouter's record for this key — including
requests aiterm did not make."* Activity is per account. Implying aiterm caused
all of it would be a lie the panel tells by omission.

- [ ] **Step 3: Say why OpenCode has no live line**

Under the heading: *"OpenCode sessions appear here only — aiterm renders its
screen but never reads its stream."* The asymmetry between the two session
types is otherwise something the user discovers by wondering.

- [ ] **Step 4: See it work**

Run: `npm run tauri dev`, open the view. Expected: the free-model traffic from
2026-08-10 shows a **Darkbloom** row at $0.00.

- [ ] **Step 5: Commit**

```bash
git add src/components/RoutingActivity.tsx src/components/ModelAccess.tsx
git commit -m "Report which hosts actually served this account, and at what cost"
```

---

### Task 13: End-to-end verification

**Files:** none — this task produces evidence, not code.

- [ ] **Step 1: Both consumers, one pin**

With GLM 5.2 pinned to Novita: start it from the API-model dropdown (OpenCode
path), and start a chat console session on it. Both must answer.

- [ ] **Step 2: Prove the chat line**

The chat reply must end with `· via Novita · $0.00…`. If the host reads as
anything else, the pin did not reach the wire — check the body, not the UI.

- [ ] **Step 3: Prove the OpenCode path**

OpenCode renders its own screen and shows no host. Confirm the environment
instead:

```bash
tr '\0' '\n' < /proc/$(pgrep -n -f 'opencode')/environ | grep OPENCODE_CONFIG_CONTENT
```

Expected: the JSON with `"order":["novita"]`. Then confirm
`~/.config/opencode/opencode.json` is byte-identical to before — `git status`
will not help here; compare against a copy taken first.

- [ ] **Step 4: Prove it end to end**

Next day, open Routing activity. Both sessions must appear under **Novita** and
no other host.

- [ ] **Step 5: Prove the refusal**

Set the model's ceiling below Novita's price and send a message. Expected: a
failed request whose message names the pin, not a silent reroute to a pricier
host. This is the designed behaviour of "only that host" — confirm it reads as
a decision rather than a bug.

- [ ] **Step 6: Commit the version bump**

Follow the repo's release habit (`git log` shows bare version-number commits).

---

## Self-review notes

- Spec sections and their tasks: data model → 1; `routing_block` → 2; endpoints
  → 3; directory, resolution, stored-not-computed → 4; activity → 5; chat body,
  attribution, cost → 6; OpenCode env → 7; IPC → 8; Starred → 9; provider list,
  pin, caveat, per-model cap → 10; policy section, staleness → 11; activity view
  and its two disclaimers → 12; manual checks → 13.
- Naming is consistent across tasks: `routing_block`, `resolve_ignore`,
  `resolved_ignore`, `opencode_config_content`, `parse_endpoints`,
  `parse_directory`, `parse_activity`, `EndpointCard.excluded`,
  `LaunchPlan.env_model`.
- Two things this plan deliberately leaves to the implementer's judgment,
  flagged in place: whether `sse_delta` is replaced by `frame` or joined by it
  (Task 6), and the exact async wrapper shape for the new commands (Task 4) —
  the existing file is the authority in both cases.
- Known gap, from the spec's own "What this does not fix": the model card still
  shows the catalog price rather than the pinned host's. Not in this plan.

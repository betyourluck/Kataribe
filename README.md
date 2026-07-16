**English** | [日本語](README_jp.md)

# <img src="images/kataribe.png" alt="Kataribe" width="32"> Kataribe (語り部) — a GM that never forgets or contradicts

A TRPG game master with a cloud LLM as narrator and a **deterministic Rust engine as the source of truth** for game state.

The failure mode that always breaks AI Dungeon–style LLM GMs isn't weak prose — it's **forgetting and contradiction** (what you carry, who died, where you are, what was decided last turn). Kataribe cuts that off structurally: the LLM never holds the state. What it sells is not "infinite freedom" but **consistency**.

A side effect of that architecture: because the engine guarantees correctness, Kataribe runs well on **cheap, free, or fully local models**. The engine backs up a small model's mistakes, so you don't need a frontier model to get a coherent game — only richer prose.

![Kataribe in play](images/kataribe_ui.png)

*A scenario package in play — the GM's narration over a scene background, with live goals and the characters present in the scene on the right. Everything is driven by the deterministic engine underneath.*

## Download

Get the installer for your OS from the [**latest release**](https://github.com/betyourluck/Kataribe/releases/latest).

| OS | File | Status |
|---|---|---|
| **Windows** | `Kataribe_x.y.z_x64-setup.exe` (installer) / `.msi` | ✅ Verified working |
| macOS (Apple Silicon) | `.dmg` | CI build only, unverified |
| Linux | `.deb` / `.AppImage` / `.rpm` | CI build only, unverified |

After launching, go to **Settings → AI Model** and set the `base_url` / `model` / `api_key` for an OpenAI-compatible endpoint (a cloud LLM, or a local OpenAI-compatible server). Play scenario packages by adding a folder or fetching them from the distribution site.

## Design core — separation of powers

> **The LLM proposes, the engine adjudicates, Memoria remembers, the scenario constrains.**

| Branch | Role | Implementation | Status |
|---|---|---|---|
| **Engine (source of truth)** | Deterministically adjudicates every mutable state — HP/stats, inventory, dice, flags, location, skills, attributes | `crates/gm_core` (Rust) | ✅ Done |
| **LLM (proposal)** | Narration, NPC lines, action proposals. Holds no numeric truth (structurally can't) | `crates/llm_client` (Rust) | ✅ Done — 4 providers |
| **Memoria (memory)** | Semantic recall of foreshadowing & character personality (never mutable state) | `crates/harness` (memoria_bridge) | ✅ Done |
| **Scenario (constraint)** | A location graph + gate conditions keep improvisation on the rails | YAML packages | ✅ Done |

**Iron rule:** mutable world state lives in the engine's state machine. It is **never** placed in vector recall — fuzzy recall would recreate the "forgetting GM." Only foreshadowing and personality belong to Memoria.

## What the engine guarantees

The LLM proposes a `StateDelta` (structured output: `narration` + `ops`). The engine's `adjudicate` — a pure function that changes no state — verifies every op; on an illegal op it rejects with a machine-readable reason and the loop regenerates. Only on acceptance does `apply` mutate state, **atomically** (one bad op rejects the whole delta; state stays intact).

Because of that boundary:

- **Numbers are the engine's.** The LLM states the intent ("train hard: +STR, −HP"); the engine does the arithmetic. It cannot fabricate a die roll, an HP value, or an item it doesn't hold — the op structure makes it impossible.
- **Dice are deterministic & auditable** (seeded RNG). Same seed → same rolls.
- **Closed world.** Undeclared stats / items / skills / flags don't exist; the engine rejects any op that touches them. Skills, character classes, and who-is-present change only through authored triggers, never LLM whim.
- **Consequence is authored.** Named goals, campaign transitions (state carries across modules), challenges (dice → tiers → flags), delayed events, and hidden roles + voting (werewolf-style) are all gated by the engine, not the prose.
- **Long memory.** A running chronicle and compressed chapter synopses feed the GM its own past, so it stays consistent across long sessions — the second half of "never forgets."

## Proven with real LLMs, across genres

The same unchanged engine drives fantasy dungeons, dating-sim routes (raising a heroine's affection), mystery, and social deduction (a hidden-werewolf village). The engine is genre-neutral; the LLM supplies the flavor. Verified end-to-end with **Claude, Gemini, and Grok**, and with local OpenAI-compatible servers via a **no-tools JSON mode** (for models that don't support tool calling). Prompt caching (Anthropic `cache_control` / Gemini `cachedContent` / xAI sticky routing) keeps the repeated input cheap.

The signature demonstration: tell the GM you use a "prophecy skill you never had," and it grounds the lie away — *"there was never such a power"* — with zero state change. **The source of truth beats the LLM's fluency.**

## Authoring & distribution

Scenarios ship as self-contained **packages** — a folder with `package.yaml` + characters + scenarios (+ optional campaign, images, audio). Zip it, unzip it, it runs. A companion distribution site (the *Kataribe 書庫*) lets authors share packages and players install them from inside the app. You can even build a package by handing an LLM the format spec and a synopsis; see the authoring guide.

## Build & test

```bash
cargo test --workspace                     # 250+ deterministic PoC tests (Red→Green)
cargo clippy --workspace --all-targets
```

The desktop app (Tauri 2 + Vue 3) lives in `app/`:

```bash
cd app && npm install && npm run tauri dev  # requires WebView2 on Windows
```

## Layout

```text
Kataribe/
├── data_contract.yaml   # ★ Frozen nouns (the GameState / StateDelta / Gate / Scenario contract)
├── crates/
│   ├── gm_core/         # Source of truth: state, scenario spine, adjudicate/apply engine
│   ├── llm_client/      # Narrator leg: 4-provider unified tool layer, schemars-generated schema,
│   │                    #   prompt caching, no-tools JSON fallback for cheap/local models
│   └── harness/         # Turn loop, memoria_bridge, synopsis/chronicle (long-term memory), campaigns
├── app/                 # Tauri 2 + Vue 3 desktop app (save/load, immersion assets, i18n ja/en, 書庫)
├── packages/            # Distributable scenario packages
├── specs/               # Design specs (NN_*.md)
└── CLAUDE.md            # Project ledger (architecture, north star, mandates)
```

## License

[MIT License](LICENSE). The engine and the bundled scenarios can be freely used, modified, and redistributed. **It's worth something only when it's used** — fork it and build your own world.

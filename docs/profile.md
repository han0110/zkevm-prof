# zkEVM guest cost profiling

Measures what a zkVM charges to prove a stateless validator executing a block, and turns a corpus of
those measurements into a comparison across guests.

The cost is the zkVM's own price for the execution rather than wall clock time, so it is reproducible
on any machine and comparable between guests that ran the same blocks.

Work runs in two steps. `profile` measures one guest over a fixture corpus and writes JSON, and
`report` aggregates several of those JSON files into one document.

```sh
cargo build --release
./target/release/zkevm-prof --help
```

## profile

Runs one guest over a corpus of EEST fixtures and records what each block cost.

```sh
zkevm-prof profile \
  --zkvm zisk \
  --stateless-validator nethermind \
  --input fixtures/mainnet-25580000-1000 \
  --output nethermind-zisk.json
```

| Option | Values | Meaning |
| --- | --- | --- |
| `--zkvm` | `openvm`, `zisk` | zkVM the guest is profiled on. |
| `--stateless-validator` | `ethrex`, `nethermind`, `reth`, `zesu` | Guest to profile. |
| `--input` | path | Directory of EEST fixtures. |
| `--output` | path | Where the profile JSON is written. |

The input directory is walked recursively for `.json` files. Only the last block of each fixture is
profiled, since that is the block an EEST blockchain test is written to exercise and the ones before
it only build the state it runs against. A fixture file holding several tests contributes one entry
per test.

The output holds the per-test profile alongside the zkVM that produced it, which is how the report
knows what a cost map's kinds mean.

```json
{
  "profile": {
    "witness-generator-spec-cli::block_25580000_24c8fa4d": {
      "cost": {
        "base": 293601280,
        "main": 25355756224,
        "memory": 3326600265,
        "opcodes": 6117328484,
        "precompiles": 7802641084,
        "total": 42895927337
      },
      "metadata": { "gas_used": 26211834, "block_number": 25580000 }
    }
  },
  "meta": { "zkvm": "zisk" }
}
```

Blocks are profiled in parallel over rayon's global pool, which by default fills every core. Cap it
with `RAYON_NUM_THREADS` on a machine with many cores and little memory, since a profiled execution
holds the whole guest memory.

```sh
RAYON_NUM_THREADS=8 zkevm-prof profile --zkvm zisk ...
```

Progress and failures go to stderr, so stdout stays free for the backends themselves. A block that
fails to profile is reported and skipped rather than discarding the corpus, and the run ends with a
count of what was skipped. The command fails only when every block failed.

## report

Aggregates profiles into one document.

```sh
zkevm-prof report --input ethrex-zisk.json nethermind-zisk.json --output index.html
zkevm-prof report --input ethrex-zisk.json nethermind-zisk.json --output report.md --format md
```

| Option | Values | Meaning |
| --- | --- | --- |
| `--input` | one or more paths | Profile JSON files, one per guest. |
| `--output` | path | Where the report is written. |
| `--format` | `html`, `md` | Shape of the report, `html` by default. |

Each input contributes one series labelled by its file stem, so `nethermind-zisk.json` becomes the
series `nethermind-zisk`. Series are compared over the tests they all profiled, so a guest that
failed on some blocks does not appear cheaper for having skipped them.

`html` renders a single page with a stacked composition bar, a cost against gas chart and a totals
table. `md` renders the same figures as tables, for pasting into a pull request or an issue. Both
documents live in [`templates/`](../templates) and are checked at build time by askama, so changing
what a report says is a template edit. Rust computes geometry and formats numbers; the templates hold
no logic beyond iteration.

Every input has to come from the same zkVM, since costs from different zkVMs are in different units.
Mixing them is an error rather than a chart of incomparable numbers.

## Guests

Ethrex, Reth and Zesu guests are downloaded from the [ere-guests][ere-guests] release the CLI is
pinned to. The stateless input schema is versioned with the guests, so a corpus and a guest have to
come from the same release or the guest rejects the input.

Nethermind has no ere-guests release, so its guest is published from a fork under the
`glamsterdam-devnet-7` tag and fetched from there. Assets follow the ere-guests naming, so a
fork-released guest and a catalog one resolve alike.

Rebuilding it runs the same recipe the Nethermind release workflow does, from a Nethermind checkout.

```sh
cd src/Nethermind/Nethermind.Stateless.ZiskGuest
make build
```

The bflat container writes `libziskos.a` back into the artifacts directory as root, which makes a
second build fail with `MSB3021`. Restore ownership before rebuilding.

```sh
docker run --rm -v "$PWD/../artifacts/bin/Nethermind.Stateless.ZiskGuest/release":/w alpine:3 \
  chown -R "$(id -u):$(id -g)" /w
```

## zkVMs

ZisK prices an execution as a weighted sum over the work the prover has to do, which its emulator
accumulates as it runs. The emulator is driven in process rather than through the `ziskemu` binary,
and the resulting distribution matches `ziskemu -X` exactly.

OpenVM prices an execution from trace geometry instead, so its backend runs metered execution and
records the rows every AIR fills. On x86_64 that runs through the AOT instance, which compiles the
program to native code once and shares it across threads; elsewhere the same call falls back to the
interpreter, which reports the same heights more slowly. Turning heights into one number needs
per-AIR weights calibrated against measured proving time, and until that lands an OpenVM profile
carries rows and no total, which means `report` refuses it.

The two backends cost very differently to run. Over 100 mainnet blocks at eight threads, ZisK spent
about 1.5 s of wall clock per block after negligible setup, while OpenVM spent about 0.18 s per block
after roughly 47 s of proving-key generation and AOT compilation. OpenVM also peaked at 14.5 GB of
memory against ZisK's 1.4 GB, so it needs a large machine rather than a capped thread pool.

Adding a zkVM means implementing `Profiler` in a module under `src/zkvm` and declaring how its cost
map decomposes. The cost is an open map of whatever kinds that zkVM charges for, so the fixture
walking, the output format and the report need no change.

```rust
pub const COMPOSITION: Composition = Composition {
    total: "total",
    components: &["base", "main", "memory", "opcodes", "precompiles"],
};
```

The report reads `meta.zkvm` and picks that backend's composition, so a profile never has to repeat
the shape of its own cost map.

## Continuous integration

[`profile.yml`](../.github/workflows/profile.yml) is a reusable workflow that profiles every guest
over a mainnet corpus in a matrix, aggregates the results and publishes the page to GitHub Pages on
pushes to `main`. It takes the zkVM as an input, so adding a backend to CI means adding one caller.
[`profile-zisk.yml`](../.github/workflows/profile-zisk.yml) is that caller for ZisK.

A pull request profiles 100 blocks and every other run profiles 1000. The full corpus extracts to
15 GB, which is more than a GitHub-hosted runner has free, so the job first runs
[`free-up-disk-space.sh`](../.github/scripts/free-up-disk-space.sh) to drop the preinstalled Android,
Haskell, .NET and CodeQL toolchains.

Only the compressed archive is cached, keyed by its Drive file id, so a rerun extracts it instead of
downloading it again. The two corpora take about 6.3 GB of the 10 GB a repository gets, so adding a
third would start evicting the others.

[`unit-test.yml`](../.github/workflows/unit-test.yml) runs `cargo fmt`, `clippy` and the tests on
every push and pull request. It shares one cargo cache with the profile workflow, since both build
this crate in release.

Each zkVM publishes into its own subdirectory of the Pages branch, so the ZisK page lands at
`/zisk/` and a second zkVM never replaces it.

[ere-guests]: https://github.com/eth-act/ere-guests

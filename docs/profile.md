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
| `--elf` | path | Guest ELF to profile, in place of the downloaded one. |

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

`html` renders a single page with a totals table, a cost against gas chart and, for a zkVM that prices
in kinds, a stacked composition bar. `md` renders the same figures as tables, for pasting into a pull
request or an issue. Both documents live in [`templates/`](../templates) and are checked at build time
by askama, so changing what a report says is a template edit. Rust computes geometry and formats
numbers; the templates hold no logic beyond iteration.

Every input has to come from the same zkVM, since costs from different zkVMs are in different units.
Mixing them is an error rather than a chart of incomparable numbers.

## Guests

Ethrex, Reth and Zesu guests are downloaded from the build artifacts of the [ere-guests][ere-guests]
commit the CLI is pinned to. The stateless input schema is versioned with the guests, so a corpus and
a guest have to come from the same commit or the guest rejects the input. A build names its artifacts
after the zkVM SDK it built them with, and that SDK has to be the one this crate links, since OpenVM
moved its guest target from 32-bit to 64-bit RISC-V between `v2.0.0` and `v2.1.0-preview`.

Not every guest is built for every zkVM. ere-guests compiles Ethrex and Reth for each zkVM it
supports, while Zesu is republished from a Consensys release and only for ZisK, so no Zesu guest
exists on OpenVM. A caller of the profile workflow lists only the guests its zkVM has.

Artifacts are used rather than release assets because no ere-guests release yet carries
`v2.1.0-preview` guests. Moving to another commit means bumping `ERE_GUESTS_COMMIT` and the
`ere-catalog` pin together, since the catalog is what names the SDK version in an artifact. Unlike a
release asset, the GitHub API serves an artifact only to an authenticated caller, so `GITHUB_TOKEN`
has to hold a token; ere-guests is public, so any token is enough. Artifacts also expire on the
repository's retention window, after which the pin has to move to a newer commit or a release.

Handing over a locally built guest with `--elf` bypasses the download.

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

OpenVM prices an execution from trace geometry instead, through the metered cost mode its own
`cargo openvm run --mode meter` uses. Every instruction is charged the rows it adds times the width
of the AIR that proves it, so the total is the trace cells the app VM has to commit to. The mode
carries no memory model and no segmentation, which leaves the number independent of the machine and
of the prover backend. A profile records that total under `cost`.

Running a guest goes through rvr, which translates the program to C and builds it into a shared
library once per guest ELF. That build needs LLVM clang 19 or newer and a matching lld, which
[`install-llvm.sh`](../.github/scripts/install-llvm.sh) installs on a runner. That build dominates a
run and does not grow with the corpus. Building the Reth guest cost 1220 CPU-seconds and peaked at
3.8 GB of memory, most of it in the toolchain rather than in the profiler, while metering a mainnet
block afterwards cost under a second. Only part of the build parallelises, so those 1220
CPU-seconds took 65 s of wall clock on 32 cores.

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
the shape of its own cost map. A zkVM that prices an execution as one number leaves `components`
empty, and the report drops its composition section rather than drawing a bar of one segment.

## Continuous integration

[`profile.yml`](../.github/workflows/profile.yml) is a reusable workflow that profiles a matrix of
guests over a mainnet corpus, aggregates the results and publishes the page to GitHub Pages from
`main`. It takes the zkVM and the guests it has a built ELF for as inputs, so adding a backend to CI
means adding one caller. [`profile-zisk.yml`](../.github/workflows/profile-zisk.yml) and
[`profile-openvm.yml`](../.github/workflows/profile-openvm.yml) are those callers. The OpenVM one
runs on demand rather than on a push, since building a guest through rvr dominates the run.

A pull request profiles 100 blocks and every other run profiles 1000. The full corpus extracts to
15 GB, which is more than a GitHub-hosted runner has free, so the job first runs
[`free-up-disk-space.sh`](../.github/scripts/free-up-disk-space.sh) to drop the preinstalled Android,
Haskell, .NET and CodeQL toolchains.

Only the compressed archive is cached, keyed by its Drive file id, so a rerun extracts it instead of
downloading it again. The two corpora take about 6.3 GB of the 10 GB a repository gets, so adding a
third would start evicting the others. The cargo cache competes for the same budget, and the OpenVM
dependency makes it large enough to matter, so only `main` writes it and a branch restores what
`main` last saved.

Every job that builds this crate first installs the LLVM toolchain OpenVM's rvr backend needs, since
Ubuntu ships a clang older than rvr accepts and no lld at all.

[`unit-test.yml`](../.github/workflows/unit-test.yml) runs `cargo fmt`, `clippy` and the tests on
every push and pull request. It shares one cargo cache with the profile workflow, since both build
this crate in release.

Each zkVM publishes into its own subdirectory of the Pages branch, so the ZisK page lands at
`/zisk/` and a second zkVM never replaces it. Callers share that branch, so the job holding the
publish step takes one global concurrency slot and their pushes cannot race.

[ere-guests]: https://github.com/eth-act/ere-guests

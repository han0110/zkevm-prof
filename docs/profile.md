# zkEVM guest cost profiling

Measures what a zkVM charges to prove a stateless validator executing a block, and turns a corpus of
those measurements into a comparison across guests. The cost is the zkVM's own price for the
execution rather than wall clock time, so it reproduces on any machine and compares across guests
that ran the same blocks.

`profile` measures one guest over a fixture corpus and writes JSON. `report` aggregates several of
those files into one document.

```sh
cargo build --release
./target/release/zkevm-prof --help
```

## profile

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
| `--input` | path | Directory of EEST fixtures, walked recursively for `.json`. |
| `--output` | path | Where the profile JSON is written. |
| `--elf` | path | Guest ELF to profile, in place of the downloaded one. |

Only the last block of each fixture is profiled, since that is the block an EEST blockchain test is
written to exercise and the ones before it only build the state it runs against. A fixture holding
several tests contributes one entry per test.

The output pairs the per-test costs with the zkVM that produced them, which is how the report knows
what a cost map's kinds mean.

```json
{
  "profile": {
    "witness-generator-spec-cli::block_25580000_24c8fa4d": {
      "cost": { "base": 293601280, "main": 25355756224, "total": 42895927337 },
      "metadata": { "gas_used": 26211834, "block_number": 25580000 }
    }
  },
  "meta": { "zkvm": "zisk" }
}
```

Blocks are profiled in parallel over rayon's global pool, which by default fills every core. A
profiled execution holds the whole guest memory, so cap the pool on a machine with many cores and
little memory.

```sh
RAYON_NUM_THREADS=8 zkevm-prof profile --zkvm zisk ...
```

Progress and failures go to stderr, leaving stdout to the backends. A failed block is reported and
skipped, the run prints how many were skipped, and the command fails only when every block failed.

## report

```sh
zkevm-prof report --input ethrex-zisk.json nethermind-zisk.json --output index.html
zkevm-prof report --input ethrex-zisk.json nethermind-zisk.json --output report.md --format md
```

| Option | Values | Meaning |
| --- | --- | --- |
| `--input` | one or more paths | Profile JSON files, one per guest. |
| `--output` | path | Where the report is written. |
| `--format` | `json`, `md` | Shape of the report, `json` by default. |

Each input contributes one series labelled by its file stem, so `nethermind-zisk.json` becomes the
series `nethermind-zisk`. Series are compared over the tests they all profiled, so a guest that
failed on some blocks does not appear cheaper for having skipped them. Every input has to come from
the same zkVM, since costs from different zkVMs are in different units.

`json` carries the series in the units they were profiled in, along with the zkVM version, the block
count and the time and workflow run that produced them. `md` renders the same figures as tables, from
[`templates/report.md`](../templates/report.md), for pasting into a pull request.

The page itself is static and lives in [`site/`](../site). It loads one report per zkVM by name, so
`openvm.json` fills the OpenVM tab and `zisk.json` the ZisK tab, and a tab always shows one run's
batch rather than a mix of two. Numbers are formatted in the page rather than in the report, so the
JSON stays the raw measurement. Opening a tab records it in the URL fragment, which makes a tab
linkable.

## Guests

Ethrex, Reth and Zesu guests are downloaded from [ere-guests][ere-guests] at the version
`stateless-validator-catalog` is pinned to in `Cargo.toml`, which the build script reads so the
download and the linked catalog cannot drift apart. A tag pin resolves to that release's assets, and
a branch or revision pin to the build artifacts of the commit it locks to. The stateless input
schema is versioned with the guests, so a corpus and a guest have to come from the same version or
the guest rejects the input. Artifacts are named after the zkVM SDK that built them, and that SDK has
to be the one this crate links, so moving to another version means bumping the ere-guests and
`ere-catalog` pins together, since the catalog is what names the SDK version.

The GitHub API serves build artifacts only to an authenticated caller, so a branch or revision pin
needs `GITHUB_TOKEN` to hold a token. ere-guests is public, so any token is enough. Artifacts also
expire on the repository's retention window, after which the pin has to move, while release assets
do not. Passing `--elf` bypasses the download entirely.

Not every guest is built for every zkVM. ere-guests compiles Ethrex and Reth for each zkVM it
supports, while Zesu is republished from a Consensys release and only for ZisK, so no Zesu guest
exists on OpenVM. A caller of the profile workflow lists only the guests its zkVM has.

Nethermind is outside the ere-guests catalog. Its guest is published from a fork under the
`glamsterdam-devnet-7` tag, following the ere-guests asset naming so it resolves alike. Rebuilding it
runs the recipe the Nethermind release workflow uses, from a Nethermind checkout.

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

OpenVM prices an execution from trace geometry instead, through the same metered cost mode as
`cargo openvm run --mode meter`. Every instruction is charged the rows it adds times the width of the
AIR that proves it, so the total is the trace cells the app VM commits to. A profile records that
total under `cost`. The number depends on the app VM configuration, which is kept equal to the one
ere proves these guests under.

Running an OpenVM guest goes through rvr, which translates the program to C and builds it into a
shared library once per ELF. That build needs LLVM clang 19 or newer and a matching lld, installed by
[`install-llvm.sh`](../.github/scripts/install-llvm.sh). It dominates a run and does not grow with
the corpus, so metering more blocks is nearly free once it is done.

Adding a zkVM means implementing `Profiler` in a module under `src/zkvm` and declaring how its cost
map decomposes. The cost is an open map of whatever kinds that zkVM charges for, so the fixture
walking, the output format and the report need no change.

```rust
pub const COMPOSITION: Composition = Composition {
    total: "total",
    components: &["base", "main", "memory", "opcodes", "precompiles"],
};
```

The report reads `meta.zkvm` and picks that backend's composition, so a profile never repeats the
shape of its own cost map. A zkVM that prices an execution as one number leaves `components` empty,
and the report drops its composition section rather than drawing a bar of one segment.

## Continuous integration

[`profile.yml`](../.github/workflows/profile.yml) is a reusable workflow that profiles a matrix of
guests over a mainnet corpus, aggregates the results and publishes the page to GitHub Pages from
`main`. It takes the zkVM and the guests it has a built ELF for as inputs, so adding a backend means
adding one caller. [`profile-zisk.yml`](../.github/workflows/profile-zisk.yml) and
[`profile-openvm.yml`](../.github/workflows/profile-openvm.yml) are those callers. Every job that
builds this crate first installs the LLVM toolchain rvr needs, since Ubuntu ships a clang older than
rvr accepts and no lld at all.

A pull request profiles 100 blocks and every other run profiles 1000. The corpus extracts to 15 GB,
more than a GitHub-hosted runner has free, so the job first runs
[`free-up-disk-space.sh`](../.github/scripts/free-up-disk-space.sh) to drop the preinstalled Android,
Haskell, .NET and CodeQL toolchains. Only the compressed archive is cached, keyed by its Drive file
id, so a rerun extracts it instead of downloading it again. The corpora and the cargo cache compete
for the 10 GB a repository gets, so only `main` writes and a branch restores what `main` last saved.

[`unit-test.yml`](../.github/workflows/unit-test.yml) runs `cargo fmt`, `clippy` and the tests on
every push and pull request, sharing the cargo cache with the profile workflow.

Every caller publishes the same static page to the root of the Pages branch alongside its own
`<zkvm>.json`, and keep_files leaves the other zkVMs' reports in place, so a run only ever replaces
its own tab. Callers share that branch, so the job holding the publish step takes one global
concurrency slot and their pushes cannot race.

[ere-guests]: https://github.com/eth-act/ere-guests
